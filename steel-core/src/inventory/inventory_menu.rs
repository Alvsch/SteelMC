//! The player's inventory menu.
//!
//! Slot layout (46 total):
//! - Slot 0: Crafting result
//! - Slots 1-4: 2x2 crafting grid
//! - Slots 5-8: Armor (head, chest, legs, feet)
//! - Slots 9-35: Main inventory (27 slots)
//! - Slots 36-44: Hotbar (9 slots)
//! - Slot 45: Offhand

use std::{mem, sync::Arc};

use steel_registry::item_stack::ItemStack;
use steel_utils::locks::SyncMutex;

use crate::inventory::{
    BuiltMenu, MenuBuilder, MenuLayout, Section, SyncPlayerInv,
    container::Container,
    crafting::{CraftingContainer, ResultContainer},
    equipment::{EquipmentSlot, EquipmentSlotType},
    lock::{ContainerLockGuard, ContainerRef},
    menu::{Menu, MenuBehavior},
    slots::{
        ArmorSlot, CraftingHandler, NormalSlot, ResultHandler,
        slot::{Slot, SlotType, SyncCraftingContainer, SyncResultContainer},
    },
};
use crate::player::Player;

/// The player's inventory menu.
/// This is always open when no other menu is open.
pub struct InventoryMenu {
    behavior: MenuBehavior,
    /// The 2x2 crafting grid container.
    crafting_container: SyncCraftingContainer,
    /// The crafting result container.
    result_container: SyncResultContainer,
    handler: CraftingHandler,
    /// Section ranges + the (empty) route table, kept for the drain-on-close.
    layout: MenuLayout,
    /// The 2x2 crafting grid (slots 1-4).
    grid: Section,
    /// The four armor slots (slots 5-8).
    armor: Section,
    /// Main inventory + hotbar combined (slots 9-44).
    inv: Section,
    /// Main inventory only (slots 9-35).
    main: Section,
    /// Hotbar only (slots 36-44).
    hotbar: Section,
    /// Offhand (slot 45).
    offhand: Section,
}

impl InventoryMenu {
    /// Container ID for the player inventory (always 0).
    pub const CONTAINER_ID: u8 = 0;

    /// Slot index of the (virtual) crafting result.
    const RESULT_SLOT: usize = 0;

    /// Creates a new inventory menu for a player.
    ///
    /// The inventory container should contain:
    /// - Slots 0-35: Main inventory (hotbar 0-8, main 9-35)
    /// - Slots 36-39: Armor (feet, legs, chest, head)
    /// - Slot 40: Offhand
    pub fn new(inventory: SyncPlayerInv) -> Self {
        // Create the crafting containers
        let crafting_container: SyncCraftingContainer =
            Arc::new(SyncMutex::new(CraftingContainer::new(2, 2)));
        let result_container: SyncResultContainer =
            Arc::new(SyncMutex::new(ResultContainer::new()));

        let handler = CraftingHandler::new(crafting_container.clone(), result_container.clone(), 2);

        let mut builder = MenuBuilder::new(None, Self::CONTAINER_ID);

        // Slot 0: crafting result.
        builder.result_slot(
            Arc::new(handler.clone()),
            ContainerRef::ResultContainer(result_container.clone()),
        );
        // Slots 1-4: 2x2 crafting grid.
        let grid = builder.section(
            ContainerRef::CraftingContainer(crafting_container.clone()),
            4,
        );
        // Slots 5-8: armor (head, chest, legs, feet → inventory slots 39, 38, 37, 36).
        let armor_slots = [
            (39, EquipmentSlot::Head),
            (38, EquipmentSlot::Chest),
            (37, EquipmentSlot::Legs),
            (36, EquipmentSlot::Feet),
        ]
        .map(|(offset, eq)| SlotType::Armor(ArmorSlot::new(inventory.clone(), offset, eq)));
        let armor = builder.custom_section(
            armor_slots,
            [ContainerRef::PlayerInventory(inventory.clone())],
        );
        // Slots 9-44: main inventory + hotbar.
        let player = builder.player_inventory(&inventory);
        // Slot 45: offhand (inventory slot 40).
        let offhand = builder.custom_section(
            [SlotType::Normal(NormalSlot::new(inventory.clone(), 40))],
            [ContainerRef::PlayerInventory(inventory.clone())],
        );

        // No routes — quick_move is custom (armor/offhand auto-equip). The grid
        // is drained back to the player on close.
        builder.drain([grid]);

        let BuiltMenu { behavior, layout } = builder.build();

        Self {
            behavior,
            crafting_container,
            result_container,
            handler,
            layout,
            grid,
            armor,
            inv: player.all,
            main: player.main,
            hotbar: player.hotbar,
            offhand,
        }
    }

