//! Slot abstraction for inventory access.

use std::sync::Arc;

use enum_dispatch::enum_dispatch;
use steel_registry::equipment::EquipmentSlot;
use steel_registry::item_stack::ItemStack;
use steel_utils::locks::Shared;

use crate::inventory::lock::{ContainerId, ContainerLockGuard};
use crate::inventory::slots::armor_slot::ArmorSlot;
use crate::inventory::slots::normal_slot::NormalSlot;
use crate::inventory::slots::restricted_slot::RestrictedSlot;
use crate::inventory::slots::result_slot::ResultSlot;
use crate::player::Player;
use crate::player::player_inventory::PlayerInventory;

/// A view into a single position in a container, accessed via a `ContainerLockGuard`.
#[enum_dispatch]
pub trait Slot {
    /// Returns a reference to the item in this slot.
    fn get_item<'a>(&self, guard: &'a ContainerLockGuard) -> &'a ItemStack;

    /// Returns a mutable reference to the item in this slot.
    fn get_item_mut<'a>(&self, guard: &'a mut ContainerLockGuard) -> &'a mut ItemStack;

    /// Sets the item in this slot.
    fn set_item(&self, guard: &mut ContainerLockGuard, stack: ItemStack);

    /// Sets the item, triggered by a player action. `previous` is the prior item.
    fn set_by_player(
        &self,
        guard: &mut ContainerLockGuard,
        stack: ItemStack,
        _previous: &ItemStack,
    ) {
        self.set_item(guard, stack);
    }

    /// Returns true if this slot has an item.
    fn has_item(&self, guard: &ContainerLockGuard) -> bool {
        !self.get_item(guard).is_empty()
    }

    /// Returns true if the given item can be placed in this slot.
    fn may_place(&self, _stack: &ItemStack) -> bool {
        true
    }

    /// Returns true if items can be picked up from this slot.
    fn may_pickup(&self, _guard: &ContainerLockGuard, _player: &Player) -> bool {
        true
    }

    /// Returns true if partial removal is allowed from this slot.
    fn allow_modification(&self, guard: &ContainerLockGuard, player: &Player) -> bool {
        self.may_pickup(guard, player) && self.may_place(self.get_item(guard))
    }

    /// Returns the maximum stack size for this slot.
    fn get_max_stack_size(&self, guard: &ContainerLockGuard) -> i32;

    /// Returns the max stack size for `stack` here (min of slot and item limits).
    fn get_max_stack_size_for_item(&self, guard: &ContainerLockGuard, stack: &ItemStack) -> i32 {
        self.get_max_stack_size(guard).min(stack.max_stack_size())
    }

    /// Removes up to `amount` items from this slot and returns them.
    fn remove(&self, guard: &mut ContainerLockGuard, amount: i32) -> ItemStack {
        let item = self.get_item_mut(guard);
        if item.is_empty() || amount <= 0 {
            return ItemStack::empty();
        }
        item.split(amount)
    }

    /// Tries to remove items from this slot with validation.
    fn try_remove(
        &self,
        guard: &mut ContainerLockGuard,
        amount: i32,
        max_amount: i32,
        player: &Player,
    ) -> Option<ItemStack> {
        if !self.may_pickup(guard, player) {
            return None;
        }

        let item_count = self.get_item(guard).count();

        if !self.allow_modification(guard, player) && max_amount < item_count {
            return None;
        }

        let take_amount = amount.min(max_amount);
        let result = self.remove(guard, take_amount);
        if result.is_empty() {
            return None;
        }

        if self.get_item(guard).is_empty() {
            self.set_by_player(guard, ItemStack::empty(), &result);
        }

        Some(result)
    }

    /// Called when an item is taken. Returns any remainder that couldn't be placed back.
    fn on_take(
        &self,
        guard: &mut ContainerLockGuard,
        _stack: &ItemStack,
        _player: &Player,
    ) -> Option<ItemStack> {
        self.set_changed(guard);
        None
    }

    /// Takes items with all checks and callbacks. Returns the items taken.
    fn safe_take(
        &self,
        guard: &mut ContainerLockGuard,
        amount: i32,
        max_amount: i32,
        player: &Player,
    ) -> ItemStack {
        if let Some(taken) = self.try_remove(guard, amount, max_amount, player) {
            if let Some(remainder) = self.on_take(guard, &taken, player) {
                player.add_item_or_drop_with_guard(guard, remainder);
            }
            taken
        } else {
            ItemStack::empty()
        }
    }

    /// Inserts up to `amount` items, firing set callbacks.
    fn safe_insert(
        &self,
        guard: &mut ContainerLockGuard,
        mut input: ItemStack,
        amount: i32,
    ) -> ItemStack {
        if input.is_empty() || !self.may_place(&input) {
            return input;
        }

        let slot_stack = self.get_item(guard).clone();
        let transferable = amount
            .min(input.count)
            .min(self.get_max_stack_size_for_item(guard, &input) - slot_stack.count);
        if transferable <= 0 {
            return input;
        }

        if slot_stack.is_empty() {
            self.set_by_player(guard, input.split(transferable), &slot_stack);
        } else if ItemStack::is_same_item_same_components(&slot_stack, &input) {
            input.shrink(transferable);
            let mut new_slot_stack = slot_stack.clone();
            new_slot_stack.grow(transferable);
            self.set_by_player(guard, new_slot_stack, &slot_stack);
        }

        input
    }

    /// Marks the slot's container as changed.
    fn set_changed(&self, guard: &mut ContainerLockGuard);

    /// Returns the container slot index.
    fn get_container_slot(&self) -> usize;

    /// Returns the physical container and slot backing this slot.
    ///
    /// Every container-backed slot returns `Some`, including fake slots.
    /// Slots without stable physical storage return `None`.
    fn container_key(&self) -> Option<(ContainerId, usize)>;

    /// Returns true if this is a fake slot that doesn't persist items.
    fn is_fake(&self) -> bool {
        false
    }
}

