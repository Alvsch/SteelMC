//! Anvil Menus
use std::{
    any::Any,
    mem,
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
};

use steel_registry::{
    REGISTRY, RegistryExt,
    data_components::{
        components::ItemEnchantments,
        vanilla_components::{CUSTOM_NAME, ENCHANTMENTS, REPAIR_COST, STORED_ENCHANTMENTS},
    },
    enchantment::Enchantment,
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
        lock::{ContainerId, ContainerRef},
        menu::{Menu, MenuBehavior},
        simple_menu::SimpleContainer,
        slots::{
            AnvilResultSlot, NormalSlot, Slot, SlotType, SyncResultContainer,
            add_standard_inventory_slots,
        },
    },
    player::Player,
    world::World,
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
/// FIXME: Stopping the server while having items in the anvil deletes them instead of adding them to your inventory
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
    repair_item_count: Arc<AtomicI32>,
    level_cost: Arc<AtomicI32>,
    item_name: SyncMutex<Option<String>>,
}

impl AnvilMenu {
    /// Creates a new Anvil Menu
    #[must_use]
    pub fn new(
        inventory: SyncPlayerInv,
        container_id: u8,
        pos: BlockPos,
        world: &Arc<World>,
    ) -> Self {
        let mut menu_slots = Vec::with_capacity(slots::TOTAL_SLOTS);

        let simple_container = Arc::new(SyncMutex::new(SimpleContainer::new(2)));
        let container_ref = ContainerRef::SimpleContainer(simple_container.clone());
        let repair_item_count = Arc::new(AtomicI32::new(0));
        let level_cost = Arc::new(AtomicI32::new(0));

        let result_container: SyncResultContainer =
            Arc::new(SyncMutex::new(ResultContainer::new()));

        menu_slots.push(SlotType::Normal(NormalSlot::new(container_ref.clone(), 0)));
        menu_slots.push(SlotType::Normal(NormalSlot::new(container_ref.clone(), 1)));
        menu_slots.push(SlotType::AnvilResult(AnvilResultSlot::new(
            simple_container.clone(),
            result_container.clone(),
            repair_item_count.clone(),
            level_cost.clone(),
            pos,
            world.clone(),
        )));

        add_standard_inventory_slots(&mut menu_slots, &inventory);

        let mut behavior = MenuBehavior::new(
            menu_slots,
            container_id,
            Some(vanilla_menu_types::ANVIL),
            vec![
                container_ref.clone(),
                ContainerRef::ResultContainer(result_container.clone()),
                ContainerRef::PlayerInventory(inventory.clone()),
            ],
        );
        behavior.add_data_slot(0);

        Self {
            behavior,
            input_container: simple_container,
            result_container,
            block_pos: pos,
            repair_item_count: repair_item_count.clone(),
            level_cost: level_cost.clone(),
            item_name: SyncMutex::new(None),
        }
    }

