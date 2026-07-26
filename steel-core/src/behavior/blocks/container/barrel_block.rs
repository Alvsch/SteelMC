//! Barrel block behavior implementation.
//!
//! Opens a 27-slot container menu when right-clicked.

use std::sync::{Arc, Weak};

use crate::behavior::InventoryAccess;
use crate::behavior::block::{BlockBehavior, BlockEntityCreation};
use crate::behavior::context::{BlockHitResult, BlockPlaceContext, InteractionResult};
use crate::block_entity::BLOCK_ENTITIES;
use crate::block_entity::entities::BarrelBlockEntity;
use crate::inventory::container::calculate_redstone_signal_from_container;
use crate::inventory::lock::{ContainerLockGuard, ContainerRef};
use crate::inventory::menu::kinds::chest_with_openers;
use crate::player::Player;
use crate::world::{LevelReader, World};
use steel_macros::block_behavior;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::blocks::properties::{BlockStateProperties, Direction};
use steel_registry::vanilla_block_entity_types;
use steel_utils::Downcast as _;
use steel_utils::{BlockPos, BlockStateId};

/// Behavior for barrel blocks.
///
/// Barrels are container block entities with 27 slots (3x9 grid).
/// They use the same menu as chests but cannot form double containers.
#[block_behavior]
pub struct BarrelBlock {
    block: BlockRef,
}

impl BarrelBlock {
    /// Creates a new barrel block behavior.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }
}

impl BlockBehavior for BarrelBlock {
    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        // Barrel faces opposite to the player's look direction (all 6 directions).
        let facing = context.get_nearest_looking_direction().opposite();

        Some(
            self.block
                .default_state()
                .set_value(&BlockStateProperties::FACING, facing),
        )
    }

    fn use_without_item(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        _hit_result: &BlockHitResult,
        _inv: &mut InventoryAccess,
    ) -> InteractionResult {
        let Some(block_entity) = world.get_block_entity(pos) else {
            return InteractionResult::Success;
        };
        let Some(barrel) = block_entity.downcast_ref::<BarrelBlockEntity>() else {
            return InteractionResult::Success;
        };
        if !barrel.menu_is_ready(player) {
            return InteractionResult::Success;
        }
        let Some(container_ref) = block_entity.container_ref() else {
            return InteractionResult::Success;
        };
        if !container_ref.prepare_access(Some(player)) {
            return InteractionResult::Success;
        }
        let title = barrel.display_name();

        player.open_menu(title, move |id, _world| {
            chest_with_openers(
                player.inventory.clone(),
                id,
                vec![(container_ref, 27)],
                3,
                vec![block_entity],
            )
        });

        // TODO: Award stat OPEN_BARREL
        // TODO: Anger nearby piglins (PiglinAi.angerNearbyPiglins)

        InteractionResult::Success
    }

    fn tick(&self, _state: BlockStateId, world: &Arc<World>, pos: BlockPos) {
        let Some(block_entity) = world.get_block_entity(pos) else {
            return;
        };
        if let Some(openers) = block_entity.container_openers() {
            openers.recheck_open();
        }
    }

    fn new_block_entity(
        &self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        BlockEntityCreation::from_registered_factory(BLOCK_ENTITIES.create(
            &vanilla_block_entity_types::BARREL,
            level,
            pos,
            state,
        ))
    }

    fn affect_neighbors_after_removal(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        _moved_by_piston: bool,
    ) {
        world.update_neighbor_for_output_signal(pos, self.block);
    }

    fn has_analog_output_signal(&self, _state: BlockStateId) -> bool {
        true
    }

    fn get_analog_output_signal(
        &self,
        _state: BlockStateId,
        world: &dyn LevelReader,
        pos: BlockPos,
        _direction: Direction,
    ) -> i32 {
        let Some(block_entity) = world.get_block_entity(pos) else {
            return 0;
        };
        if block_entity.downcast_ref::<BarrelBlockEntity>().is_none() {
            return 0;
        }
        let Some(container_ref) = ContainerRef::from_block_entity(block_entity) else {
            return 0;
        };
        let guard = ContainerLockGuard::lock_all(&[&container_ref]);
        guard
            .get(container_ref.container_id())
            .map_or(0, |container| {
                calculate_redstone_signal_from_container(container)
            })
    }
}