/// Forwards `Slot` through `Arc`. Every method is forwarded so inner overrides survive.
impl<T: Slot + ?Sized> Slot for Arc<T> {
    fn get_item<'a>(&self, guard: &'a ContainerLockGuard) -> &'a ItemStack {
        (**self).get_item(guard)
    }

    fn get_item_mut<'a>(&self, guard: &'a mut ContainerLockGuard) -> &'a mut ItemStack {
        (**self).get_item_mut(guard)
    }

    fn set_item(&self, guard: &mut ContainerLockGuard, stack: ItemStack) {
        (**self).set_item(guard, stack);
    }

    fn set_by_player(
        &self,
        guard: &mut ContainerLockGuard,
        stack: ItemStack,
        previous: &ItemStack,
    ) {
        (**self).set_by_player(guard, stack, previous);
    }

    fn has_item(&self, guard: &ContainerLockGuard) -> bool {
        (**self).has_item(guard)
    }

    fn may_place(&self, stack: &ItemStack) -> bool {
        (**self).may_place(stack)
    }

    fn may_pickup(&self, guard: &ContainerLockGuard, player: &Player) -> bool {
        (**self).may_pickup(guard, player)
    }

    fn allow_modification(&self, guard: &ContainerLockGuard, player: &Player) -> bool {
        (**self).allow_modification(guard, player)
    }

    fn get_max_stack_size(&self, guard: &ContainerLockGuard) -> i32 {
        (**self).get_max_stack_size(guard)
    }

    fn get_max_stack_size_for_item(&self, guard: &ContainerLockGuard, stack: &ItemStack) -> i32 {
        (**self).get_max_stack_size_for_item(guard, stack)
    }

    fn remove(&self, guard: &mut ContainerLockGuard, amount: i32) -> ItemStack {
        (**self).remove(guard, amount)
    }

    fn try_remove(
        &self,
        guard: &mut ContainerLockGuard,
        amount: i32,
        max_amount: i32,
        player: &Player,
    ) -> Option<ItemStack> {
        (**self).try_remove(guard, amount, max_amount, player)
    }

    fn on_take(
        &self,
        guard: &mut ContainerLockGuard,
        stack: &ItemStack,
        player: &Player,
    ) -> Option<ItemStack> {
        (**self).on_take(guard, stack, player)
    }

    fn safe_take(
        &self,
        guard: &mut ContainerLockGuard,
        amount: i32,
        max_amount: i32,
        player: &Player,
    ) -> ItemStack {
        (**self).safe_take(guard, amount, max_amount, player)
    }

    fn safe_insert(
        &self,
        guard: &mut ContainerLockGuard,
        input: ItemStack,
        amount: i32,
    ) -> ItemStack {
        (**self).safe_insert(guard, input, amount)
    }

    fn set_changed(&self, guard: &mut ContainerLockGuard) {
        (**self).set_changed(guard);
    }

    fn get_container_slot(&self) -> usize {
        (**self).get_container_slot()
    }

    fn container_key(&self) -> Option<(ContainerId, usize)> {
        (**self).container_key()
    }

    fn is_fake(&self) -> bool {
        (**self).is_fake()
    }
}