    /// Returns a reference to the crafting container.
    #[must_use]
    pub const fn crafting_container(&self) -> &SyncCraftingContainer {
        &self.crafting_container
    }

    /// Returns a reference to the result container.
    #[must_use]
    pub const fn result_container(&self) -> &SyncResultContainer {
        &self.result_container
    }

    /// Returns a `ContainerRef` for the crafting container.
    #[must_use]
    pub fn crafting_container_ref(&self) -> ContainerRef {
        ContainerRef::CraftingContainer(Arc::clone(&self.crafting_container))
    }

    /// Returns a `ContainerRef` for the result container.
    #[must_use]
    pub fn result_container_ref(&self) -> ContainerRef {
        ContainerRef::ResultContainer(Arc::clone(&self.result_container))
    }

    /// Helper method to move items between inventory and hotbar.
    fn move_between_inventory_and_hotbar(
        &self,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        stack: &mut ItemStack,
    ) -> bool {
        if self.main.contains(slot_index) {
            // Main inventory -> hotbar.
            self.behavior.move_item_stack_to(
                guard,
                stack,
                self.hotbar.start(),
                self.hotbar.end(),
                false,
            )
        } else if self.hotbar.contains(slot_index) {
            // Hotbar -> main inventory.
            self.behavior.move_item_stack_to(
                guard,
                stack,
                self.main.start(),
                self.main.end(),
                false,
            )
        } else {
            // Offhand (or fallback) -> main inventory + hotbar.
            self.behavior
                .move_item_stack_to(guard, stack, self.inv.start(), self.inv.end(), false)
        }
    }
}

impl Menu for InventoryMenu {
    fn behavior(&self) -> &MenuBehavior {
        &self.behavior
    }

    fn behavior_mut(&mut self) -> &mut MenuBehavior {
        &mut self.behavior
    }

