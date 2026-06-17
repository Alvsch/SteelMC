//! Slot abstraction for inventory access.
//!
//! This module provides slot types and helper functions for building menus.
//! The helper functions mirror vanilla Java's `AbstractContainerMenu` methods:
//! - `add_standard_inventory_slots` - adds main inventory (27 slots) + hotbar (9 slots)
//! - `add_inventory_slots` - adds main inventory (27 slots, indices 9-35)
//! - `add_hotbar_slots` - adds hotbar (9 slots, indices 0-8)

use std::sync::Arc;

use enum_dispatch::enum_dispatch;
use steel_registry::item_stack::ItemStack;
use steel_utils::locks::SyncMutex;

use crate::inventory::SyncPlayerInv;
use crate::inventory::crafting::{CraftingContainer, ResultContainer};
use crate::inventory::lock::{ContainerId, ContainerLockGuard};
use crate::inventory::simple_menu::SimpleContainer;
use crate::inventory::slots::armor_slot::ArmorSlot;
use crate::inventory::slots::normal_slot::NormalSlot;
use crate::inventory::slots::{AnvilResultSlot, ProcessingResultSlot};
use crate::player::Player;

/// A synchronized crafting container.
pub type SyncCraftingContainer = Arc<SyncMutex<CraftingContainer>>;

/// A synchronized result container.
pub type SyncResultContainer = Arc<SyncMutex<ResultContainer>>;

/// A synchronized simple container.
pub type SyncSimpleContainer = Arc<SyncMutex<SimpleContainer>>;

/// A slot is a view into a single position in a container.
/// Slots require a `ContainerLockGuard` to access items, ensuring proper locking.
#[enum_dispatch]
pub trait Slot {
    /// Returns a reference to the item in this slot.
    fn get_item<'a>(&self, guard: &'a ContainerLockGuard) -> &'a ItemStack;

    /// Returns a mutable reference to the item in this slot.
    fn get_item_mut<'a>(&self, guard: &'a mut ContainerLockGuard) -> &'a mut ItemStack;

    /// Sets the item in this slot.
    fn set_item(&self, guard: &mut ContainerLockGuard, stack: ItemStack);

    /// Modifies the item in this slot in-place.
    fn modify_item<R>(
        &self,
        guard: &mut ContainerLockGuard,
        f: impl FnOnce(&mut ItemStack) -> R,
    ) -> R {
        let item = self.get_item_mut(guard);
        f(item)
    }

    /// Sets the item in this slot, triggered by a player action.
    ///
    /// This is called when a player directly places or swaps an item in a slot.
    /// The `previous` parameter contains the item that was in the slot before.
    ///
    /// Subclasses can override this to trigger events like equipment change sounds.
    /// The default implementation just calls `set_item`.
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
    ///
    /// Vanilla signature: `mayPickup(Player)`. The `player` is needed because
    /// some slots (e.g. armor with Curse of Binding) conditionally prevent pickup.
    fn may_pickup(&self, _guard: &ContainerLockGuard, _player: &Player) -> bool {
        true
    }

    /// Returns true if partial removal is allowed from this slot.
    ///
    /// For normal slots: `may_pickup() && may_place(current_item)`
    /// For result slots: `false` (must take the full stack)
    fn allow_modification(&self, guard: &ContainerLockGuard, player: &Player) -> bool {
        self.may_pickup(guard, player) && self.may_place(self.get_item(guard))
    }

    /// Returns the maximum stack size for this slot.
    ///
    /// For normal slots, this delegates to the container's max stack size.
    /// For special slots (like armor), this may return a fixed value (e.g., 1).
    fn get_max_stack_size(&self, guard: &ContainerLockGuard) -> i32;

    /// Returns the maximum stack size for a specific item in this slot.
    ///
    /// Takes the minimum of the slot's max stack size and the item's max stack size.
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
    ///
    /// Returns `Some(items)` if removal succeeded, `None` otherwise.
    /// If `allow_modification()` is false and `max_amount < item.count`,
    /// returns `None` (forcing full stack pickup for result slots).
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

        // If modification not allowed (e.g., result slots), must take full stack
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

    /// Called when an item is taken from this slot.
    /// Returns any remainder items that couldn't be placed back (e.g., crafting remainders).
    fn on_take(
        &self,
        guard: &mut ContainerLockGuard,
        _stack: &ItemStack,
        _player: &Player,
    ) -> Option<ItemStack> {
        self.set_changed(guard);
        None
    }

    /// Safely takes items from this slot with all checks and callbacks.
    ///
    /// This combines `try_remove` and `on_take` into a single operation.
    ///
    /// Returns the items taken (empty if nothing could be taken).
    fn safe_take(
        &self,
        guard: &mut ContainerLockGuard,
        amount: i32,
        max_amount: i32,
        player: &Player,
    ) -> ItemStack {
        if let Some(taken) = self.try_remove(guard, amount, max_amount, player) {
            if let Some(remainder) = self.on_take(guard, &taken, player) {
                // Try to add remainder to player inventory, or drop it
                player.add_item_or_drop_with_guard(guard, remainder);
            }
            taken
        } else {
            ItemStack::empty()
        }
    }

    /// Marks the slot's container as changed.
    fn set_changed(&self, guard: &mut ContainerLockGuard);

    /// Returns the container slot index.
    fn get_container_slot(&self) -> usize;

    /// Returns true if this is a "fake" slot (like crafting result).
    /// Fake slots don't persist items and are virtual views.
    fn is_fake(&self) -> bool {
        false
    }
}

