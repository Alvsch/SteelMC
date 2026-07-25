use std::sync::Arc;

use steel_registry::item_stack::ItemStack;
use steel_utils::{DowncastType, DowncastTypeKey};

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

/// A [`NormalSlot`] with custom place and pickup rules.
pub struct RestrictedSlot {
    base: NormalSlot,
    may_place_fn: MayPlaceFn,
    may_pickup_fn: Option<MayPickupFn>,
}

// SAFETY: This key uniquely identifies Steel's `RestrictedSlot`.
unsafe impl DowncastType for RestrictedSlot {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:slot/restricted");
}

impl RestrictedSlot {
    /// Creates a restricted slot. `None` pickup fn always allows pickup.
    pub fn new(
        container: impl Into<ContainerRef>,
        index: usize,
        may_place_fn: MayPlaceFn,
        may_pickup_fn: Option<MayPickupFn>,
    ) -> Self {
        Self {
            base: NormalSlot::new(container, index),
            may_place_fn,
            may_pickup_fn,
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

    fn get_max_stack_size(&self, guard: &ContainerLockGuard) -> i32 {
        self.base.get_max_stack_size(guard)
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

#[cfg(test)]
mod tests {
    use std::slice;
    use std::sync::Arc;

    use super::RestrictedSlot;
    use crate::inventory::container::{Container, SimpleContainer};
    use crate::inventory::lock::{ContainerLockGuard, ContainerRef};
    use crate::inventory::slots::Slot as _;
    use steel_registry::data_components::vanilla_components::MAX_STACK_SIZE;
    use steel_registry::{item_stack::ItemStack, test_support::init_test_registry, vanilla_items};
    use steel_utils::locks::{IntoShared as _, SyncMutex};
    use steel_utils::{DowncastType, DowncastTypeKey};

    struct SingleItemContainer {
        item: ItemStack,
        max_stack_size: i32,
    }

    // SAFETY: This test-only key uniquely identifies `SingleItemContainer`.
    unsafe impl DowncastType for SingleItemContainer {
        const TYPE_KEY: DowncastTypeKey =
            DowncastTypeKey::new("steel:test/container/restricted_slot_single_item");
    }

    impl Container for SingleItemContainer {
        fn items(&self) -> &[ItemStack] {
            slice::from_ref(&self.item)
        }

        fn items_mut(&mut self) -> &mut [ItemStack] {
            slice::from_mut(&mut self.item)
        }

        fn get_max_stack_size(&self) -> i32 {
            self.max_stack_size
        }

        fn set_changed(&mut self) {}
    }

    #[test]
    fn max_stack_size_delegates_to_the_container_and_item() {
        init_test_registry();
        let capped = Arc::new(SyncMutex::new(SingleItemContainer {
            item: ItemStack::empty(),
            max_stack_size: 1,
        }));
        let capped_ref = ContainerRef::from(capped);
        let capped_slot = RestrictedSlot::new(capped_ref.clone(), 0, Arc::new(|_, _| true), None);
        let mut capped_guard = ContainerLockGuard::lock_all(&[capped_ref]);
        let capped_remainder = capped_slot.safe_insert(
            &mut capped_guard,
            ItemStack::with_count(&vanilla_items::STONE, 64),
            64,
        );
        assert_eq!(capped_slot.get_item(&capped_guard).count(), 1);
        assert_eq!(capped_remainder.count(), 63);

        let default = SimpleContainer::new(1).into_shared();
        let default_ref = ContainerRef::from(default);
        let default_slot = RestrictedSlot::new(default_ref.clone(), 0, Arc::new(|_, _| true), None);
        let mut default_guard = ContainerLockGuard::lock_all(&[default_ref]);
        let mut stack = ItemStack::new(&vanilla_items::STONE);
        stack.set(MAX_STACK_SIZE, 99);
        stack.set_count(99);
        let default_remainder = default_slot.safe_insert(&mut default_guard, stack, 99);

        assert!(default_remainder.is_empty());
        assert_eq!(default_slot.get_item(&default_guard).count(), 99);
    }
}
