use std::sync::{Arc, Weak};

use glam::DVec3;
use steel_macros::block_behavior;
use steel_registry::{
    block_entity_type::BlockEntityTypeRef,
    blocks::{
        BlockRef,
        block_state_ext::BlockStateExt,
        properties::{BlockStateProperties, EnumProperty},
        shapes::VoxelShape,
    },
    item_stack::ItemStack,
    items::item::BlockHitResult,
    vanilla_block_entity_types, vanilla_custom_stats,
};
use steel_utils::{BlockPos, BlockStateId, Direction, Downcast, translations};
use text_components::TextComponent;

use crate::{
    behavior::{
        BlockBehavior, BlockCollisionBoxes, BlockCollisionContext, BlockEntityCreation,
        BlockLootContext, BlockPlaceContext, InteractionResult, InventoryAccess,
    },
    block_entity::{
        BLOCK_ENTITIES, BlockEntity, BlockEntityTicker,
        entities::{AnimationStatus, ShulkerBoxBlockEntity, get_progress_delta_aabb},
    },
    inventory::{
        container::calculate_redstone_signal_from_container,
        lock::{ContainerLockGuard, ContainerRef},
        menu::kinds::shulker_box,
    },
    physics::{CollisionWorld, WorldCollisionProvider},
    player::Player,
    world::{LevelReader, World},
};

/// Behavior for barrel blocks.
#[block_behavior]
pub struct ShulkerBoxBlock {
    block: BlockRef,
}

impl ShulkerBoxBlock {
    pub const FACING: &EnumProperty<Direction> = &BlockStateProperties::HORIZONTAL_FACING;

    /// Creates a new shulker block behavior.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }
}

fn can_open(
    state: BlockStateId,
    world: &Arc<World>,
    pos: BlockPos,
    block_entity: &ShulkerBoxBlockEntity,
) -> bool {
    if !matches!(block_entity.animation_status(), AnimationStatus::Closed) {
        return true;
    }

    let direction = state.get_value(&BlockStateProperties::FACING);

    let lid_open_bounding_box = get_progress_delta_aabb(
        1.0,
        direction,
        0.0,
        0.5,
        DVec3::from(pos.get_bottom_center()),
    )
    .deflate(1.0E-6);

    let collision = WorldCollisionProvider::new(world);
    !collision.has_block_collision(&lid_open_bounding_box)
}

impl BlockBehavior for ShulkerBoxBlock {
    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        let facing = context.clicked_face();
        Some(
            self.block
                .default_state()
                .set_value(&BlockStateProperties::FACING, facing),
        )
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
        if let Some(block_entity) = world.get_block_entity(pos)
            && let Some(block_entity) = block_entity.downcast_ref::<ShulkerBoxBlockEntity>()
            && let Some(container_ref) = block_entity.container_ref()
            && can_open(state, world, pos, block_entity)
        {
            let inventory = player.inventory.clone();
            player.open_menu(
                TextComponent::translated(translations::CONTAINER_SHULKER_BOX.msg()),
                move |context| shulker_box(inventory, context.container_id, container_ref),
            );

            player.award_custom_stat(&vanilla_custom_stats::OPEN_SHULKER_BOX);
            // TODO: Anger nearby piglins (PiglinAi.angerNearbyPiglins)
        }
        InteractionResult::Success
    }

    fn player_will_destroy(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
    ) -> BlockStateId {
        let Some(block_entity) = world.get_block_entity(pos) else {
            return state;
        };

        let Some(shulker_box_block_entity) = block_entity.downcast_ref::<ShulkerBoxBlockEntity>()
        else {
            return state;
        };

        if player.prevents_block_drops() && !shulker_box_block_entity.is_empty() {
            let item = shulker_box_block_entity.shulker_box_as_item(state);
            world.pop_resource(pos, item);
        } else {
            // TODO: maybe? shulkerBoxBlockEntity.unpackLootTable(player);
        }

        state
    }

    fn get_drops(
        &self,
        state: BlockStateId,
        context: &BlockLootContext<'_>,
    ) -> Option<Vec<ItemStack>> {
        let block_entity = context.block_entity()?;
        let shulker_box_block_entity = block_entity.downcast_ref::<ShulkerBoxBlockEntity>()?;

        let item = shulker_box_block_entity.shulker_box_as_item(state);
        Some(vec![item])
    }

    fn new_block_entity(
        &self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        BlockEntityCreation::from_registered_factory(BLOCK_ENTITIES.create(
            &vanilla_block_entity_types::SHULKER_BOX,
            level,
            pos,
            state,
        ))
    }

    fn get_block_entity_ticker(
        &self,
        _world: &Arc<World>,
        _state: BlockStateId,
        block_entity_type: BlockEntityTypeRef,
    ) -> Option<BlockEntityTicker> {
        BlockEntityTicker::for_matching_entity_tick(
            block_entity_type,
            &vanilla_block_entity_types::SHULKER_BOX,
        )
    }

    fn trigger_event(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        event: i32,
        data: i32,
    ) -> bool {
        let Some(block_entity) = world.get_block_entity(pos) else {
            return false;
        };
        block_entity.trigger_event(event, data)
    }

    fn get_collision_shape(
        &self,
        state: BlockStateId,
        world: &dyn LevelReader,
        pos: BlockPos,
        _context: BlockCollisionContext,
    ) -> VoxelShape {
        if let Some(block_entity) = world.get_block_entity(pos)
            && let Some(shulker_block_entity) = block_entity.downcast_ref::<ShulkerBoxBlockEntity>()
        {
            VoxelShape::from_boxes(&[shulker_block_entity.get_bounding_box(state)])
        } else {
            VoxelShape::FULL_BLOCK
        }
    }

    fn get_block_support_boxes(
        &self,
        state: BlockStateId,
        world: &dyn LevelReader,
        pos: BlockPos,
    ) -> BlockCollisionBoxes {
        if let Some(block_entity) = world.get_block_entity(pos)
            && let Some(shulker_block_entity) = block_entity.downcast_ref::<ShulkerBoxBlockEntity>()
            && matches!(
                shulker_block_entity.animation_status(),
                AnimationStatus::Closed
            )
        {
        } else {
            BlockCollisionBoxes::from_slice(VoxelShape::FULL_BLOCK.boxes())
        }
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
        // Get the block entity and calculate signal from container contents
        let Some(container_ref) = world
            .get_block_entity(pos)
            .and_then(ContainerRef::from_block_entity)
        else {
            return 0;
        };
        let guard = ContainerLockGuard::lock_all(&[&container_ref]);
        guard
            .get(container_ref.container_id())
            .map_or(0, |container| {
                calculate_redstone_signal_from_container(container)
            })
    }

    fn fits_inside_container_items(&self) -> bool {
        false
    }
}
