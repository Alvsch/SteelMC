use std::sync::Arc;

use steel_registry::item_stack::ItemStack;

use crate::{
    inventory::{
        lock::{ContainerLockGuard, ContainerRef},
        slots::{NormalSlot, Slot},
    },
    player::Player,
};

/// Shared predicate deciding whether an item may be placed into a
/// [`RestrictedSlot`]. Stored behind an [`Arc`] so one closure serves every
/// slot in a section.
pub type MayPlaceFn = Arc<dyn Fn(&ItemStack) -> bool + Send + Sync>;
/// Shared predicate gating whether the player may take the current item back
/// out of a [`RestrictedSlot`]; receives the lock guard, the player, and the
/// item being removed.
pub type MayPickupFn = Arc<dyn Fn(&ContainerLockGuard, &Player, &ItemStack) -> bool + Send + Sync>;

/// A [`NormalSlot`] whose place/pickup rules and max stack size are supplied
/// as closures instead of a dedicated [`Slot`] impl.
///
/// Built via
/// [`MenuBuilder::restricted_section`](crate::inventory::MenuBuilder::restricted_section).
pub struct RestrictedSlot {
    base: NormalSlot,
    may_place_fn: MayPlaceFn,
    may_pickup_fn: Option<MayPickupFn>,
    max_stack: i32,
}

impl RestrictedSlot {
    /// Creates a restricted slot over `container[index]`. Pass `None` for
    /// `may_pickup_fn` to always allow pickup.
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
    #[doc = " Returns a reference to the item in this slot."]
    fn get_item<'a>(&self, guard: &'a ContainerLockGuard) -> &'a ItemStack {
        self.base.get_item(guard)
    }

    #[doc = " Returns a mutable reference to the item in this slot."]
    fn get_item_mut<'a>(&self, guard: &'a mut ContainerLockGuard) -> &'a mut ItemStack {
        self.base.get_item_mut(guard)
    }

    #[doc = " Sets the item in this slot."]
    fn set_item(&self, guard: &mut ContainerLockGuard, stack: ItemStack) {
        self.base.set_item(guard, stack);
    }

    fn may_place(&self, stack: &ItemStack) -> bool {
        (self.may_place_fn)(stack)
    }

    fn may_pickup(&self, guard: &ContainerLockGuard, player: &Player) -> bool {
        (self.may_pickup_fn)
            .as_ref()
            .is_none_or(|it| it(guard, player, self.base.get_item(guard)))
    }

    #[doc = " Returns the maximum stack size for this slot."]
    #[doc = ""]
    #[doc = " For normal slots, this delegates to the container\'s max stack size."]
    #[doc = " For special slots (like armor), this may return a fixed value (e.g., 1)."]
    fn get_max_stack_size(&self, _guard: &ContainerLockGuard) -> i32 {
        self.max_stack
    }

    #[doc = " Marks the slot\'s container as changed."]
    fn set_changed(&self, guard: &mut ContainerLockGuard) {
        self.base.set_changed(guard);
    }

    #[doc = " Returns the container slot index."]
    fn get_container_slot(&self) -> usize {
        self.base.get_container_slot()
    }
}
