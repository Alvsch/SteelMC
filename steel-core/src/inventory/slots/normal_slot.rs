use steel_registry::item_stack::ItemStack;

use crate::inventory::{
    lock::{ContainerLockGuard, ContainerRef},
    slots::slot::Slot,
};

/// A normal slot that references a container and index.
pub struct NormalSlot {
    container: ContainerRef,
    index: usize,
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
        guard
            .get_mut(self.container.container_id())
            .expect("container not locked")
            .set_item(self.index, stack);
    }

    fn set_changed(&self, guard: &mut ContainerLockGuard) {
        guard
            .get_mut(self.container.container_id())
            .expect("container not locked")
            .set_changed();
    }

    fn get_container_slot(&self) -> usize {
        self.index
    }

    fn get_max_stack_size(&self, guard: &ContainerLockGuard) -> i32 {
        guard
            .get(self.container.container_id())
            .expect("container not locked")
            .get_max_stack_size()
    }
}
