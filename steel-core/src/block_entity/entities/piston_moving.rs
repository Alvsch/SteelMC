//! Vanilla moving-piston block entity.

use std::cell::Cell;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::{BaseNbtCompound as BorrowedNbtCompound, NbtCompound as NbtCompoundView};
use simdnbt::owned::NbtCompound;
use steel_registry::block_entity_type::BlockEntityTypeRef;
use steel_registry::blocks::behavior::PushReaction;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, Direction, PistonType};
use steel_registry::{vanilla_block_entity_types, vanilla_blocks};
use steel_utils::axis::Axis;
use steel_utils::types::UpdateFlags;
use steel_utils::{
    BlockLocalAabb, BlockPos, BlockStateId, DowncastType, DowncastTypeKey, WorldAabb,
};

use crate::behavior::{BLOCK_BEHAVIORS, BlockCollisionBoxes, BlockCollisionContext};
use crate::block_entity::block_state_nbt;
use crate::block_entity::{BlockEntity, BlockEntityTickAction};
use crate::entity::Entity;
use crate::physics::MoverType;
use crate::world::{LevelReader, World};

const PUSH_OFFSET: f64 = 0.01;

thread_local! {
    static NOCLIP: Cell<Option<Direction>> = const { Cell::new(None) };
}

struct NoClipGuard;

impl NoClipGuard {
    fn set(direction: Direction) -> Self {
        NOCLIP.set(Some(direction));
        Self
    }
}

impl Drop for NoClipGuard {
    fn drop(&mut self) {
        NOCLIP.set(None);
    }
}

/// Vanilla `PistonMovingBlockEntity`.
pub struct PistonMovingBlockEntity {
    world: Weak<World>,
    pos: BlockPos,
    state: BlockStateId,
    moved_state: BlockStateId,
    direction: Direction,
    extending: bool,
    source_piston: bool,
    progress: f32,
    progress_o: f32,
    last_ticked: i64,
    removed: bool,
}

// SAFETY: This key is owned by Steel and uniquely identifies `PistonMovingBlockEntity`.
unsafe impl DowncastType for PistonMovingBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/piston_moving");
}

impl PistonMovingBlockEntity {
    /// Creates the default instance used while loading a piston block entity.
    #[must_use]
    pub fn new(world: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        Self::new_moving(
            world,
            pos,
            state,
            vanilla_blocks::AIR.default_state(),
            Direction::Down,
            false,
            false,
        )
    }

    /// Creates a moving block or source-piston entity.
    #[must_use]
    pub const fn new_moving(
        world: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
        moved_state: BlockStateId,
        direction: Direction,
        extending: bool,
        source_piston: bool,
    ) -> Self {
        Self {
            world,
            pos,
            state,
            moved_state,
            direction,
            extending,
            source_piston,
            progress: 0.0,
            progress_o: 0.0,
            last_ticked: 0,
            removed: false,
        }
    }

    /// Returns whether this block is extending.
    #[must_use]
    pub const fn is_extending(&self) -> bool {
        self.extending
    }

    /// Returns the piston facing direction.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// Returns whether this entity represents the source piston or its head.
    #[must_use]
    pub const fn is_source_piston(&self) -> bool {
        self.source_piston
    }

    /// Returns the state being moved.
    #[must_use]
    pub const fn moved_state(&self) -> BlockStateId {
        self.moved_state
    }

    /// Returns the last game time at which this entity ticked.
    #[must_use]
    pub const fn last_ticked(&self) -> i64 {
        self.last_ticked
    }

    /// Returns interpolated movement progress.
    #[must_use]
    pub fn progress(&self, partial_tick: f32) -> f32 {
        let partial_tick = partial_tick.min(1.0);
        (self.progress - self.progress_o).mul_add(partial_tick, self.progress_o)
    }

    /// Returns the movement direction, which reverses while retracting.
    #[must_use]
    pub const fn movement_direction(&self) -> Direction {
        if self.extending {
            self.direction
        } else {
            self.direction.opposite()
        }
    }

