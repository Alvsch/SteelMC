//! The player's inventory menu.
//!
//! Slot layout (46 total):
//! - Slot 0: Crafting result
//! - Slots 1-4: 2x2 crafting grid
//! - Slots 5-8: Armor (head, chest, legs, feet)
//! - Slots 9-35: Main inventory (27 slots)
//! - Slots 36-44: Hotbar (9 slots)
//! - Slot 45: Offhand

use std::sync::Arc;

use steel_registry::item_stack::ItemStack;
use steel_utils::locks::{IntoShared, Shared};

use crate::inventory::container::{CraftingContainer, ResultContainer};
use crate::inventory::prelude::*;
use crate::inventory::slots::{ArmorSlot, CraftingHandler, NormalSlot, Slot as _};
use crate::player::Player;
use crate::player::player_inventory::PlayerInventory;

/// Container ID for the player inventory (always 0).
pub const INVENTORY_MENU_CONTAINER_ID: u8 = 0;

/// Builds the player's inventory menu — the menu that is always open when no
/// other menu is open.
///
/// The inventory container should contain:
/// - Slots 0-35: Main inventory (hotbar 0-8, main 9-35)
/// - Slots 36-39: Armor (feet, legs, chest, head)
/// - Slot 40: Offhand
#[must_use]
pub fn inventory_menu(inventory: Shared<PlayerInventory>) -> Menu {
    // Create the crafting containers
    let crafting_container = CraftingContainer::new(2, 2).into_shared();
    let result_container = ResultContainer::new().into_shared();

    let handler = CraftingHandler::new(crafting_container.clone(), result_container.clone(), 2);

    let mut builder = MenuBuilder::new(None, INVENTORY_MENU_CONTAINER_ID);

    // Slot 0: crafting result.
    builder.result_slot(
        Arc::new(handler.clone()),
        ContainerRef::from(result_container.clone()),
    );
    // Slots 1-4: 2x2 crafting grid.
    let grid = builder.section(ContainerRef::from(crafting_container), 4);
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
        [ContainerRef::from(inventory.clone())],
    );
    // Slots 9-44: main inventory + hotbar.
    let player = builder.player_inventory(&inventory);
    // Slot 45: offhand (inventory slot 40).
    let offhand = builder.custom_section(
        [SlotType::Normal(NormalSlot::new(inventory.clone(), 40))],
        [ContainerRef::from(inventory)],
    );

    // No routes — quick_move is a custom override (armor/offhand auto-equip).
    // The grid is drained back to the player on close.
    builder.drain([grid]);

    builder.build(InventoryKind {
        result_container,
        handler,
        grid,
        armor,
        inv: player.all(),
        main: player.main(),
        hotbar: player.hotbar(),
        offhand,
    })
}

/// The per-menu part of the player inventory: the recipe handler, the virtual
/// result container, and the section handles its custom shift-click needs.
pub struct InventoryKind {
    /// The crafting result container.
    result_container: Shared<ResultContainer>,
    handler: CraftingHandler,
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

impl InventoryKind {
    /// Slot index of the (virtual) crafting result.
    const RESULT_SLOT: usize = 0;

