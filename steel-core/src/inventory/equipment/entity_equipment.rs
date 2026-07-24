//! Entity equipment access and owned storage.

use std::mem;

use steel_registry::item_stack::ItemStack;

use super::EquipmentSlot;

/// Equipment access shared by player inventories and owned entity storage.
pub trait EntityEquipment: Send {
    /// Gets a reference to the item in a slot.
    fn get_ref(&self, slot: EquipmentSlot) -> &ItemStack;

    /// Gets a mutable reference to the item in a slot.
    fn get_mut(&mut self, slot: EquipmentSlot) -> &mut ItemStack;

    /// Sets the item in a slot, returning the old item.
    fn set(&mut self, slot: EquipmentSlot, stack: ItemStack) -> ItemStack;

    /// Takes the item from a slot, leaving an empty stack in its place.
    fn take(&mut self, slot: EquipmentSlot) -> ItemStack;

    /// Clears all equipment slots.
    fn clear(&mut self);

    /// Drains equipment slots that changed since the last sync.
    fn drain_dirty_items(&mut self) -> Vec<(EquipmentSlot, ItemStack)>;

    /// Returns non-empty equipment slots for initial spawn synchronization.
    fn non_empty_items(&self) -> Vec<(EquipmentSlot, ItemStack)> {
        EquipmentSlot::ALL
            .into_iter()
            .filter_map(|slot| {
                let item = self.get_ref(slot);
                (!item.is_empty()).then(|| (slot, item.clone()))
            })
            .collect()
    }
}

/// Owned equipment storage used by non-player living entities.
pub struct OwnedEntityEquipment {
    slots: [ItemStack; 8],
    dirty_slots: [bool; 8],
}

impl Default for OwnedEntityEquipment {
    fn default() -> Self {
        Self::new()
    }
}

impl OwnedEntityEquipment {
    /// Creates a new empty equipment storage.
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: [
                ItemStack::empty(),
                ItemStack::empty(),
                ItemStack::empty(),
                ItemStack::empty(),
                ItemStack::empty(),
                ItemStack::empty(),
                ItemStack::empty(),
                ItemStack::empty(),
            ],
            dirty_slots: [false; 8],
        }
    }

    const fn mark_dirty(&mut self, slot: EquipmentSlot) {
        self.dirty_slots[slot.index()] = true;
    }
}

impl EntityEquipment for OwnedEntityEquipment {
    fn get_ref(&self, slot: EquipmentSlot) -> &ItemStack {
        &self.slots[slot.index()]
    }

    fn get_mut(&mut self, slot: EquipmentSlot) -> &mut ItemStack {
        self.mark_dirty(slot);
        &mut self.slots[slot.index()]
    }

    fn set(&mut self, slot: EquipmentSlot, stack: ItemStack) -> ItemStack {
        let old = mem::replace(&mut self.slots[slot.index()], stack);
        if old != self.slots[slot.index()] {
            self.mark_dirty(slot);
        }
        old
    }

    fn take(&mut self, slot: EquipmentSlot) -> ItemStack {
        let old = mem::take(&mut self.slots[slot.index()]);
        if !old.is_empty() {
            self.mark_dirty(slot);
        }
        old
    }

    fn clear(&mut self) {
        for slot in EquipmentSlot::ALL {
            if !self.slots[slot.index()].is_empty() {
                self.slots[slot.index()] = ItemStack::empty();
                self.mark_dirty(slot);
            }
        }
    }

    fn drain_dirty_items(&mut self) -> Vec<(EquipmentSlot, ItemStack)> {
        let mut dirty_items = Vec::new();
        for slot in EquipmentSlot::ALL {
            let index = slot.index();
            if !self.dirty_slots[index] {
                continue;
            }
            self.dirty_slots[index] = false;
            dirty_items.push((slot, self.slots[index].clone()));
        }
        dirty_items
    }
}