    /// Returns the direction used for the final neighbor notification.
    #[must_use]
    pub const fn push_direction(&self) -> Direction {
        self.movement_direction()
    }

    fn extended_progress(&self, progress: f32) -> f32 {
        if self.extending {
            progress - 1.0
        } else {
            1.0 - progress
        }
    }

    fn collision_related_state(&self) -> BlockStateId {
        let behavior = BLOCK_BEHAVIORS.get_behavior(self.moved_state.get_block());
        if !self.extending && self.source_piston && behavior.is_piston_base() {
            vanilla_blocks::PISTON_HEAD
                .default_state()
                .set_value(&BlockStateProperties::SHORT, self.progress > 0.25)
                .set_value(
                    &BlockStateProperties::PISTON_TYPE,
                    if self.moved_state.get_block() == &vanilla_blocks::STICKY_PISTON {
                        PistonType::Sticky
                    } else {
                        PistonType::Normal
                    },
                )
                .set_value(
                    &BlockStateProperties::FACING,
                    self.moved_state.get_value(&BlockStateProperties::FACING),
                )
        } else {
            self.moved_state
        }
    }

    fn state_collision_boxes(
        state: BlockStateId,
        world: &dyn LevelReader,
        pos: BlockPos,
    ) -> BlockCollisionBoxes {
        BLOCK_BEHAVIORS
            .get_behavior(state.get_block())
            .get_collision_boxes(state, world, pos, BlockCollisionContext::empty())
    }

    fn boxes_bounds(boxes: &BlockCollisionBoxes) -> Option<BlockLocalAabb> {
        let mut boxes = boxes.iter().filter(|aabb| !aabb.is_empty());
        let mut bounds = *boxes.next()?;
        for aabb in boxes {
            bounds = BlockLocalAabb::encapsulating(&bounds, aabb);
        }
        Some(bounds)
    }

    /// Resolves the transient collision boxes at the current progress.
    #[must_use]
    pub fn collision_boxes(&self, world: &dyn LevelReader, pos: BlockPos) -> BlockCollisionBoxes {
        let mut result = BlockCollisionBoxes::new();
        let moved_behavior = BLOCK_BEHAVIORS.get_behavior(self.moved_state.get_block());
        if !self.extending && self.source_piston && moved_behavior.is_piston_base() {
            let extended = self
                .moved_state
                .set_value(&BlockStateProperties::EXTENDED, true);
            result.extend(Self::state_collision_boxes(extended, world, pos));
        }

        let no_clip_direction = NOCLIP.get();
        if self.progress < 1.0 && no_clip_direction == Some(self.movement_direction()) {
            return result;
        }

        let moving_state = if self.source_piston {
            vanilla_blocks::PISTON_HEAD
                .default_state()
                .set_value(&BlockStateProperties::FACING, self.direction)
                .set_value(
                    &BlockStateProperties::SHORT,
                    self.extending != ((1.0 - self.progress) < 0.25),
                )
        } else {
            self.moved_state
        };
        let amount = f64::from(self.extended_progress(self.progress));
        let (x, y, z) = self.direction.offset();
        let offset = DVec3::new(
            f64::from(x) * amount,
            f64::from(y) * amount,
            f64::from(z) * amount,
        );
        result.extend(
            Self::state_collision_boxes(moving_state, world, pos)
                .into_iter()
                .map(|aabb| aabb.translate(offset)),
        );
        result
    }

    fn move_by_position_and_progress(&self, pos: BlockPos, aabb: BlockLocalAabb) -> WorldAabb {
        let amount = f64::from(self.extended_progress(self.progress));
        let (x, y, z) = self.direction.offset();
        aabb.at_block(pos).translate(DVec3::new(
            f64::from(x) * amount,
            f64::from(y) * amount,
            f64::from(z) * amount,
        ))
    }

