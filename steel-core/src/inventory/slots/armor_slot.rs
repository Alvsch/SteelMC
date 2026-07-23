use std::sync::Arc;

use steel_registry::{
    equipment::EquipmentSlot, item_stack::ItemStack, vanilla_enchantments::BINDING_CURSE,
};
use steel_utils::locks::Shared;

use crate::{
    inventory::{
        lock::{ContainerId, ContainerLockGuard, ContainerRef},
        slots::slot::Slot,
    },
    player::{Player, player_inventory::PlayerInventory},
};

/// An armor slot that only accepts items equippable in the corresponding slot.
pub struct ArmorSlot {
    container: Shared<PlayerInventory>,
    index: usize,
    slot: EquipmentSlot,
}

impl ArmorSlot {
    /// Creates a new armor slot.
    pub const fn new(
        container: Shared<PlayerInventory>,
        index: usize,
        slot: EquipmentSlot,
    ) -> Self {
        Self {
            container,
            index,
            slot,
        }
    }

    /// Returns the equipment slot this armor slot accepts.
    #[must_use]
    pub const fn equipment_slot(&self) -> EquipmentSlot {
        self.slot
    }

    /// Returns a reference to the container.
    #[must_use]
    pub fn container_ref(&self) -> ContainerRef {
        ContainerRef::from(Arc::clone(&self.container))
    }
}

impl Slot for ArmorSlot {
    fn get_item<'a>(&self, guard: &'a ContainerLockGuard) -> &'a ItemStack {
        guard
            .get(ContainerId::from_arc(&self.container))
            .expect("container not locked")
            .get_item(self.index)
    }

    fn get_item_mut<'a>(&self, guard: &'a mut ContainerLockGuard) -> &'a mut ItemStack {
        guard
            .get_mut(ContainerId::from_arc(&self.container))
            .expect("container not locked")
            .get_item_mut(self.index)
    }

    fn set_item(&self, guard: &mut ContainerLockGuard, stack: ItemStack) {
        guard
            .get_mut(ContainerId::from_arc(&self.container))
            .expect("container not locked")
            .set_item(self.index, stack);
    }

    fn set_by_player(
        &self,
        guard: &mut ContainerLockGuard,
        stack: ItemStack,
        previous: &ItemStack,
    ) {
        // TODO: Call player.onEquipItem(equipmentSlot, previous, stack) here
        let _ = previous;
        self.set_item(guard, stack);
    }

    fn may_place(&self, stack: &ItemStack) -> bool {
        stack.is_equippable_in_slot(self.slot)
    }

    fn may_pickup(&self, guard: &ContainerLockGuard, player: &Player) -> bool {
        let item = self.get_item(guard);
        if !item.is_empty()
            && !player.has_infinite_materials()
            && item.get_enchantment_level(&BINDING_CURSE.key) > 0
        {
            return false;
        }
        true
    }

    fn get_max_stack_size(&self, _guard: &ContainerLockGuard) -> i32 {
        1
    }

    fn set_changed(&self, guard: &mut ContainerLockGuard) {
        guard
            .get_mut(ContainerId::from_arc(&self.container))
            .expect("container not locked")
            .set_changed();
    }

    fn get_container_slot(&self) -> usize {
        self.index
    }
}
