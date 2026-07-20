//! Vanilla lever behavior.

use std::sync::Arc;

use steel_macros::block_behavior;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, Direction};
use steel_registry::{sound_events, vanilla_game_events};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId};

use super::face_attached_horizontal_directional_block::FaceAttachedHorizontalDirectionalBlock;
use crate::behavior::{
    BlockBehavior, BlockHitResult, BlockPlaceContext, InteractionResult, InventoryAccess,
};
use crate::entity::Entity;
use crate::player::Player;
use crate::world::game_event_context::GameEventContext;
use crate::world::{LevelReader, ScheduledTickAccess, SignalQueryContext, World};

/// Vanilla `LeverBlock` source behavior.
#[block_behavior]
pub struct LeverBlock {
    face_attached: FaceAttachedHorizontalDirectionalBlock,
}

impl LeverBlock {
    /// Creates lever behavior for `block`.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self {
            face_attached: FaceAttachedHorizontalDirectionalBlock::new(block),
        }
    }

    fn update_neighbors(&self, state: BlockStateId, world: &Arc<World>, pos: BlockPos) {
        let support_direction =
            FaceAttachedHorizontalDirectionalBlock::connected_direction(state).opposite();
        world.update_neighbors_at(pos, self.face_attached.block);
        world.update_neighbors_at(pos.relative(support_direction), self.face_attached.block);
    }

    fn pull(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: Option<&Player>,
    ) {
        let powered = !state.get_value(&BlockStateProperties::POWERED);
        let next_state = state.set_value(&BlockStateProperties::POWERED, powered);
        world.set_block(pos, next_state, UpdateFlags::UPDATE_ALL);
        self.update_neighbors(next_state, world, pos);
        world.play_block_sound(
            &sound_events::BLOCK_LEVER_CLICK,
            pos,
            0.3,
            if powered { 0.6 } else { 0.5 },
            player.map(Entity::id),
        );
        world.game_event(
            if powered {
                &vanilla_game_events::BLOCK_ACTIVATE
            } else {
                &vanilla_game_events::BLOCK_DEACTIVATE
            },
            pos,
            &GameEventContext::new(player.map(|player| player as &dyn Entity), None),
        );
    }
}

impl BlockBehavior for LeverBlock {
    fn can_survive(&self, state: BlockStateId, world: &dyn LevelReader, pos: BlockPos) -> bool {
        FaceAttachedHorizontalDirectionalBlock::can_survive(state, world, pos)
    }

    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        self.face_attached.state_for_placement(context)
    }

    fn update_shape(
        &self,
        state: BlockStateId,
        world: &dyn ScheduledTickAccess,
        pos: BlockPos,
        direction: Direction,
        _neighbor_pos: BlockPos,
        _neighbor_state: BlockStateId,
    ) -> BlockStateId {
        FaceAttachedHorizontalDirectionalBlock::update_shape(state, world, pos, direction)
    }

    fn use_without_item(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        _hit_result: &BlockHitResult,
        _inv: &mut InventoryAccess,
    ) -> InteractionResult {
        self.pull(state, world, pos, Some(player));
        InteractionResult::Success
    }

    fn affect_neighbors_after_removal(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        moved_by_piston: bool,
    ) {
        if !moved_by_piston && state.get_value(&BlockStateProperties::POWERED) {
            self.update_neighbors(state, world, pos);
        }
    }

    fn is_signal_source(&self, _state: BlockStateId, _context: SignalQueryContext) -> bool {
        true
    }

    fn get_own_signal(
        &self,
        state: BlockStateId,
        _world: &dyn LevelReader,
        _pos: BlockPos,
        _context: SignalQueryContext,
    ) -> i32 {
        if state.get_value(&BlockStateProperties::POWERED) {
            15
        } else {
            0
        }
    }

    fn get_direct_signal(
        &self,
        state: BlockStateId,
        _world: &dyn LevelReader,
        _pos: BlockPos,
        direction: Direction,
        _context: SignalQueryContext,
    ) -> i32 {
        if state.get_value(&BlockStateProperties::POWERED)
            && FaceAttachedHorizontalDirectionalBlock::connected_direction(state) == direction
        {
            15
        } else {
            0
        }
    }

    // Client-local interaction/ambient dust particles are omitted. Explosion
    // toggling awaits Steel's shared block-explosion callback foundation.
}

#[cfg(test)]
mod tests {
    use steel_registry::blocks::properties::AttachFace;
    use steel_registry::test_support::init_test_registry;
    use steel_registry::vanilla_blocks;

    use super::*;
    use crate::test_support::TestLevel;

    fn lever_state(facing: Direction, face: AttachFace, powered: bool) -> BlockStateId {
        vanilla_blocks::LEVER
            .default_state()
            .set_value(&BlockStateProperties::HORIZONTAL_FACING, facing)
            .set_value(&BlockStateProperties::ATTACH_FACE, face)
            .set_value(&BlockStateProperties::POWERED, powered)
    }

    #[test]
    fn wall_lever_survives_only_with_its_backing_face() {
        init_test_registry();
        let behavior = LeverBlock::new(&vanilla_blocks::LEVER);
        let pos = BlockPos::new(0, 64, 0);
        let state = lever_state(Direction::East, AttachFace::Wall, false);
        let supported =
            TestLevel::default().with_block(pos.west(), vanilla_blocks::STONE.default_state());

        assert!(behavior.can_survive(state, &supported, pos));
        assert!(!behavior.can_survive(state, &TestLevel::default(), pos));
    }

    #[test]
    fn powered_lever_strongly_powers_only_away_from_support() {
        init_test_registry();
        let behavior = LeverBlock::new(&vanilla_blocks::LEVER);
        let state = lever_state(Direction::East, AttachFace::Wall, true);
        let level = TestLevel::default();
        let pos = BlockPos::new(0, 64, 0);

        assert_eq!(
            behavior.get_own_signal(state, &level, pos, SignalQueryContext::DEFAULT),
            15
        );
        assert_eq!(
            behavior.get_direct_signal(
                state,
                &level,
                pos,
                Direction::East,
                SignalQueryContext::DEFAULT,
            ),
            15
        );
        assert_eq!(
            behavior.get_direct_signal(
                state,
                &level,
                pos,
                Direction::West,
                SignalQueryContext::DEFAULT,
            ),
            0
        );
    }
}