    fn movement_area(aabb: WorldAabb, direction: Direction, amount: f64) -> WorldAabb {
        let signed_amount = if matches!(
            direction,
            Direction::West | Direction::Down | Direction::North
        ) {
            -amount
        } else {
            amount
        };
        let min = signed_amount.min(0.0);
        let max = signed_amount.max(0.0);
        match direction {
            Direction::West => WorldAabb::new(
                aabb.min_x() + min,
                aabb.min_y(),
                aabb.min_z(),
                aabb.min_x() + max,
                aabb.max_y(),
                aabb.max_z(),
            ),
            Direction::East => WorldAabb::new(
                aabb.max_x() + min,
                aabb.min_y(),
                aabb.min_z(),
                aabb.max_x() + max,
                aabb.max_y(),
                aabb.max_z(),
            ),
            Direction::Down => WorldAabb::new(
                aabb.min_x(),
                aabb.min_y() + min,
                aabb.min_z(),
                aabb.max_x(),
                aabb.min_y() + max,
                aabb.max_z(),
            ),
            Direction::Up => WorldAabb::new(
                aabb.min_x(),
                aabb.max_y() + min,
                aabb.min_z(),
                aabb.max_x(),
                aabb.max_y() + max,
                aabb.max_z(),
            ),
            Direction::North => WorldAabb::new(
                aabb.min_x(),
                aabb.min_y(),
                aabb.min_z() + min,
                aabb.max_x(),
                aabb.max_y(),
                aabb.min_z() + max,
            ),
            Direction::South => WorldAabb::new(
                aabb.min_x(),
                aabb.min_y(),
                aabb.max_z() + min,
                aabb.max_x(),
                aabb.max_y(),
                aabb.max_z() + max,
            ),
        }
    }

    fn overlap_movement(outside: WorldAabb, movement: Direction, entity: WorldAabb) -> f64 {
        match movement {
            Direction::East => outside.max_x() - entity.min_x(),
            Direction::West => entity.max_x() - outside.min_x(),
            Direction::Up => outside.max_y() - entity.min_y(),
            Direction::Down => entity.max_y() - outside.min_y(),
            Direction::South => outside.max_z() - entity.min_z(),
            Direction::North => entity.max_z() - outside.min_z(),
        }
    }

    fn move_entity_by_piston(
        piston_direction: Direction,
        entity: &dyn Entity,
        delta: f64,
        movement: Direction,
    ) {
        let _no_clip = NoClipGuard::set(piston_direction);
        let (x, y, z) = movement.offset();
        let previous_position = entity.position();
        entity.move_entity(
            MoverType::Piston,
            DVec3::new(
                delta * f64::from(x),
                delta * f64::from(y),
                delta * f64::from(z),
            ),
        );
        entity.apply_effects_from_blocks_between(previous_position, entity.position());
        entity.remove_latest_movement_recording();
    }

    fn fix_entity_within_piston_base(
        pos: BlockPos,
        entity: &dyn Entity,
        direction: Direction,
        delta_progress: f64,
    ) {
        let entity_aabb = entity.bounding_box();
        let box_at_pos = BlockLocalAabb::FULL_BLOCK.at_block(pos);
        if !entity_aabb.intersects(box_at_pos) {
            return;
        }

        let opposite = direction.opposite();
        let delta = Self::overlap_movement(box_at_pos, opposite, entity_aabb) + PUSH_OFFSET;
        let intersection = WorldAabb::new(
            entity_aabb.min_x().max(box_at_pos.min_x()),
            entity_aabb.min_y().max(box_at_pos.min_y()),
            entity_aabb.min_z().max(box_at_pos.min_z()),
            entity_aabb.max_x().min(box_at_pos.max_x()),
            entity_aabb.max_y().min(box_at_pos.max_y()),
            entity_aabb.max_z().min(box_at_pos.max_z()),
        );
        let intersected_delta =
            Self::overlap_movement(box_at_pos, opposite, intersection) + PUSH_OFFSET;
        if (delta - intersected_delta).abs() < PUSH_OFFSET {
            let delta = delta.min(delta_progress) + PUSH_OFFSET;
            Self::move_entity_by_piston(direction, entity, delta, opposite);
        }
    }

