use std::mem;

use enum_dispatch::enum_dispatch;
use steel_registry::item_stack::ItemStack;

use crate::{
    inventory::{
        lock::{ContainerLockGuard, ContainerRef},
        slots::{Slot, crafting_slots::CraftingHandler},
    },
    player::Player,
};

#[enum_dispatch]
pub trait RecipeHandler: Send + Sync {
    /// Recalculate the result based on current inputs.
    fn update_result(&self, guard: &mut ContainerLockGuard);

    /// Consume inputs when the result is taken. Return overflow remainders.
    fn on_result_taken(&self, guard: &mut ContainerLockGuard, player: &Player)
    -> Option<ItemStack>;
}

#[derive(Clone)]
#[enum_dispatch(RecipeHandler)]
pub enum RecipeHandlerType {
    Crafting(CraftingHandler),
    // Furnace(FurnaceHandler),
    // Loom(LoomHandler),
}

pub struct ProcessingInputSlot {
    pub container_ref: ContainerRef,
    pub index: usize,
    pub handler: RecipeHandlerType,
}

impl ProcessingInputSlot {
    pub fn container_ref(&self) -> ContainerRef {
        self.container_ref.clone()
    }
}

impl Slot for ProcessingInputSlot {
    fn get_item<'a>(&self, guard: &'a ContainerLockGuard) -> &'a ItemStack {
        guard
            .get(self.container_ref.container_id())
            .expect("container_ref should exist in guard")
            .get_item(self.index)
    }

    fn set_item(&self, guard: &mut ContainerLockGuard, stack: ItemStack) {
        guard
            .get_mut(self.container_ref.container_id())
            .expect("container_ref should exist in guard")
            .set_item(self.index, stack);
        self.handler.update_result(guard);
    }

    fn set_changed(&self, guard: &mut ContainerLockGuard) {
        guard
            .get_mut(self.container_ref.container_id())
            .expect("container_ref should exist in guard")
            .set_changed();
        self.handler.update_result(guard);
    }

    fn get_item_mut<'a>(&self, guard: &'a mut ContainerLockGuard) -> &'a mut ItemStack {
        guard
            .get_mut(self.container_ref.container_id())
            .expect("container_ref should exist in guard")
            .get_item_mut(self.index)
    }

    fn get_max_stack_size(&self, _guard: &ContainerLockGuard) -> i32 {
        64
    }

    fn get_container_slot(&self) -> usize {
        self.index
    }
}

pub struct ProcessingResultSlot {
    pub container_ref: ContainerRef,
    pub handler: RecipeHandlerType,
}

impl ProcessingResultSlot {
    pub fn container_ref(&self) -> ContainerRef {
        self.container_ref.clone()
    }
}

impl Slot for ProcessingResultSlot {
    fn may_place(&self, _stack: &ItemStack) -> bool {
        false
    }
    fn allow_modification(&self, _guard: &ContainerLockGuard, _player: &Player) -> bool {
        false
    }
    fn is_fake(&self) -> bool {
        true
    }

    fn remove(&self, guard: &mut ContainerLockGuard, _amount: i32) -> ItemStack {
        mem::take(self.get_item_mut(guard))
    }

    fn on_take(
        &self,
        guard: &mut ContainerLockGuard,
        _stack: &ItemStack,
        player: &Player,
    ) -> Option<ItemStack> {
        self.handler.on_result_taken(guard, player)
    }

    fn get_item<'a>(&self, guard: &'a ContainerLockGuard) -> &'a ItemStack {
        guard
            .get(self.container_ref.container_id())
            .expect("container_ref should exist in guard")
            .get_item(0)
    }

    fn get_item_mut<'a>(&self, guard: &'a mut ContainerLockGuard) -> &'a mut ItemStack {
        guard
            .get_mut(self.container_ref.container_id())
            .expect("container_ref should exist in guard")
            .get_item_mut(0)
    }

    fn set_item(&self, guard: &mut ContainerLockGuard, stack: ItemStack) {
        guard
            .get_mut(self.container_ref.container_id())
            .expect("container_ref should exist in guard")
            .set_item(0, stack);
    }

    fn get_max_stack_size(&self, _guard: &ContainerLockGuard) -> i32 {
        64
    }

    fn set_changed(&self, guard: &mut ContainerLockGuard) {
        guard
            .get_mut(self.container_ref.container_id())
            .expect("container not locked")
            .set_changed();
    }

    fn get_container_slot(&self) -> usize {
        0
    }
}