/// Enum of all slot types that implement the Slot trait.
#[enum_dispatch(Slot)]
pub enum SlotType {
    /// Normal inventory slot with no restrictions.
    Normal(NormalSlot),
    /// Armor slot that only accepts armor items.
    Armor(ArmorSlot),
    /// Crafting result slot (fake, doesn't persist items).
    ProcessingResultSlot(ProcessingResultSlot),
    /// Anvil result slot (fake, doesn't persist items).
    AnvilResult(AnvilResultSlot),
}

impl SlotType {
    /// Returns the primary container ID and container slot index for this slot.
    /// Used for matching slots between menus when transferring state.
    ///
    /// Only returns `Some` for slots that reference a persistent container
    /// (player inventory). Returns `None` for fake/virtual slots like crafting results.
    #[must_use]
    pub fn container_key(&self) -> Option<(ContainerId, usize)> {
        match self {
            SlotType::Normal(s) => Some((s.container_ref().container_id(), s.get_container_slot())),
            SlotType::Armor(s) => Some((s.container_ref().container_id(), s.get_container_slot())),
            _ => None,
        }
    }
}

// These functions mirror vanilla Java's AbstractContainerMenu methods for
// adding standard inventory slots. They create SlotType vectors that can
// be appended to a menu's slot list.

/// Adds hotbar slots (9 slots) to the given slot vector.
///
/// Maps menu slots to player inventory indices 0-8.
/// This mirrors Java's `AbstractContainerMenu::addInventoryHotbarSlots`.
///
/// # Arguments
/// * `slots` - The slot vector to append to
/// * `inventory` - The player's inventory
pub fn add_hotbar_slots(slots: &mut Vec<SlotType>, inventory: &SyncPlayerInv) {
    for i in 0..9 {
        slots.push(SlotType::Normal(NormalSlot::new(inventory.clone(), i)));
    }
}

/// Adds main inventory slots (27 slots) to the given slot vector.
///
/// Maps menu slots to player inventory indices 9-35.
/// This mirrors Java's `AbstractContainerMenu::addInventoryExtendedSlots`.
///
/// # Arguments
/// * `slots` - The slot vector to append to
/// * `inventory` - The player's inventory
pub fn add_inventory_slots(slots: &mut Vec<SlotType>, inventory: &SyncPlayerInv) {
    for i in 9..36 {
        slots.push(SlotType::Normal(NormalSlot::new(inventory.clone(), i)));
    }
}

/// Adds standard inventory slots (36 slots total) to the given slot vector.
///
/// This adds:
/// - Main inventory: 27 slots (inventory indices 9-35)
/// - Hotbar: 9 slots (inventory indices 0-8)
///
/// This mirrors Java's `AbstractContainerMenu::addStandardInventorySlots`,
/// which calls `addInventoryExtendedSlots` followed by `addInventoryHotbarSlots`.
///
/// # Arguments
/// * `slots` - The slot vector to append to
/// * `inventory` - The player's inventory
pub fn add_standard_inventory_slots(slots: &mut Vec<SlotType>, inventory: &SyncPlayerInv) {
    add_inventory_slots(slots, inventory);
    add_hotbar_slots(slots, inventory);
}