    fn move_collided_entities(&self, world: &Arc<World>, new_progress: f32) {
        let movement = self.movement_direction();
        let delta_progress = f64::from(new_progress - self.progress);
        let shape =
            Self::state_collision_boxes(self.collision_related_state(), world.as_ref(), self.pos);
        let Some(bounds) = Self::boxes_bounds(&shape) else {
            return;
        };
        let aabb = self.move_by_position_and_progress(self.pos, bounds);
        let query =
            WorldAabb::encapsulating(&Self::movement_area(aabb, movement, delta_progress), &aabb);
        let entities = world.get_entities_in_aabb(&query);
        let cause_bounce = self.moved_state.get_block() == &vanilla_blocks::SLIME_BLOCK;

        for entity in entities {
            if entity.piston_push_reaction() == PushReaction::Ignore {
                continue;
            }
            if cause_bounce && entity.as_player().is_none() {
                let mut velocity = entity.velocity();
                let (x, y, z) = movement.offset();
                match movement.axis() {
                    Axis::X => velocity.x = f64::from(x),
                    Axis::Y => velocity.y = f64::from(y),
                    Axis::Z => velocity.z = f64::from(z),
                }
                entity.set_velocity(velocity);
            }

            let mut delta: f64 = 0.0;
            let entity_aabb = entity.bounding_box();
            for shape_aabb in &shape {
                let moving_aabb = Self::movement_area(
                    self.move_by_position_and_progress(self.pos, *shape_aabb),
                    movement,
                    delta_progress,
                );
                if moving_aabb.intersects(entity_aabb) {
                    delta = delta.max(Self::overlap_movement(moving_aabb, movement, entity_aabb));
                    if delta >= delta_progress {
                        break;
                    }
                }
            }

            if delta <= 0.0 {
                continue;
            }
            let delta = delta.min(delta_progress) + PUSH_OFFSET;
            Self::move_entity_by_piston(movement, entity.as_ref(), delta, movement);
            if !self.extending && self.source_piston {
                Self::fix_entity_within_piston_base(
                    self.pos,
                    entity.as_ref(),
                    movement,
                    delta_progress,
                );
            }
        }
    }

    fn move_stuck_entities(&self, world: &Arc<World>, new_progress: f32) {
        if self.moved_state.get_block() != &vanilla_blocks::HONEY_BLOCK {
            return;
        }
        let movement = self.movement_direction();
        if !movement.is_horizontal() {
            return;
        }

        let collision = Self::state_collision_boxes(self.moved_state, world.as_ref(), self.pos);
        let sticky_top = collision
            .iter()
            .map(BlockLocalAabb::max_y)
            .fold(f64::NEG_INFINITY, f64::max);
        let local = BlockLocalAabb::new(0.0, sticky_top, 0.0, 1.0, 1.500_001, 1.0);
        let aabb = self.move_by_position_and_progress(self.pos, local);
        let entities = world.get_entities_in_aabb_matching(&aabb, |entity| {
            let position = entity.position();
            entity.piston_push_reaction() == PushReaction::Normal
                && entity.on_ground()
                && (entity.is_supported_by(self.pos)
                    || (position.x >= aabb.min_x()
                        && position.x <= aabb.max_x()
                        && position.z >= aabb.min_z()
                        && position.z <= aabb.max_z()))
        });
        let delta_progress = f64::from(new_progress - self.progress);
        for entity in entities {
            Self::move_entity_by_piston(movement, entity.as_ref(), delta_progress, movement);
        }
    }

