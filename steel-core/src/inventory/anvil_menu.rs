//! Anvil Menus
use std::{
    mem,
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
};

use steel_registry::{
    REGISTRY, RegistryExt,
    data_components::vanilla_components::{ENCHANTMENTS, REPAIR_COST, STORED_ENCHANTMENTS},
    item_stack::ItemStack,
    menu_type::MenuTypeRef,
    vanilla_items, vanilla_menu_types,
};
use steel_utils::{BlockPos, locks::SyncMutex, translations};
use text_components::TextComponent;

use crate::{
    inventory::{
        MenuInstance, MenuProvider, SyncPlayerInv,
        container::Container,
        crafting::ResultContainer,
        lock::ContainerRef,
        menu::{Menu, MenuBehavior},
        simple_menu::SimpleContainer,
        slot::{
            AnvilResultSlot, NormalSlot, Slot, SlotType, SyncResultContainer,
            add_standard_inventory_slots,
        },
    },
    player::Player,
};

use super::lock::ContainerLockGuard;

/// Slot indices for the anvil menu.
pub mod slots {
    /// Slot index for the first input item (slot 0).
    pub const FIRST_INPUT_SLOT: usize = 0;
    /// Slot index for the second input item (slot 1).
    pub const SECOND_INPUT_SLOT: usize = 1;
    /// Slot index for the result (slot 2).
    pub const RESULT_SLOT: usize = 2;
    /// Start of main inventory (slot 3).
    pub const INV_SLOT_START: usize = 3;
    /// End of main inventory (slot 30, exclusive).
    pub const INV_SLOT_END: usize = 30;
    /// Start of hotbar (slot 30).
    pub const HOTBAR_SLOT_START: usize = 30;
    /// End of hotbar (slot 39, exclusive).
    pub const HOTBAR_SLOT_END: usize = 39;
    /// Total number of slots in the anvil menu.
    pub const TOTAL_SLOTS: usize = 39;
}

/// Anvil Menu Behavior
pub struct AnvilMenu {
    /// The Menu Behavior
    behavior: MenuBehavior,
    /// The Input Slots
    input_container: Arc<SyncMutex<SimpleContainer>>,
    /// The Result Slot
    result_container: SyncResultContainer,
    /// The Position
    #[expect(dead_code, reason = "not yet implemented")]
    block_pos: BlockPos,
    repair_durability_cost: AtomicI32,
}

impl AnvilMenu {
    /// Creates a new Anvil Menu
    #[must_use]
    pub fn new(inventory: SyncPlayerInv, container_id: u8, pos: BlockPos) -> Self {
        let mut menu_slots = Vec::with_capacity(slots::TOTAL_SLOTS);

        let simple_container = Arc::new(SyncMutex::new(SimpleContainer::new(2)));
        let container_ref = ContainerRef::SimpleContainer(simple_container.clone());

        let result_container: SyncResultContainer =
            Arc::new(SyncMutex::new(ResultContainer::new()));

        menu_slots.push(SlotType::Normal(NormalSlot::new(container_ref.clone(), 0)));
        menu_slots.push(SlotType::Normal(NormalSlot::new(container_ref.clone(), 1)));
        menu_slots.push(SlotType::AnvilResult(AnvilResultSlot::new(
            simple_container.clone(),
            result_container.clone(),
        )));

        add_standard_inventory_slots(&mut menu_slots, &inventory);

        let mut behavior =
            MenuBehavior::new(menu_slots, container_id, Some(vanilla_menu_types::ANVIL));
        behavior.add_data_slot(0);

        Self {
            behavior,
            input_container: simple_container,
            result_container,
            block_pos: pos,
            repair_durability_cost: AtomicI32::new(0),
        }
    }