/// Enum of all slot types that implement the Slot trait.
#[enum_dispatch(Slot)]
pub enum SlotType {
    /// Normal inventory slot with no restrictions.
    Normal(NormalSlot),
    /// Armor slot that only accepts armor items.
    Armor(ArmorSlot),
    /// Result slot (fake, doesn't persist items).
    Result(ResultSlot),
    /// Slot whose place/pickup rules come from closures.
    Restricted(RestrictedSlot),
    /// Custom implementations by Plugins
    Custom(Arc<dyn Slot + Send + Sync>),
}

/// Adds hotbar slots (player inventory indices 0-8).
pub fn add_hotbar_slots(slots: &mut Vec<SlotType>, inventory: &Shared<PlayerInventory>) {
    for i in 0..9 {
        slots.push(SlotType::Normal(NormalSlot::new(inventory.clone(), i)));
    }
}

/// Adds main inventory slots (player inventory indices 9-35).
pub fn add_inventory_slots(slots: &mut Vec<SlotType>, inventory: &Shared<PlayerInventory>) {
    for i in 9..36 {
        slots.push(SlotType::Normal(NormalSlot::new(inventory.clone(), i)));
    }
}

/// Adds the main inventory (indices 9-35) followed by the hotbar (indices 0-8).
pub fn add_standard_inventory_slots(
    slots: &mut Vec<SlotType>,
    inventory: &Shared<PlayerInventory>,
) {
    add_inventory_slots(slots, inventory);
    add_hotbar_slots(slots, inventory);
}

/// The four armor slots in display order (head, chest, legs, feet).
#[must_use]
pub fn armor_slots(inventory: &Shared<PlayerInventory>) -> [SlotType; 4] {
    [
        (39, EquipmentSlot::Head),
        (38, EquipmentSlot::Chest),
        (37, EquipmentSlot::Legs),
        (36, EquipmentSlot::Feet),
    ]
    .map(|(index, slot)| SlotType::Armor(ArmorSlot::new(inventory.clone(), index, slot)))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use steel_registry::{test_support::init_test_registry, vanilla_items};
    use steel_utils::locks::IntoShared;

    use super::*;
    use crate::inventory::{container::SimpleContainer, lock::ContainerRef};

    struct SafeInsertOverrideSlot {
        base: NormalSlot,
        called: Arc<AtomicBool>,
    }

    impl Slot for SafeInsertOverrideSlot {
        fn get_item<'a>(&self, guard: &'a ContainerLockGuard) -> &'a ItemStack {
            self.base.get_item(guard)
        }

        fn get_item_mut<'a>(&self, guard: &'a mut ContainerLockGuard) -> &'a mut ItemStack {
            self.base.get_item_mut(guard)
        }

        fn set_item(&self, guard: &mut ContainerLockGuard, stack: ItemStack) {
            self.base.set_item(guard, stack);
        }

        fn safe_insert(
            &self,
            _guard: &mut ContainerLockGuard,
            input: ItemStack,
            _amount: i32,
        ) -> ItemStack {
            self.called.store(true, Ordering::Relaxed);
            input
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

    #[test]
    fn custom_slot_safe_insert_override_survives_arc_erasure() {
        init_test_registry();
        let container = SimpleContainer::new(1).into_shared();
        let container_ref = ContainerRef::from(Arc::clone(&container));
        let called = Arc::new(AtomicBool::new(false));
        let slot = SlotType::Custom(Arc::new(SafeInsertOverrideSlot {
            base: NormalSlot::new(container_ref.clone(), 0),
            called: Arc::clone(&called),
        }));
        let mut guard = ContainerLockGuard::lock_all(&[container_ref]);

        let remaining = slot.safe_insert(&mut guard, ItemStack::new(&vanilla_items::STONE), 1);

        assert!(called.load(Ordering::Relaxed));
        assert!(remaining.is(&vanilla_items::STONE));
        assert!(slot.get_item(&guard).is_empty());
    }
}