    fn final_state_action(&mut self, world: &Arc<World>) -> BlockEntityTickAction {
        self.removed = true;
        let mut actions = vec![BlockEntityTickAction::RemoveBlockEntity { pos: self.pos }];
        if world.get_block_state(self.pos).get_block() != &vanilla_blocks::MOVING_PISTON {
            return BlockEntityTickAction::Batch(actions);
        }

        let mut new_state = world.update_from_neighbor_shapes(self.moved_state, self.pos);
        if new_state.get_block() == &vanilla_blocks::AIR {
            actions.push(BlockEntityTickAction::SetBlock {
                pos: self.pos,
                state: self.moved_state,
                flags: UpdateFlags::UPDATE_INVISIBLE
                    | UpdateFlags::UPDATE_KNOWN_SHAPE
                    | UpdateFlags::UPDATE_MOVE_BY_PISTON
                    | UpdateFlags::UPDATE_SKIP_BLOCK_ENTITY_SIDEEFFECTS,
                game_event: None,
            });
            actions.push(BlockEntityTickAction::UpdateOrDestroy {
                old_state: self.moved_state,
                new_state,
                pos: self.pos,
                flags: UpdateFlags::UPDATE_ALL,
                update_limit: 512,
            });
            return BlockEntityTickAction::Batch(actions);
        }

        if new_state.try_get_value(&BlockStateProperties::WATERLOGGED) == Some(true) {
            new_state = new_state.set_value(&BlockStateProperties::WATERLOGGED, false);
        }
        actions.push(BlockEntityTickAction::SetBlock {
            pos: self.pos,
            state: new_state,
            flags: UpdateFlags::UPDATE_ALL | UpdateFlags::UPDATE_MOVE_BY_PISTON,
            game_event: None,
        });
        actions.push(BlockEntityTickAction::NeighborChanged {
            pos: self.pos,
            source_block: new_state.get_block(),
        });
        BlockEntityTickAction::Batch(actions)
    }

    /// Completes an in-flight moving block before its piston starts retracting.
    #[must_use]
    pub fn final_tick_action(&mut self, world: &Arc<World>) -> Option<BlockEntityTickAction> {
        if self.progress_o >= 1.0 {
            return None;
        }
        self.progress = 1.0;
        self.progress_o = 1.0;
        self.removed = true;

        let mut actions = vec![BlockEntityTickAction::RemoveBlockEntity { pos: self.pos }];
        if world.get_block_state(self.pos).get_block() == &vanilla_blocks::MOVING_PISTON {
            let new_state = if self.source_piston {
                vanilla_blocks::AIR.default_state()
            } else {
                world.update_from_neighbor_shapes(self.moved_state, self.pos)
            };
            actions.push(BlockEntityTickAction::SetBlock {
                pos: self.pos,
                state: new_state,
                flags: UpdateFlags::UPDATE_ALL,
                game_event: None,
            });
            actions.push(BlockEntityTickAction::NeighborChanged {
                pos: self.pos,
                source_block: new_state.get_block(),
            });
        }
        Some(BlockEntityTickAction::Batch(actions))
    }

    const fn direction_from_legacy_id(id: i32) -> Direction {
        match id {
            1 => Direction::Up,
            2 => Direction::North,
            3 => Direction::South,
            4 => Direction::West,
            5 => Direction::East,
            _ => Direction::Down,
        }
    }

    const fn direction_legacy_id(direction: Direction) -> i32 {
        match direction {
            Direction::Down => 0,
            Direction::Up => 1,
            Direction::North => 2,
            Direction::South => 3,
            Direction::West => 4,
            Direction::East => 5,
        }
    }
}

impl BlockEntity for PistonMovingBlockEntity {
    fn get_type(&self) -> BlockEntityTypeRef {
        &vanilla_block_entity_types::PISTON
    }

    fn get_block_pos(&self) -> BlockPos {
        self.pos
    }

    fn get_block_state(&self) -> BlockStateId {
        self.state
    }

    fn set_block_state(&mut self, state: BlockStateId) {
        self.state = state;
    }

    fn is_removed(&self) -> bool {
        self.removed
    }

    fn set_removed(&mut self) {
        self.removed = true;
    }

    fn clear_removed(&mut self) {
        self.removed = false;
    }

    fn get_level(&self) -> Option<Arc<World>> {
        self.world.upgrade()
    }

