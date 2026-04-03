use std::{
    mem,
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
};

use steel_registry::item_stack::ItemStack;

use crate::{
    inventory::{
        lock::{ContainerId, ContainerLockGuard, ContainerRef},
        slots::{Slot, SyncResultContainer, SyncSimpleContainer},
    },
    player::Player,
};

/// A slot in a anvil input.
pub struct AnvilInputSlot {
    input_container: SyncSimpleContainer,
    result_container: SyncResultContainer,
    index: usize,
}

impl AnvilInputSlot {
    /// Creates a new anvil slot
    pub const fn new(
        input_container: SyncSimpleContainer,
        result_container: SyncResultContainer,
        index: usize,
    ) -> Self {
        Self {
            input_container,
            result_container,
            index,
        }
    }

    /// Returns a reference to the crafting container.
    #[must_use]
    pub fn input_container_ref(&self) -> ContainerRef {
        ContainerRef::SimpleContainer(Arc::clone(&self.input_container))
    }

    /// Returns a reference to the result container.
    #[must_use]
    pub fn result_container_ref(&self) -> ContainerRef {
        ContainerRef::ResultContainer(Arc::clone(&self.result_container))
    }
}

impl Slot for AnvilInputSlot {
    fn get_item<'a>(&self, guard: &'a ContainerLockGuard) -> &'a ItemStack {
        guard
            .get(ContainerId::from_arc(&self.input_container))
            .expect("container not locked")
            .get_item(self.index)
    }

    fn get_item_mut<'a>(&self, guard: &'a mut ContainerLockGuard) -> &'a mut ItemStack {
        guard
            .get_mut(ContainerId::from_arc(&self.input_container))
            .expect("container not locked")
            .get_item_mut(self.index)
    }

    fn set_item(&self, guard: &mut ContainerLockGuard, stack: ItemStack) {
        guard
            .get_mut(ContainerId::from_arc(&self.input_container))
            .expect("container not locked")
            .set_item(self.index, stack);
    }

    fn set_changed(&self, guard: &mut ContainerLockGuard) {
        guard
            .get_mut(ContainerId::from_arc(&self.input_container))
            .expect("container not locked")
            .set_changed();
    }

    fn get_container_slot(&self) -> usize {
        self.index
    }

    fn get_max_stack_size(&self, guard: &ContainerLockGuard) -> i32 {
        guard
            .get(ContainerId::from_arc(&self.input_container))
            .expect("container not locked")
            .get_max_stack_size()
    }
}

/// The Result Slot in an Anvil
pub struct AnvilResultSlot {
    input_container: SyncSimpleContainer,
    result_container: SyncResultContainer,
    cost: Arc<AtomicI32>,
}

impl AnvilResultSlot {
    /// Creates a new Anvil Result Slot
    pub const fn new(
        input_container: SyncSimpleContainer,
        result_container: SyncResultContainer,
        cost: Arc<AtomicI32>,
    ) -> Self {
        Self {
            input_container,
            result_container,
            cost,
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
            experience.add_levels(-self.cost.load(Ordering::Relaxed));
            // FIXME: doesnt seem to always we accurate
        }

        let input_id = ContainerId::from_arc(&self.input_container);
        let input = guard.get_mut(input_id).expect("input container not locked");

        input.set_item(0, ItemStack::empty());

        let second = input.get_item_mut(1);
        if !second.is_empty() {
            let repair_cost = self.cost.load(Ordering::Relaxed);
            if repair_cost > 0 {
                second.shrink(repair_cost);
            } else {
                input.set_item(1, ItemStack::empty());
            }
        }

        input.set_changed();
        None
    }

    fn is_fake(&self) -> bool {
        true
    }
}
