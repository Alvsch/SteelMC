//! Stateless ticking storage for daylight detectors.

use std::sync::{Arc, Weak};

use simdnbt::borrow::BaseNbtCompound as BorrowedNbtCompound;
use simdnbt::owned::NbtCompound;
use steel_registry::block_entity_type::BlockEntityTypeRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::BlockStateProperties;
use steel_registry::vanilla_block_entity_types;
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

use crate::behavior::blocks::DaylightDetectorBlock;
use crate::block_entity::{BlockEntity, BlockEntityTickAction};
use crate::world::World;

/// Vanilla `DaylightDetectorBlockEntity`.
pub struct DaylightDetectorBlockEntity {
    world: Weak<World>,
    pos: BlockPos,
    state: BlockStateId,
    removed: bool,
}

// SAFETY: This key is owned by Steel and uniquely identifies
// `DaylightDetectorBlockEntity`.
unsafe impl DowncastType for DaylightDetectorBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/daylight_detector");
}

impl DaylightDetectorBlockEntity {
    /// Creates daylight-detector ticking storage.
    #[must_use]
    pub const fn new(world: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        Self {
            world,
            pos,
            state,
            removed: false,
        }
    }
}

impl BlockEntity for DaylightDetectorBlockEntity {
    fn get_type(&self) -> BlockEntityTypeRef {
        &vanilla_block_entity_types::DAYLIGHT_DETECTOR
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

    fn load_additional(&mut self, _nbt: &BorrowedNbtCompound<'_>) {}

    fn save_additional(&self, _nbt: &mut NbtCompound) {}

    fn is_ticking(&self) -> bool {
        self.world
            .upgrade()
            .is_some_and(|world| world.dimension_type.has_skylight)
    }

    fn tick(&mut self, world: &Arc<World>) -> Option<BlockEntityTickAction> {
        if world.game_time() % 20 != 0 {
            return None;
        }
        let target = DaylightDetectorBlock::signal_strength(world, self.pos, self.state);
        if self.state.get_value(&BlockStateProperties::POWER) == target {
            return None;
        }
        Some(BlockEntityTickAction::SetBlock {
            pos: self.pos,
            state: self.state.set_value(&BlockStateProperties::POWER, target),
            flags: UpdateFlags::UPDATE_ALL,
            game_event: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::test_support::init_test_registry;
    use steel_registry::{vanilla_blocks, vanilla_world_clocks};

    use super::*;
    use crate::test_support::fresh_test_world;

    #[test]
    fn detector_updates_only_on_vanilla_twenty_game_tick_cadence() {
        init_test_registry();
        let world = fresh_test_world("daylight_detector_cadence");
        assert_eq!(
            world.set_clock_total_ticks(&vanilla_world_clocks::OVERWORLD, 18_000),
            Some(())
        );
        let state = vanilla_blocks::DAYLIGHT_DETECTOR
            .default_state()
            .set_value(&BlockStateProperties::INVERTED, true);
        let mut detector = DaylightDetectorBlockEntity::new(
            Arc::downgrade(&world),
            BlockPos::new(4, 64, 4),
            state,
        );

        assert!(matches!(
            detector.tick(&world),
            Some(BlockEntityTickAction::SetBlock { state, .. })
                if state.get_value(&BlockStateProperties::POWER) == 11
        ));

        world.level_data.write().set_game_time(1);
        assert!(detector.tick(&world).is_none());
    }
}