    fn create_result(&mut self, player: &Player) {
        let mut input_container = self.input_container.lock();
        let [first, second] = input_container
            .items_mut()
            .get_disjoint_mut([0, 1])
            .expect("failed to get");

        let mut additional_cost = 0i32;
        let mut rename_cost = 0i32;
        self.behavior.set_data(0, 1);

        if first.is_empty() || !Self::can_store_enchantments(first) {
            self.result_container.lock().set_item(0, ItemStack::empty());
            self.behavior.set_data(0, 0);
            return;
        }

        self.repair_durability_cost.store(0, Ordering::Relaxed);

        let mut result = first.clone();
        let mut enchantments = first.get_enchantments().cloned().unwrap_or_default();
        let prior_repair_cost: i64 = *first.get(REPAIR_COST).unwrap_or(&0) as i64
            + *second.get(REPAIR_COST).unwrap_or(&0) as i64;

        if !second.is_empty() {
            let has_stored_enchantments = second.has(STORED_ENCHANTMENTS);

            if result.is_damageable_item() {
                // && first.is_valid_repair_item(second) {
                let mut repair_per_unit =
                    result.get_damage_value().min(result.get_max_damage() / 4);
                if repair_per_unit <= 0 {
                    self.result_container.lock().set_item(0, ItemStack::empty());
                    self.behavior.set_data(0, 0);
                    return;
                }

                let mut materials_used = 0;
                while repair_per_unit > 0 && materials_used < second.count {
                    let new_damage = result.get_damage_value() - repair_per_unit;
                    result.set_damage_value(new_damage);
                    additional_cost += 1;
                    materials_used += 1;
                    repair_per_unit = result.get_damage_value().min(result.get_max_damage() / 4);
                }

                self.repair_durability_cost
                    .store(materials_used, Ordering::Relaxed);
            } else {
                if !has_stored_enchantments
                    && (!result.is(second.item) || !result.is_damageable_item())
                {
                    self.result_container.lock().set_item(0, ItemStack::empty());
                    self.behavior.set_data(0, 0);
                    return;
                }

                if result.is_damageable_item() && !has_stored_enchantments {
                    // Combining two of the same item
                    let first_durability = first.get_max_damage() - first.get_damage_value();
                    let second_durability = second.get_max_damage() - second.get_damage_value();
                    let durability_bonus = second_durability + result.get_max_damage() * 12 / 100;
                    let total_durability = first_durability + durability_bonus;
                    let new_damage = (result.get_max_damage() - total_durability).max(0);

                    if new_damage < result.get_damage_value() {
                        result.set_damage_value(new_damage);
                        additional_cost += 2;
                    }
                }

                // Enchantment merging
                let sacrifice_enchantments = second.get_enchantments().cloned().unwrap_or_default();
                let mut any_compatible = false;
                let mut any_incompatible = false;

                for (ident, &level) in sacrifice_enchantments.iter() {
                    let existing_level = enchantments.get_level(ident);
                    let mut merged_level = if existing_level == level {
                        level + 1
                    } else {
                        existing_level.max(level)
                    };

                    let enchantment = REGISTRY
                        .enchantments
                        .by_key(ident)
                        .expect("should exist because we got it from item enchantments");
                    //     let mut can_apply = enchantment.slots
                    //         || first.is(&vanilla_items::ITEMS.enchanted_book)
                    //         || player.has_infinite_materials();

                    //     for existing_holder in enchantments.keys() {
                    //         if existing_holder != enchantment
                    //             && !Enchantment::are_compatible(holder, existing_holder)
                    //         {
                    //             can_apply = false;
                    //             additional_cost += 1;
                    //         }
                    //     }

                    //     if !can_apply {
                    //         any_incompatible = true;
                    //     } else {
                    //         any_compatible = true;
                    //         merged_level = merged_level.min(enchantment.get_max_level());
                    //         enchantments.set(holder, merged_level);

                    //         let mut anvil_cost = enchantment.get_anvil_cost();
                    //         if has_stored_enchantments {
                    //             anvil_cost = (anvil_cost / 2).max(1);
                    //         }
                    //         additional_cost += anvil_cost * merged_level;

                    //         if first.count > 1 {
                    //             additional_cost = 40;
                    //         }
                    //     }
                }

                // if any_incompatible && !any_compatible {
                //     self.result_container.lock().set_item(0, ItemStack::empty());
                //     self.behavior.set_data(0, 0);
                //     return;
                // }
            }
        }

        // TODO: missing packet implementation
        //// --- Renaming ---
        //if let Some(name) = &self.item_name {
        //    if !name.is_empty() {
        //        if name != &first.get_hover_name() {
        //            rename_cost = 1;
        //            additional_cost += rename_cost;
        //            result.set(CUSTOM_NAME, TextComponent::from(name.clone()));
        //        }
        //    }
        //} else if first.has(CUSTOM_NAME) {
        //    rename_cost = 1;
        //    additional_cost += rename_cost;
        //    result.remove(CUSTOM_NAME);
        //}

        // --- Final cost calculation ---
        let total_cost = if additional_cost <= 0 {
            0
        } else {
            (prior_repair_cost + additional_cost as i64).clamp(0, i32::MAX as i64) as i32
        };
        self.behavior.set_data(0, total_cost as i16);

        if additional_cost <= 0 {
            result = ItemStack::empty();
        }

        let only_renaming = rename_cost == additional_cost && rename_cost > 0;
        if only_renaming && total_cost >= 40 {
            self.behavior.set_data(0, 39);
        }

        if total_cost >= 40 && !player.has_infinite_materials() {
            result = ItemStack::empty();
        }

        // --- Write repair cost to result ---
        if !result.is_empty() {
            let second_repair_cost = *second.get(REPAIR_COST).unwrap_or(&0);
            let mut final_repair_cost = *result.get(REPAIR_COST).unwrap_or(&0);
            if final_repair_cost < second_repair_cost {
                final_repair_cost = second_repair_cost;
            }
            if rename_cost != additional_cost || rename_cost == 0 {
                final_repair_cost = Self::calculate_increased_repair_cost(final_repair_cost);
            }
            result.set(REPAIR_COST, final_repair_cost);
            //EnchantmentHelper::set_enchantments(&mut result, enchantments.to_immutable());
        }

        self.result_container.lock().set_item(0, result);
        self.behavior.broadcast_changes(&player.connection);
    }

