use std::{mem, sync::Arc};

use steel_registry::item_stack::ItemStack;

use crate::{
    inventory::{
        lock::{ContainerId, ContainerLockGuard, ContainerRef},
        slots::{ResultHandler, Slot},
    },
    player::Player,
};

/// A fake Slot that contains the resulting item of f.e. a craft
pub struct ResultSlot {
    handler: Arc<dyn ResultHandler + Send + Sync>,
    result_container: ContainerRef,
}

impl ResultSlot {
    /// Creates a new `ResultSlot`
    pub fn new(
        handler: impl ResultHandler + 'static,
        result_container: impl Into<ContainerRef>,
    ) -> Self {
        Self {
            handler: Arc::new(handler),
            result_container: result_container.into(),
        }
    }
}

impl Slot for ResultSlot {
    fn get_item<'a>(&self, guard: &'a ContainerLockGuard) -> &'a ItemStack {
        guard
            .get(self.result_container.container_id())
            .expect("failed to get item from result container")
            .get_item(0)
    }

    fn get_item_mut<'a>(&self, guard: &'a mut ContainerLockGuard) -> &'a mut ItemStack {
        guard
            .get_mut(self.result_container.container_id())
            .expect("failed to get item mutabily from result container")
            .get_item_mut(0)
    }

    fn set_item(&self, guard: &mut ContainerLockGuard, stack: ItemStack) {
        guard
            .get_mut(self.result_container.container_id())
            .expect("failed to get item mutabily from result container")
            .set_item(0, stack);
    }

    fn get_max_stack_size(&self, guard: &ContainerLockGuard) -> i32 {
        guard
            .get(self.result_container.container_id())
            .expect("result container not locked")
            .get_max_stack_size()
    }

    fn set_changed(&self, guard: &mut ContainerLockGuard) {
        guard
            .get_mut(self.result_container.container_id())
            .expect("result container not locked")
            .set_changed();
    }

    fn get_container_slot(&self) -> usize {
        0
    }

    fn container_key(&self) -> Option<(ContainerId, usize)> {
        Some((self.result_container.container_id(), 0))
    }

    fn on_take(
        &self,
        guard: &mut ContainerLockGuard,
        _stack: &ItemStack,
        player: &Player,
    ) -> Option<ItemStack> {
        self.handler.on_result_taken(guard, player)
    }

    fn remove(&self, guard: &mut ContainerLockGuard, _amount: i32) -> ItemStack {
        mem::take(self.get_item_mut(guard))
    }

    fn is_fake(&self) -> bool {
        true
    }

    fn allow_modification(&self, _guard: &ContainerLockGuard, _player: &Player) -> bool {
        false
    }

    fn may_place(&self, _stack: &ItemStack) -> bool {
        false
    }

    fn may_pickup(&self, guard: &ContainerLockGuard, player: &Player) -> bool {
        self.handler.is_result_valid(guard, player)
    }
}