    fn pre_remove_side_effects(&mut self, _pos: BlockPos, _state: BlockStateId) {
        if self.progress_o < 1.0 {
            self.progress = 1.0;
            self.progress_o = 1.0;
        }
        self.removed = true;
    }

    fn load_additional(&mut self, nbt: &BorrowedNbtCompound<'_>) {
        let view = NbtCompoundView::from(nbt);
        self.moved_state = view
            .compound("blockState")
            .and_then(block_state_nbt::load)
            .unwrap_or_else(|| vanilla_blocks::AIR.default_state());
        self.direction = Self::direction_from_legacy_id(view.int("facing").unwrap_or(0));
        self.progress = view.float("progress").unwrap_or(0.0);
        self.progress_o = self.progress;
        self.extending = view.byte("extending").is_some_and(|value| value != 0);
        self.source_piston = view.byte("source").is_some_and(|value| value != 0);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        nbt.insert("blockState", block_state_nbt::save(self.moved_state));
        nbt.insert("facing", Self::direction_legacy_id(self.direction));
        nbt.insert("progress", self.progress_o);
        nbt.insert("extending", i8::from(self.extending));
        nbt.insert("source", i8::from(self.source_piston));
    }

    fn get_update_tag(&self) -> Option<NbtCompound> {
        Some(self.save_custom_only())
    }

    fn is_ticking(&self) -> bool {
        true
    }

    fn tick(&mut self, world: &Arc<World>) -> Option<BlockEntityTickAction> {
        self.last_ticked = world.game_time();
        self.progress_o = self.progress;
        if self.progress_o >= 1.0 {
            return Some(self.final_state_action(world));
        }

        let new_progress = self.progress + 0.5;
        self.move_collided_entities(world, new_progress);
        self.move_stuck_entities(world, new_progress);
        self.progress = new_progress.min(1.0);
        None
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use simdnbt::borrow::read_compound as read_borrowed_compound;
    use steel_registry::test_support::init_test_registry;

    use super::*;

    #[test]
    fn moving_state_and_progress_round_trip_with_vanilla_keys() {
        init_test_registry();
        let state = vanilla_blocks::MOVING_PISTON
            .default_state()
            .set_value(&BlockStateProperties::FACING, Direction::West)
            .set_value(&BlockStateProperties::PISTON_TYPE, PistonType::Sticky);
        let moved = vanilla_blocks::PISTON_HEAD
            .default_state()
            .set_value(&BlockStateProperties::FACING, Direction::West)
            .set_value(&BlockStateProperties::PISTON_TYPE, PistonType::Sticky)
            .set_value(&BlockStateProperties::SHORT, true);
        let mut source = PistonMovingBlockEntity::new_moving(
            Weak::new(),
            BlockPos::new(8, 64, -3),
            state,
            moved,
            Direction::West,
            true,
            true,
        );
        source.progress_o = 0.5;

        let mut nbt = NbtCompound::new();
        source.save_additional(&mut nbt);
        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed = read_borrowed_compound(&mut Cursor::new(bytes.as_slice()))
            .expect("test NBT should reborrow");

        let mut loaded = PistonMovingBlockEntity::new(Weak::new(), source.pos, state);
        loaded.load_additional(&borrowed);
        assert_eq!(loaded.moved_state(), moved);
        assert_eq!(loaded.direction(), Direction::West);
        assert!(loaded.is_extending());
        assert!(loaded.is_source_piston());
        assert!((loaded.progress(0.0) - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn movement_area_matches_vanilla_directional_sweep() {
        let aabb = WorldAabb::new(1.0, 2.0, 3.0, 2.0, 3.0, 4.0);
        assert_eq!(
            PistonMovingBlockEntity::movement_area(aabb, Direction::East, 0.5),
            WorldAabb::new(2.0, 2.0, 3.0, 2.5, 3.0, 4.0)
        );
        assert_eq!(
            PistonMovingBlockEntity::movement_area(aabb, Direction::North, 0.5),
            WorldAabb::new(1.0, 2.0, 2.5, 2.0, 3.0, 3.0)
        );
    }
}
