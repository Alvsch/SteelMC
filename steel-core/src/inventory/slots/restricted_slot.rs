use std::sync::Arc;

use steel_registry::item_stack::ItemStack;

use crate::{
    inventory::{
        lock::{ContainerId, ContainerLockGuard, ContainerRef},
        slots::{NormalSlot, Slot},
    },
    player::Player,
};

/// Predicate deciding whether an item may be placed into a [`RestrictedSlot`].
pub type MayPlaceFn = Arc<dyn Fn(usize, &ItemStack) -> bool + Send + Sync>;
/// Predicate gating pickup from a [`RestrictedSlot`].
pub type MayPickupFn =
    Arc<dyn Fn(usize, &ContainerLockGuard, &Player, &ItemStack) -> bool + Send + Sync>;

/// A [`NormalSlot`] whose place/pickup rules and max stack size are closures.
pub struct RestrictedSlot {
    base: NormalSlot,
    may_place_fn: MayPlaceFn,
    may_pickup_fn: Option<MayPickupFn>,
    max_stack: i32,
}

impl RestrictedSlot {
    /// Creates a restricted slot. `None` pickup fn always allows pickup.
    pub fn new(
        container: impl Into<ContainerRef>,
        index: usize,
        may_place_fn: MayPlaceFn,
        may_pickup_fn: Option<MayPickupFn>,
        max_stack: i32,
    ) -> Self {
        Self {
            base: NormalSlot::new(container, index),
            may_place_fn,
            may_pickup_fn,
            max_stack,
        }
    }
}

impl Slot for RestrictedSlot {
    fn get_item<'a>(&self, guard: &'a ContainerLockGuard) -> &'a ItemStack {
        self.base.get_item(guard)
    }

    fn get_item_mut<'a>(&self, guard: &'a mut ContainerLockGuard) -> &'a mut ItemStack {
        self.base.get_item_mut(guard)
    }

    fn set_item(&self, guard: &mut ContainerLockGuard, stack: ItemStack) {
        self.base.set_item(guard, stack);
    }

    fn may_place(&self, stack: &ItemStack) -> bool {
        (self.may_place_fn)(self.base.get_container_slot(), stack)
    }

    fn may_pickup(&self, guard: &ContainerLockGuard, player: &Player) -> bool {
        (self.may_pickup_fn).as_ref().is_none_or(|it| {
            it(
                self.base.get_container_slot(),
                guard,
                player,
                self.base.get_item(guard),
            )
        })
    }

    fn get_max_stack_size(&self, _guard: &ContainerLockGuard) -> i32 {
        self.max_stack
    }

    fn set_changed(&self, guard: &mut ContainerLockGuard) {
        self.base.set_changed(guard);
    }

    fn get_container_slot(&self) -> usize {
        self.base.get_container_slot()
    }

    fn container_key(&self) -> Option<(ContainerId, usize)> {
        self.base.container_key()
    }
}