    fn can_store_enchantments(item_stack: &ItemStack) -> bool {
        item_stack.has(if item_stack.is(&vanilla_items::ITEMS.enchanted_book) {
            STORED_ENCHANTMENTS
        } else {
            ENCHANTMENTS
        })
    }

    fn calculate_increased_repair_cost(old_repair_cost: i32) -> i32 {
        old_repair_cost.saturating_mul(2).saturating_add(1)
    }
}

impl Menu for AnvilMenu {
    fn behavior(&self) -> &MenuBehavior {
        &self.behavior
    }

    fn behavior_mut(&mut self) -> &mut MenuBehavior {
        &mut self.behavior
    }

    fn quick_move_stack(
        &mut self,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        player: &Player,
    ) -> ItemStack {
        if slot_index >= self.behavior.slots.len() {
            return ItemStack::empty();
        }

        let clicked = self.behavior.slots[slot_index].get_item(guard).clone();
        if clicked.is_empty() {
            return ItemStack::empty();
        }

        let mut stack_mut = clicked.clone();

        if slot_index == slots::RESULT_SLOT {
            if !self.behavior.move_item_stack_to(
                guard,
                &mut stack_mut,
                slots::INV_SLOT_START,
                slots::HOTBAR_SLOT_END,
                true,
            ) {
                return ItemStack::empty();
            }
        } else if (0..slots::RESULT_SLOT).contains(&slot_index) {
            if !self.behavior.move_item_stack_to(
                guard,
                &mut stack_mut,
                slots::INV_SLOT_START,
                slots::HOTBAR_SLOT_END,
                false,
            ) {
                return ItemStack::empty();
            }
        } else if (slots::INV_SLOT_START..slots::HOTBAR_SLOT_END).contains(&slot_index) {
            if !self.behavior.move_item_stack_to(
                guard,
                &mut stack_mut,
                0,
                slots::RESULT_SLOT,
                false,
            ) {
                if (slots::INV_SLOT_START..slots::INV_SLOT_END).contains(&slot_index) {
                    if !self.behavior.move_item_stack_to(
                        guard,
                        &mut stack_mut,
                        slots::HOTBAR_SLOT_START,
                        slots::HOTBAR_SLOT_END,
                        false,
                    ) {
                        return ItemStack::empty();
                    }
                } else if !self.behavior.move_item_stack_to(
                    guard,
                    &mut stack_mut,
                    slots::INV_SLOT_START,
                    slots::INV_SLOT_END,
                    false,
                ) {
                    return ItemStack::empty();
                }
            }
        } else {
            return ItemStack::empty();
        }

        if stack_mut.is_empty() {
            self.behavior.slots[slot_index].set_by_player(guard, ItemStack::empty(), &clicked);
        } else {
            self.behavior.slots[slot_index].set_changed(guard);
        }

        if stack_mut.count == clicked.count {
            return ItemStack::empty();
        }

        self.behavior.slots[slot_index].on_take(guard, &stack_mut, player);

        stack_mut
    }

    fn removed(&mut self, player: &Player) {
        let carried = mem::take(&mut self.behavior.carried);

        if !carried.is_empty() {
            player.add_item_or_drop(carried);
        }

        let items = self
            .input_container
            .lock()
            .iter_mut()
            .map(mem::take)
            .filter(|item| !item.is_empty())
            .collect::<Vec<ItemStack>>();

        for item in items {
            player.add_item_or_drop(item);
        }

        self.result_container.lock().set_item(0, ItemStack::empty());
    }
}

impl MenuInstance for AnvilMenu {
    fn menu_type(&self) -> MenuTypeRef {
        vanilla_menu_types::ANVIL
    }

    fn container_id(&self) -> u8 {
        self.behavior.container_id
    }
}

/// Provider for creating a anvil menu.
pub struct AnvilMenuProvider {
    inventory: SyncPlayerInv,
    pos: BlockPos,
}

impl AnvilMenuProvider {
    /// Creates a new anvil menu provider.
    #[must_use]
    pub const fn new(inventory: SyncPlayerInv, pos: BlockPos) -> Self {
        Self { inventory, pos }
    }
}

impl MenuProvider for AnvilMenuProvider {
    fn title(&self) -> TextComponent {
        TextComponent::translated(translations::CONTAINER_REPAIR.msg())
    }

    fn create(&self, container_id: u8) -> Box<dyn MenuInstance> {
        Box::new(AnvilMenu::new(
            self.inventory.clone(),
            container_id,
            self.pos,
        ))
    }
}
