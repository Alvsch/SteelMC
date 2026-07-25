use steel_registry::item_stack::ItemStack;
use steel_utils::{DowncastType, DowncastTypeKey};

use crate::inventory::{
    lock::{ContainerId, ContainerLockGuard, ContainerRef},
    slots::slot::Slot,
};

/// A normal slot that references a container and index.
pub struct NormalSlot {
    container: ContainerRef,
    index: usize,
}

// SAFETY: This key uniquely identifies Steel's `NormalSlot`.
unsafe impl DowncastType for NormalSlot {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:slot/normal");
}

impl NormalSlot {
    /// Creates a new normal slot from a `ContainerRef`.
    pub fn new(container: impl Into<ContainerRef>, index: usize) -> Self {
        Self {
            container: container.into(),
            index,
        }
    }

    /// Returns a reference to the container.
    #[must_use]
    pub fn container_ref(&self) -> ContainerRef {
        self.container.clone()
    }
}

impl Slot for NormalSlot {
    fn get_item<'a>(&self, guard: &'a ContainerLockGuard) -> &'a ItemStack {
        guard
            .get(self.container.container_id())
            .expect("container not locked")
            .get_item(self.index)
    }

    fn get_item_mut<'a>(&self, guard: &'a mut ContainerLockGuard) -> &'a mut ItemStack {
        guard
            .get_mut(self.container.container_id())
            .expect("container not locked")
            .get_item_mut(self.index)
    }

    fn set_item(&self, guard: &mut ContainerLockGuard, stack: ItemStack) {
        assert!(
            guard.set_item(self.container.container_id(), self.index, stack),
            "container not locked"
        );
        self.set_changed(guard);
    }

    fn remove(&self, guard: &mut ContainerLockGuard, amount: i32) -> ItemStack {
        if amount <= 0 || self.get_item(guard).is_empty() {
            return ItemStack::empty();
        }
        guard
            .remove_item(self.container.container_id(), self.index, amount)
            .expect("container not locked")
    }

    fn set_changed(&self, guard: &mut ContainerLockGuard) {
        assert!(
            guard.set_changed(self.container.container_id()),
            "container not locked"
        );
    }

    fn get_container_slot(&self) -> usize {
        self.index
    }

    fn container_key(&self) -> Option<(ContainerId, usize)> {
        Some((self.container.container_id(), self.index))
    }

    fn get_max_stack_size(&self, guard: &ContainerLockGuard) -> i32 {
        guard
            .get(self.container.container_id())
            .expect("container not locked")
            .get_max_stack_size()
    }
}