    /// Creates the resulting item from the combining and renaming of the two input items
    ///
    ///# Panics
    /// if the input container doesnt have the shape 1x2
    #[tracing::instrument(skip(self, player, guard), level = "info", fields(player = %player.gameprofile.name))]
    #[expect(clippy::too_many_lines, reason = "not my choice its so long .-.")]
    pub fn create_result(&mut self, guard: &mut ContainerLockGuard, player: &Player) {
        let Some([input_container, result_container]) = guard.get_disjoint_mut([
            ContainerId::from_arc(&self.input_container),
            ContainerId::from_arc(&self.result_container),
        ]) else {
            panic!("failed to lock input and/or result containers to create anvil result")
        };

        let [first, second] = input_container.items() else {
            panic!("input_container in anvil menu does not fit expected shape")
        };

        let mut additional_cost = 0_u32;
        let mut rename_cost = 0_i32;
        self.behavior.set_data(0, 0);
        self.level_cost.store(0, Ordering::Relaxed);

        if first.is_empty() || !Self::can_store_enchantments(first) {
            result_container.set_item(0, ItemStack::empty());
            self.behavior.set_data(0, 0);
            self.level_cost.store(0, Ordering::Relaxed);
            return;
        }

        self.repair_item_count.store(0, Ordering::Relaxed);

        let mut result = first.clone();
        let mut enchantments = first.get_enchantments().cloned().unwrap_or_default();
        let prior_repair_cost: i64 = i64::from(*first.get(REPAIR_COST).unwrap_or(&0))
            + i64::from(*second.get(REPAIR_COST).unwrap_or(&0));

        if !second.is_empty() {
            let has_stored_enchantments = second.has(STORED_ENCHANTMENTS);

            if result.is_damageable_item() && first.is_valid_repair_item(second.item) {
                let mut repair_per_unit =
                    result.get_damage_value().min(result.get_max_damage() / 4);
                if repair_per_unit <= 0 {
                    result_container.set_item(0, ItemStack::empty());
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

                self.repair_item_count
                    .store(materials_used, Ordering::Relaxed);
            } else {
                if !has_stored_enchantments
                    && (!result.is(second.item) || !result.is_damageable_item())
                {
                    result_container.set_item(0, ItemStack::empty());
                    self.behavior.set_data(0, 0);
                    self.level_cost.store(0, Ordering::Relaxed);
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
                let sacrifice_enchantments: ItemEnchantments =
                    second.get_enchantments().cloned().unwrap_or_default();
                let mut any_compatible = false;
                let mut any_incompatible = false;

                for (ident, level) in sacrifice_enchantments {
                    let existing_level = enchantments.get_level(&ident);
                    let mut merged_level: u32 = if existing_level == level {
                        level + 1
                    } else {
                        existing_level.max(level)
                    };

                    let enchantment = REGISTRY
                        .enchantments
                        .by_key(&ident)
                        .expect("should exist because we got it from item enchantments");
                    let mut can_apply = enchantment.can_enchant(first.item)
                        || first.is(&vanilla_items::ITEMS.enchanted_book)
                        || player.has_infinite_materials();

                    for (existing_key, _) in first
                        .get_enchantments()
                        .unwrap_or(&ItemEnchantments::empty())
                        .iter()
                    {
                        if *existing_key == enchantment.key {
                            continue;
                        }
                        let Some(existing) = REGISTRY.enchantments.by_key(existing_key) else {
                            continue;
                        };
                        if !Enchantment::are_compatible(enchantment, existing) {
                            can_apply = false;
                            additional_cost += 1;
                        }
                    }

                    if can_apply {
                        any_compatible = true;
                        merged_level = merged_level.min(enchantment.max_level);
                        enchantments.set(ident, merged_level);

                        let mut anvil_cost: i32 = enchantment.anvil_cost;
                        if has_stored_enchantments {
                            anvil_cost = (anvil_cost / 2).max(1);
                        }
                        additional_cost += anvil_cost as u32 * merged_level;

                        if first.count > 1 {
                            additional_cost = 40;
                        }
                    } else {
                        any_incompatible = true;
                    }
                }

                if any_incompatible && !any_compatible {
                    result_container.set_item(0, ItemStack::empty());
                    self.behavior.set_data(0, 0);
                    self.level_cost.store(0, Ordering::Relaxed);
                    return;
                }
            }
        }

        //// --- Renaming ---
        if let Some(name) = self.item_name.lock().as_ref() {
            if name != &first.hover_name().to_string() {
                rename_cost = 1;
                additional_cost += rename_cost as u32;
                result.set(CUSTOM_NAME, TextComponent::from(name.clone()));
            }
        } else if first.has(CUSTOM_NAME) {
            rename_cost = 1;
            additional_cost += rename_cost as u32;
            result.remove(CUSTOM_NAME);
        }

        // --- Final cost calculation ---
        let total_cost = if additional_cost == 0 {
            0
        } else {
            (prior_repair_cost + i64::from(additional_cost)).clamp(0, i64::from(i32::MAX)) as i32
        };
        self.behavior.set_data(0, total_cost as i16);
        self.level_cost.store(total_cost, Ordering::Relaxed);

        if additional_cost == 0 {
            result = ItemStack::empty();
        }

        let only_renaming = rename_cost == additional_cost as i32 && rename_cost > 0;
        if only_renaming && total_cost >= 40 {
            self.behavior.set_data(0, 39);
            self.level_cost.store(39, Ordering::Relaxed);
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
            if rename_cost != additional_cost as i32 || rename_cost == 0 {
                final_repair_cost = Self::calculate_increased_repair_cost(final_repair_cost);
            }
            result.set(REPAIR_COST, final_repair_cost);
            result.set_enchantments(enchantments.iter(), false);
        }

        result_container.set_item(0, result.clone());
    }

    /// Sets the item name of the item
    #[tracing::instrument(skip(self, player) level = "info")]
    pub fn set_item_name(&mut self, name: String, player: &Arc<Player>) {
        let Some(validated_name) = Self::validate_item_name(name) else {
            return;
        };

        {
            let mut guard = self.item_name.lock();
            match &*guard {
                Some(current) if *current == validated_name => return,
                _ => *guard = Some(validated_name),
            }
        }

        {
            let mut guard = self.behavior.lock_all_containers();

            self.create_result(&mut guard, player);
        }
        self.behavior.broadcast_changes(&player.connection);
    }

    fn validate_item_name(name: String) -> Option<String> {
        let filtered = name
            .chars()
            .filter(|char| char != &'§' && char >= &' ' && char != &'\x7F')
            .collect::<String>();
        (filtered.len() <= 50).then_some(filtered)
    }

    fn can_store_enchantments(item_stack: &ItemStack) -> bool {
        item_stack.has(if item_stack.is(&vanilla_items::ITEMS.enchanted_book) {
            STORED_ENCHANTMENTS
        } else {
            ENCHANTMENTS
        })
    }

    const fn calculate_increased_repair_cost(old_repair_cost: i32) -> i32 {
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
        // TODO: this needs to be called before the server closes or the items inside will be deleted
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

    fn slots_changed(
        &mut self,
        guard: &mut ContainerLockGuard,
        _slot_index: usize,
        player: &Player,
    ) {
        self.create_result(guard, player);
    }
}

impl MenuInstance for AnvilMenu {
    fn menu_type(&self) -> MenuTypeRef {
        vanilla_menu_types::ANVIL
    }

    fn container_id(&self) -> u8 {
        self.behavior.container_id
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
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

    fn create(&self, container_id: u8, world: &Arc<World>) -> Box<dyn MenuInstance> {
        Box::new(AnvilMenu::new(
            self.inventory.clone(),
            container_id,
            self.pos,
            world,
        ))
    }
}