    /// Helper method to move items between inventory and hotbar.
    fn move_between_inventory_and_hotbar(
        &self,
        behavior: &MenuBehavior,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        stack: &mut ItemStack,
    ) -> bool {
        if self.main.contains(slot_index) {
            // Main inventory -> hotbar.
            behavior.move_item_stack_to(
                guard,
                stack,
                self.hotbar.start(),
                self.hotbar.end(),
                FillDirection::Forward,
            )
        } else if self.hotbar.contains(slot_index) {
            // Hotbar -> main inventory.
            behavior.move_item_stack_to(
                guard,
                stack,
                self.main.start(),
                self.main.end(),
                FillDirection::Forward,
            )
        } else {
            // Offhand (or fallback) -> main inventory + hotbar.
            behavior.move_item_stack_to(
                guard,
                stack,
                self.inv.start(),
                self.inv.end(),
                FillDirection::Forward,
            )
        }
    }
}

impl MenuKind for InventoryKind {
    /// Handles shift-click (quick move) for a slot.
    /// Based on Java's `InventoryMenu::quickMoveStack`.
    ///
    /// Always returns `Some` (the inventory menu fully owns its shift-click,
    /// including armor/offhand auto-equip): the item originally in the slot, or
    /// empty if nothing was moved.
    #[expect(
        clippy::too_many_lines,
        reason = "mirrors Java's InventoryMenu::quickMoveStack branch structure"
    )]
    fn quick_move(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        player: &Player,
    ) -> Option<ItemStack> {
        if slot_index >= behavior.slots.len() {
            return Some(ItemStack::empty());
        }

        // Get the current item from the slot
        let stack = behavior.slots[slot_index].get_item(guard).clone();
        if stack.is_empty() {
            return Some(ItemStack::empty());
        }
        if slot_index == Self::RESULT_SLOT && !behavior.slots[slot_index].may_pickup(guard, player)
        {
            return Some(ItemStack::empty());
        }

        let clicked = stack.clone();
        let mut stack_mut = stack;

        // Determine target range based on which slot was clicked
        // This matches the Java implementation in InventoryMenu::quickMoveStack
        let moved = if slot_index == Self::RESULT_SLOT {
            // Crafting result -> inventory, prefer to fill existing stacks first.
            behavior.move_item_stack_to(
                guard,
                &mut stack_mut,
                self.inv.start(),
                self.inv.end(),
                FillDirection::Backward,
            )
        } else if self.grid.contains(slot_index) || self.armor.contains(slot_index) {
            // Crafting grid / armor -> inventory.
            behavior.move_item_stack_to(
                guard,
                &mut stack_mut,
                self.inv.start(),
                self.inv.end(),
                FillDirection::Forward,
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
                    if behavior.slots[armor_slot_index].has_item(guard) {
                        // Armor slot occupied, move between inventory/hotbar
                        self.move_between_inventory_and_hotbar(
                            behavior,
                            guard,
                            slot_index,
                            &mut stack_mut,
                        )
                    } else {
                        behavior.move_item_stack_to(
                            guard,
                            &mut stack_mut,
                            armor_slot_index,
                            armor_slot_index + 1,
                            FillDirection::Forward,
                        )
                    }
                } else if eq_slot == EquipmentSlot::OffHand {
                    // Try to move to offhand slot if empty
                    if behavior.slots[self.offhand.start()].has_item(guard) {
                        self.move_between_inventory_and_hotbar(
                            behavior,
                            guard,
                            slot_index,
                            &mut stack_mut,
                        )
                    } else {
                        behavior.move_item_stack_to(
                            guard,
                            &mut stack_mut,
                            self.offhand.start(),
                            self.offhand.end(),
                            FillDirection::Forward,
                        )
                    }
                } else {
                    self.move_between_inventory_and_hotbar(
                        behavior,
                        guard,
                        slot_index,
                        &mut stack_mut,
                    )
                }
            } else {
                self.move_between_inventory_and_hotbar(behavior, guard, slot_index, &mut stack_mut)
            }
        };

        if !moved {
            return Some(ItemStack::empty());
        }

        // Update the source slot with the remaining items
        behavior.slots[slot_index].set_item(guard, stack_mut.clone());

        // Check if unchanged
        if stack_mut.count == clicked.count {
            return Some(ItemStack::empty());
        }

        behavior.slots[slot_index].set_changed(guard);

        // Call on_take for the result slot to consume ingredients
        // This must happen after set_item so the slot reflects the new state
        if slot_index == Self::RESULT_SLOT {
            if let Some(remainder) = behavior.slots[slot_index].on_take(guard, &clicked, player) {
                // Try to place crafting remainders (e.g., empty buckets) back in inventory
                player.add_item_or_drop_with_guard(guard, remainder);
            }

            // Java: if (slotIndex == 0) { player.drop(stack, false); }
            // Drop any items from the result slot that couldn't fit in the inventory
            if !stack_mut.is_empty() {
                player.drop_item(stack_mut, false, true);
            }
        }

        Some(clicked)
    }

    /// Returns true if the item can be taken from the slot during pickup all.
    /// Prevents taking from the crafting result slot.
    fn can_take_item_for_pick_all(&self, _carried: &ItemStack, slot_index: usize) -> bool {
        slot_index != Self::RESULT_SLOT
    }

    /// Called when the inventory menu is closed. The grid is drained back to the
    /// player by [`Menu::removed`]; here we just clear the virtual result.
    fn removed(&mut self, _behavior: &mut MenuBehavior, _player: &Player) {
        self.result_container.lock().set_item(0, ItemStack::empty());
    }

    fn slots_changed(
        &mut self,
        _behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
        self.handler.update_result(guard);
    }
}