    /// Handles shift-click (quick move) for a slot.
    /// Based on Java's `InventoryMenu::quickMoveStack`.
    ///
    /// Returns the item that was originally in the slot (before any move occurred),
    /// or empty if nothing was moved.
    fn quick_move_stack(
        &mut self,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        player: &Player,
    ) -> ItemStack {
        if slot_index >= self.behavior.slots.len() {
            return ItemStack::empty();
        }

        // Get the current item from the slot
        let stack = self.behavior.slots[slot_index].get_item(guard).clone();
        if stack.is_empty() {
            return ItemStack::empty();
        }
        if slot_index == Self::RESULT_SLOT
            && !self.behavior.slots[slot_index].may_pickup(guard, player)
        {
            return ItemStack::empty();
        }

        let clicked = stack.clone();
        let mut stack_mut = stack;

        // Determine target range based on which slot was clicked
        // This matches the Java implementation in InventoryMenu::quickMoveStack
        let moved = if slot_index == Self::RESULT_SLOT {
            // Crafting result -> inventory, prefer to fill existing stacks first.
            self.behavior.move_item_stack_to(
                guard,
                &mut stack_mut,
                self.inv.start(),
                self.inv.end(),
                true,
            )
        } else if self.grid.contains(slot_index) || self.armor.contains(slot_index) {
            // Crafting grid / armor -> inventory.
            self.behavior.move_item_stack_to(
                guard,
                &mut stack_mut,
                self.inv.start(),
                self.inv.end(),
                false,
            )
        } else {
            // Item is in inventory/hotbar - try to equip it first
            let equippable_slot = clicked.get_equippable_slot();

            // Try to move to armor slot if it's armor
            if let Some(eq_slot) = equippable_slot {
                if eq_slot.slot_type() == EquipmentSlotType::HumanoidArmor {
                    // Armor slots are ordered head, chest, legs, feet.
                    let armor_slot_index = self.armor.start()
                        + match eq_slot {
                            EquipmentSlot::Head => 0,
                            EquipmentSlot::Chest => 1,
                            EquipmentSlot::Legs => 2,
                            EquipmentSlot::Feet => 3,
                            _ => unreachable!(),
                        };

                    // Only try to move if the target armor slot is empty
                    if self.behavior.slots[armor_slot_index].has_item(guard) {
                        // Armor slot occupied, move between inventory/hotbar
                        self.move_between_inventory_and_hotbar(guard, slot_index, &mut stack_mut)
                    } else {
                        self.behavior.move_item_stack_to(
                            guard,
                            &mut stack_mut,
                            armor_slot_index,
                            armor_slot_index + 1,
                            false,
                        )
                    }
                } else if eq_slot == EquipmentSlot::OffHand {
                    // Try to move to offhand slot if empty
                    if self.behavior.slots[self.offhand.start()].has_item(guard) {
                        self.move_between_inventory_and_hotbar(guard, slot_index, &mut stack_mut)
                    } else {
                        self.behavior.move_item_stack_to(
                            guard,
                            &mut stack_mut,
                            self.offhand.start(),
                            self.offhand.end(),
                            false,
                        )
                    }
                } else {
                    self.move_between_inventory_and_hotbar(guard, slot_index, &mut stack_mut)
                }
            } else {
                self.move_between_inventory_and_hotbar(guard, slot_index, &mut stack_mut)
            }
        };

        if !moved {
            return ItemStack::empty();
        }

        // Update the source slot with the remaining items
        self.behavior.slots[slot_index].set_item(guard, stack_mut.clone());

        // Check if unchanged
        if stack_mut.count == clicked.count {
            return ItemStack::empty();
        }

        self.behavior.slots[slot_index].set_changed(guard);

        // Call on_take for the result slot to consume ingredients
        // This must happen after set_item so the slot reflects the new state
        if slot_index == Self::RESULT_SLOT {
            if let Some(remainder) =
                self.behavior.slots[slot_index].on_take(guard, &clicked, player)
            {
                // Try to place crafting remainders (e.g., empty buckets) back in inventory
                player.add_item_or_drop_with_guard(guard, remainder);
            }

            // Java: if (slotIndex == 0) { player.drop(stack, false); }
            // Drop any items from the result slot that couldn't fit in the inventory
            if !stack_mut.is_empty() {
                player.drop_item(stack_mut, false, true);
            }
        }

        clicked
    }

    /// Returns true if the item can be taken from the slot during pickup all.
    /// Prevents taking from the crafting result slot.
    fn can_take_item_for_pick_all(&self, _carried: &ItemStack, slot_index: usize) -> bool {
        slot_index != Self::RESULT_SLOT
    }

    /// Called when the inventory menu is closed.
    /// Returns crafting grid items to the player's inventory; the result is virtual.
    fn removed(&mut self, player: &Player) {
        let carried = mem::take(&mut self.behavior.carried);
        if !carried.is_empty() {
            player.add_item_or_drop(carried);
        }

        self.layout.return_drained_items(&self.behavior, player);
        self.result_container.lock().set_item(0, ItemStack::empty());
    }

    fn slots_changed(
        &mut self,
        guard: &mut ContainerLockGuard,
        _slot_index: usize,
        _player: &Player,
    ) {
        self.handler.update_result(guard);
    }
}
