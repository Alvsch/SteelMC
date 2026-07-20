//! Shared vanilla pressure-plate behavior.

use std::sync::Arc;

use steel_registry::blocks::BlockRef;
use steel_registry::blocks::properties::Direction;
use steel_registry::blocks::shapes::SupportType;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::{vanilla_blocks, vanilla_game_events};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, WorldAabb};

use crate::behavior::BlockPlaceContext;
use crate::entity::Entity;
use crate::world::game_event_context::GameEventContext;
use crate::world::{LevelReader, World};

const TOUCH_INSET: f64 = 1.0 / 16.0;
const TOUCH_HEIGHT: f64 = 4.0 / 16.0;

/// Common server-side behavior inherited from vanilla's `BasePressurePlateBlock`.
pub(super) struct BasePressurePlateBlock {
    pub(super) block: BlockRef,
}

impl BasePressurePlateBlock {
    #[must_use]
    pub(super) const fn new(block: BlockRef) -> Self {
        Self { block }
    }

    pub(super) fn can_survive(level: &dyn LevelReader, pos: BlockPos) -> bool {
        let below_pos = pos.below();
        let below_state = level.get_block_state(below_pos);
        level.is_face_sturdy_for(below_state, below_pos, Direction::Up, SupportType::Rigid)
            || level.is_face_sturdy_for(below_state, below_pos, Direction::Up, SupportType::Center)
    }

    pub(super) fn state_for_placement(
        &self,
        context: &BlockPlaceContext<'_>,
    ) -> Option<BlockStateId> {
        let state = self.block.default_state();
        Self::can_survive(context.world.as_ref(), context.place_pos()).then_some(state)
    }

    pub(super) fn update_shape(
        state: BlockStateId,
        level: &dyn LevelReader,
        pos: BlockPos,
        direction: Direction,
    ) -> BlockStateId {
        if direction == Direction::Down && !Self::can_survive(level, pos) {
            vanilla_blocks::AIR.default_state()
        } else {
            state
        }
    }

    fn update_neighbors(&self, world: &Arc<World>, pos: BlockPos) {
        world.update_neighbors_at(pos, self.block);
        world.update_neighbors_at(pos.below(), self.block);
    }

    pub(super) fn affect_neighbors_after_removal(
        &self,
        world: &Arc<World>,
        pos: BlockPos,
        moved_by_piston: bool,
        signal: i32,
    ) {
        if !moved_by_piston && signal > 0 {
            self.update_neighbors(world, pos);
        }
    }

    pub(super) fn entity_count(
        world: &World,
        pos: BlockPos,
        mut class_filter: impl FnMut(&dyn Entity) -> bool,
    ) -> usize {
        let min_x = f64::from(pos.x()) + TOUCH_INSET;
        let min_y = f64::from(pos.y());
        let min_z = f64::from(pos.z()) + TOUCH_INSET;
        let bounds = WorldAabb::new(
            min_x,
            min_y,
            min_z,
            f64::from(pos.x() + 1) - TOUCH_INSET,
            min_y + TOUCH_HEIGHT,
            f64::from(pos.z() + 1) - TOUCH_INSET,
        );
        world
            .get_entities_in_aabb_matching(&bounds, |entity| {
                !entity.is_spectator()
                    && !entity.is_ignoring_block_triggers()
                    && class_filter(entity)
            })
            .len()
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "arguments mirror vanilla checkPressed state and variant hooks"
    )]
    pub(super) fn check_pressed(
        &self,
        source_entity: Option<&dyn Entity>,
        world: &Arc<World>,
        pos: BlockPos,
        old_signal: i32,
        signal: i32,
        new_state: BlockStateId,
        pressed_time: i32,
        sound_click_on: SoundEventRef,
        sound_click_off: SoundEventRef,
    ) {
        let was_pressed = old_signal > 0;
        let is_pressed = signal > 0;
        if old_signal != signal {
            world.set_block(pos, new_state, UpdateFlags::UPDATE_CLIENTS);
            self.update_neighbors(world, pos);
            // Vanilla's `setBlocksDirty` is client rendering bookkeeping and
            // is a server-side no-op.
        }

        if !is_pressed && was_pressed {
            world.play_block_sound(
                sound_click_off,
                pos,
                1.0,
                1.0,
                source_entity.map(Entity::id),
            );
            world.game_event(
                &vanilla_game_events::BLOCK_DEACTIVATE,
                pos,
                &GameEventContext::new(source_entity, None),
            );
        } else if is_pressed && !was_pressed {
            world.play_block_sound(sound_click_on, pos, 1.0, 1.0, source_entity.map(Entity::id));
            world.game_event(
                &vanilla_game_events::BLOCK_ACTIVATE,
                pos,
                &GameEventContext::new(source_entity, None),
            );
        }

        if is_pressed {
            world.schedule_block_tick_default(pos, self.block, pressed_time);
        }
    }
}
