use std::{
    mem,
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
};

use steel_registry::{
    REGISTRY, TaggedRegistryExt, blocks::block_state_ext::BlockStateExt, item_stack::ItemStack,
    level_events, vanilla_block_tags, vanilla_blocks,
};
use steel_utils::{BlockPos, types::UpdateFlags};

use crate::{
    behavior::blocks::AnvilBlock,
    inventory::{
        lock::{ContainerId, ContainerLockGuard, ContainerRef},
        slots::{Slot, SyncResultContainer, SyncSimpleContainer},
    },
    player::Player,
    world::World,
};

/// The Result Slot in an Anvil
pub struct AnvilResultSlot {
    input_container: SyncSimpleContainer,
    result_container: SyncResultContainer,
    repair_item_count: Arc<AtomicI32>,
    level_cost: Arc<AtomicI32>,
    block_pos: BlockPos,
    world: Arc<World>,
}

impl AnvilResultSlot {
    /// Creates a new Anvil Result Slot
    pub const fn new(
        input_container: SyncSimpleContainer,
        result_container: SyncResultContainer,
        repair_item_count: Arc<AtomicI32>,
        level_cost: Arc<AtomicI32>,
        pos: BlockPos,
        world: Arc<World>,
    ) -> Self {
        Self {
            input_container,
            result_container,
            repair_item_count,
            level_cost,
            block_pos: pos,
            world,
        }
    }

    /// Returns a reference to the result container.
    #[must_use]
    pub fn result_container_ref(&self) -> ContainerRef {
        ContainerRef::ResultContainer(Arc::clone(&self.result_container))
    }

    /// Returns a reference to the input container.
    #[must_use]
    pub fn input_container_ref(&self) -> ContainerRef {
        ContainerRef::SimpleContainer(Arc::clone(&self.input_container))
    }
}

impl Slot for AnvilResultSlot {
    /// Returns a reference to the item in this slot.
    fn get_item<'a>(&self, guard: &'a ContainerLockGuard) -> &'a ItemStack {
        guard
            .get(ContainerId::from_arc(&self.result_container))
            .expect("container not locked")
            .get_item(0)
    }

    /// Returns a mutable reference to the item in this slot.
    fn get_item_mut<'a>(&self, guard: &'a mut ContainerLockGuard) -> &'a mut ItemStack {
        guard
            .get_mut(ContainerId::from_arc(&self.result_container))
            .expect("container not locked")
            .get_item_mut(0)
    }

    /// Sets the item in this slot.
    fn set_item(&self, guard: &mut ContainerLockGuard, stack: ItemStack) {
        guard
            .get_mut(ContainerId::from_arc(&self.result_container))
            .expect("container not locked")
            .set_item(0, stack);
    }

    /// Returns the maximum stack size for this slot
    ///
    /// For normal slots, this delegates to the container's max stack size
    /// For special slots (like armor), this may return a fixed value (e.g., 1)
    fn get_max_stack_size(&self, guard: &ContainerLockGuard) -> i32 {
        guard
            .get(ContainerId::from_arc(&self.result_container))
            .expect("container not locked")
            .get_max_stack_size()
    }

    /// Marks the slot's container as changed
    fn set_changed(&self, guard: &mut ContainerLockGuard) {
        guard
            .get_mut(ContainerId::from_arc(&self.result_container))
            .expect("container not locked")
            .set_changed();
    }

    /// Returns the container slot index
    fn get_container_slot(&self) -> usize {
        0
    }

    /// Cannot place items directly in the result slot.
    fn may_place(&self, _stack: &ItemStack) -> bool {
        false
    }

    /// Result slots don't allow partial removal.
    fn allow_modification(&self, _guard: &ContainerLockGuard, _player: &Player) -> bool {
        false
    }

    /// Removes items from the anvil result slot and remove xp.
    ///
    /// Unlike normal slots, this **always takes the entire stack** regardless
    /// of the `amount` parameter.
    fn remove(&self, guard: &mut ContainerLockGuard, _amount: i32) -> ItemStack {
        mem::take(self.get_item_mut(guard))
    }

    fn on_take(
        &self,
        guard: &mut ContainerLockGuard,
        _stack: &ItemStack,
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
                .is_in_tag(state.get_block(), &vanilla_block_tags::ANVIL_TAG)
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
                    UpdateFlags::UPDATE_CLIENTS,
                );
                self.world
                    .level_event(level_events::SOUND_ANVIL_BROKEN, self.block_pos, 0, None);
            }
        } else {
            self.world
                .level_event(level_events::SOUND_ANVIL_USED, self.block_pos, 0, None);
        }

        input.set_changed();
        None
    }

    fn is_fake(&self) -> bool {
        true
    }

    fn may_pickup(&self, _guard: &ContainerLockGuard, player: &Player) -> bool {
        let level_cost = self.level_cost.load(Ordering::Relaxed);
        player.has_infinite_materials()
            || player.experience.lock().level() >= level_cost && level_cost > 0
    }
}
