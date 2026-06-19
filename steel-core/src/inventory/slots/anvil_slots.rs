use std::sync::{
    Arc,
    atomic::{AtomicI32, Ordering},
};

use steel_registry::{
    REGISTRY, TaggedRegistryExt, blocks::block_state_ext::BlockStateExt, item_stack::ItemStack,
    level_events, vanilla_block_tags::BlockTag, vanilla_blocks,
};
use steel_utils::{BlockPos, types::UpdateFlags};

use crate::{
    behavior::blocks::AnvilBlock,
    inventory::{
        lock::{ContainerId, ContainerLockGuard},
        slots::{ResultHandler, SyncResultContainer, SyncSimpleContainer},
    },
    player::Player,
    world::World,
};

/// Handler for the result slot inside of an anvil, it handles the logic of subtracting the xp and breaking the anvil
#[derive(Clone)]
pub struct AnvilResultHandler {
    input_container: SyncSimpleContainer,
    result_container: SyncResultContainer,
    repair_item_count: Arc<AtomicI32>,
    level_cost: Arc<AtomicI32>,
    block_pos: BlockPos,
    world: Arc<World>,
}

impl AnvilResultHandler {
    /// Creates a new `AnvilResultHandler`
    pub const fn new(
        input_container: SyncSimpleContainer,
        result_container: SyncResultContainer,
        repair_item_count: Arc<AtomicI32>,
        level_cost: Arc<AtomicI32>,
        block_pos: BlockPos,
        world: Arc<World>,
    ) -> Self {
        Self {
            input_container,
            result_container,
            repair_item_count,
            level_cost,
            block_pos,
            world,
        }
    }
}

impl ResultHandler for AnvilResultHandler {
    /// Recalculate the result based on current inputs.
    fn update_result(&self, _guard: &mut ContainerLockGuard) {}

    /// Consume inputs when the result is taken. Return overflow remainders.
    fn on_result_taken(
        &self,
        guard: &mut ContainerLockGuard,
        player: &Player,
    ) -> Option<ItemStack> {
        if !player.has_infinite_materials() {
            let mut experience = player.experience.lock();
            let cost = -self.level_cost.load(Ordering::Relaxed);
            experience.add_levels(cost);
        }

        let input_id = ContainerId::from_arc(&self.input_container);
        let input = guard.get_mut(input_id).expect("input container not locked");

        input.set_item(0, ItemStack::empty());

        let second = input.get_item_mut(1);
        if !second.is_empty() {
            let repair_cost = self.repair_item_count.load(Ordering::Relaxed);
            if repair_cost > 0 {
                second.shrink(repair_cost);
            } else {
                input.set_item(1, ItemStack::empty());
            }
        }

        self.level_cost.store(0, Ordering::Relaxed);

        let state = self.world.get_block_state(self.block_pos);
        if !player.has_infinite_materials()
            && REGISTRY
                .blocks
                .is_in_tag(state.get_block(), &BlockTag::ANVIL)
            && rand::random_bool(0.12)
        {
            if let Some(new_state) = AnvilBlock::damage(state) {
                self.world
                    .set_block(self.block_pos, new_state, UpdateFlags::UPDATE_ALL);
                self.world
                    .level_event(level_events::SOUND_ANVIL_USED, self.block_pos, 0, None);
            } else {
                self.world.set_block(
                    self.block_pos,
                    vanilla_blocks::AIR.default_state(),
                    UpdateFlags::UPDATE_ALL,
                );
                self.world
                    .level_event(level_events::SOUND_ANVIL_BROKEN, self.block_pos, 0, None);
            }
        } else {
            self.world
                .level_event(level_events::SOUND_ANVIL_USED, self.block_pos, 0, None);
        }

        input.set_changed();
        guard
            .get_mut(ContainerId::from_arc(&self.result_container))
            .expect("container not locked")
            .set_changed();
        None
    }

    /// Returns whether the stored result item still matches what the current
    /// inputs would produce. Used to reject stale pickups on result slots.
    fn is_result_valid(&self, _guard: &ContainerLockGuard, player: &Player) -> bool {
        let level_cost = self.level_cost.load(Ordering::Relaxed);
        player.has_infinite_materials()
            || player.experience.lock().level() >= level_cost && level_cost > 0
    }
}
