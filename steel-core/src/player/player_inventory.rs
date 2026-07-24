//! Player inventory management.

use std::{
    array,
    f32::consts::TAU,
    mem,
    sync::{Arc, LazyLock},
};

use glam::DVec3;
use simdnbt::owned::{NbtList, NbtTag};
use steel_protocol::packets::game::{
    CContainerClose, COpenScreen, SContainerButtonClick, SContainerClick, SContainerClose,
    SContainerSlotStateChanged, SRenameItem, SSetCarriedItem, SSetCreativeModeSlot,
};
use steel_registry::enchantment_effect::EnchantmentEffectComponent;
use steel_registry::item_stack::ItemStack;
use steel_registry::{REGISTRY, RegistryExt, items::ItemRef};
use steel_utils::locks::Shared;
use steel_utils::types::{GameType, InteractionHand};
use steel_utils::{DowncastType, DowncastTypeKey};
use text_components::TextComponent;

use crate::{
    entity::{Entity, RemovalReason, entities::ItemEntity},
    inventory::{
        click::Click,
        container::{Container, CraftingContainer, clear_or_count_matching_stack},
        equipment::{EntityEquipment, EquipmentSlot},
        lock::{ContainerId, ContainerLockGuard},
        menu::{Menu, MenuKindType, kinds::INVENTORY_MENU_CONTAINER_ID},
        slots::{CraftingHandler, Slot},
    },
    player::{Player, connection::NetworkConnection as _},
    world::World,
};

/// Result of swapping a held item with an equipment slot.
#[derive(Debug, PartialEq)]
pub enum EquipmentSwapResult {
    /// The swap succeeded. Contains an overflow stack that should be dropped if non-empty.
    Success(ItemStack),
    /// The swap is blocked by vanilla equipment rules.
    Fail,
}

/// Whether a terminal menu removal completed synchronously.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum MenuRemovalStatus {
    /// Both the base inventory menu and any external menu were removed.
    Complete,
    /// A callback or in-flight open operation owns menu state; removal will
    /// finish when it unwinds.
    Pending,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MenuItemDisposition {
    ReturnToInventory,
    Drop,
}

impl MenuItemDisposition {
    const fn combine(self, other: Self) -> Self {
        if matches!(self, Self::Drop) || matches!(other, Self::Drop) {
            Self::Drop
        } else {
            Self::ReturnToInventory
        }
    }
}

pub(super) struct OpenMenuState {
    menu: Option<Menu>,
    dispatch: Option<OpenMenuDispatch>,
    terminal_removal: Option<TerminalMenuRemoval>,
    active_open_operations: usize,
}

struct OpenMenuDispatch {
    container_id: u8,
    actions: Vec<DeferredMenuAction>,
}

struct TerminalMenuRemoval {
    disposition: MenuItemDisposition,
    main_cleanup_complete: bool,
    pending_cleanup_in_progress: bool,
    pending_menus: Vec<Menu>,
}

enum DeferredMenuAction {
    Close { send_packet: bool },
    Open(Box<PreparedMenu>),
}

struct PreparedMenu {
    title: TextComponent,
    menu: Menu,
}

enum OpenMenuUnavailable {
    Closed,
    Unavailable,
}

impl OpenMenuState {
    pub(super) const fn new() -> Self {
        Self {
            menu: None,
            dispatch: None,
            terminal_removal: None,
            active_open_operations: 0,
        }
    }
}

/// Maps vanilla player-container indices 36-42 to equipment slots.
const fn slot_to_equipment(slot: usize) -> Option<EquipmentSlot> {
    match slot {
        36 => Some(EquipmentSlot::Feet),
        37 => Some(EquipmentSlot::Legs),
        38 => Some(EquipmentSlot::Chest),
        39 => Some(EquipmentSlot::Head),
        40 => Some(EquipmentSlot::OffHand),
        41 => Some(EquipmentSlot::Body),
        42 => Some(EquipmentSlot::Saddle),
        _ => None,
    }
}

const fn equipment_to_slot(slot: EquipmentSlot, selected: u8) -> usize {
    match slot {
        EquipmentSlot::MainHand => selected as usize,
        EquipmentSlot::OffHand => 40,
        EquipmentSlot::Feet => 36,
        EquipmentSlot::Legs => 37,
        EquipmentSlot::Chest => 38,
        EquipmentSlot::Head => 39,
        EquipmentSlot::Body => 41,
        EquipmentSlot::Saddle => 42,
    }
}

/// Player inventory container managing the main inventory and equipment.
///
/// Contains 36 main inventory slots (0-8 hotbar, 9-35 main) plus equipment slots
/// (armor, offhand, etc.) accessed through the Container trait.
pub struct PlayerInventory {
    /// All 43 logical inventory slots in vanilla container order.
    items: [ItemStack; Self::CONTAINER_SIZE],
    /// Currently selected hotbar slot (0-8).
    selected: u8,
    /// Counter incremented on every change.
    times_changed: u32,
}

impl Default for PlayerInventory {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: This key is owned by Steel and uniquely identifies `PlayerInventory`.
unsafe impl DowncastType for PlayerInventory {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:container/player_inventory");
}

impl PlayerInventory {
    /// Number of main inventory slots.
    pub const INVENTORY_SIZE: usize = 36;
    /// Number of logical container slots, including equipment.
    pub const CONTAINER_SIZE: usize = 43;
    /// Number of hotbar slots.
    pub const SELECTION_SIZE: usize = 9;
    /// Slot index for offhand.
    pub const SLOT_OFFHAND: usize = 40;

    /// Creates a new player inventory with empty slots.
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: array::from_fn(|_| ItemStack::empty()),
            selected: 0,
            times_changed: 0,
        }
    }

    /// Returns true if the given slot index is a hotbar slot (0-8).
    #[must_use]
    pub const fn is_hotbar_slot(slot: usize) -> bool {
        slot < Self::SELECTION_SIZE
    }

    /// Returns the currently selected hotbar slot (0-8).
    #[must_use]
    pub const fn get_selected_slot(&self) -> u8 {
        self.selected
    }

    /// Serializes the main inventory with vanilla's `ItemStackWithSlot` shape.
    #[must_use]
    pub(crate) fn to_vanilla_inventory_nbt(&self) -> NbtList {
        let items = self.items[..Self::INVENTORY_SIZE]
            .iter()
            .enumerate()
            .filter_map(|(slot, item)| {
                if item.is_empty() {
                    return None;
                }
                let NbtTag::Compound(mut nbt) = item.to_nbt_tag_ref() else {
                    return None;
                };
                nbt.insert("Slot", NbtTag::Byte(slot as i8));
                Some(nbt)
            })
            .collect();
        NbtList::Compound(items)
    }

    /// Sets the selected hotbar slot.
    ///
    /// # Panics
    ///
    /// Panics if the slot is not a valid hotbar slot (must be 0-8).
    pub fn set_selected_slot(&mut self, slot: u8) {
        if Self::is_hotbar_slot(slot as usize) {
            if self.selected != slot {
                self.selected = slot;
            }
        } else {
            panic!("Invalid hotbar slot: {slot}");
        }
    }

    /// Sets the selected hotbar slot from the signed protocol field.
    ///
    /// Returns an error when the packet value is outside the vanilla hotbar
    /// range instead of wrapping or panicking.
    pub fn try_set_selected_slot_from_packet(
        &mut self,
        slot: i16,
    ) -> Result<(), InvalidHotbarSlot> {
        let Ok(slot) = u8::try_from(slot) else {
            return Err(InvalidHotbarSlot);
        };
        if !Self::is_hotbar_slot(slot as usize) {
            return Err(InvalidHotbarSlot);
        }

        self.set_selected_slot(slot);
        Ok(())
    }

    /// Executes a function with a reference to the currently selected item.
    pub fn with_selected_item<R>(&self, f: impl FnOnce(&ItemStack) -> R) -> R {
        f(&self.items[self.selected as usize])
    }

    /// Returns a mutable reference to the currently selected item (main hand).
    #[must_use]
    pub const fn get_selected_item(&self) -> &ItemStack {
        &self.items[self.selected as usize]
    }

    /// Returns the currently selected item (main hand).
    pub fn get_selected_item_mut(&mut self) -> &mut ItemStack {
        EntityEquipment::get_mut(self, EquipmentSlot::MainHand)
    }

    /// Sets the currently selected item (main hand).
    pub fn set_selected_item(&mut self, item: ItemStack) {
        let _ = EntityEquipment::set(self, EquipmentSlot::MainHand, item);
    }

    /// Returns the offhand item.
    #[must_use]
    pub fn get_offhand_item(&self) -> &ItemStack {
        EntityEquipment::get_ref(self, EquipmentSlot::OffHand)
    }

    /// Returns a mutable reference to the offhand item.
    pub fn get_offhand_item_mut(&mut self) -> &mut ItemStack {
        EntityEquipment::get_mut(self, EquipmentSlot::OffHand)
    }

    /// Sets the offhand item.
    pub fn set_offhand_item(&mut self, item: ItemStack) {
        let _ = EntityEquipment::set(self, EquipmentSlot::OffHand, item);
    }

    /// Executes a function with a mutable reference to the currently selected item.
    pub fn with_selected_item_mut<R>(&mut self, f: impl FnOnce(&mut ItemStack) -> R) -> R {
        self.with_equipment_item_mut(EquipmentSlot::MainHand, f)
    }

    pub(super) fn with_equipment_item_mut<R>(
        &mut self,
        slot: EquipmentSlot,
        f: impl FnOnce(&mut ItemStack) -> R,
    ) -> R {
        let inventory_index = self.equipment_slot_index(slot);
        let previous = self.items[inventory_index].clone();
        let result = f(&mut self.items[inventory_index]);
        if !ItemStack::matches(&self.items[inventory_index], &previous) {
            Container::set_changed(self);
        }
        result
    }

    /// Returns the number of times this inventory has been modified.
    #[must_use]
    pub const fn get_times_changed(&self) -> u32 {
        self.times_changed
    }

    /// Returns the non-equipment items (main 36 slots).
    #[must_use]
    pub fn get_items(&self) -> &[ItemStack; Self::INVENTORY_SIZE] {
        let Some(items) = self.items.first_chunk::<{ Self::INVENTORY_SIZE }>() else {
            unreachable!("the player inventory always contains its 36 main slots");
        };
        items
    }

    /// Finds the first empty slot in the inventory, or -1 if full.
    #[must_use]
    pub fn get_free_slot(&self) -> i32 {
        for i in 0..Self::INVENTORY_SIZE {
            if self.items[i].is_empty() {
                return i as i32;
            }
        }
        -1
    }

    /// Finds a slot containing an item matching the given stack (same item type).
    /// Returns -1 if not found.
    #[must_use]
    pub fn find_slot_matching_item(&self, stack: &ItemStack) -> i32 {
        for i in 0..Self::INVENTORY_SIZE {
            if !self.items[i].is_empty() && ItemStack::is_same_item(&self.items[i], stack) {
                return i as i32;
            }
        }
        -1
    }

    /// Swaps items between selected hotbar slot and the given slot.
    /// Used for pick block when item is in main inventory but not hotbar.
    pub fn pick_slot(&mut self, slot: i32) {
        let slot = slot as usize;
        if slot >= Self::INVENTORY_SIZE {
            return;
        }
        let selected = self.selected as usize;
        self.items.swap(selected, slot);
        self.set_changed();
    }

    /// Adds an item to the hotbar (for creative pick block) and selects it.
    /// Returns true if successful.
    pub fn add_and_pick_item(&mut self, stack: ItemStack) -> bool {
        // Find first empty hotbar slot
        for i in 0..Self::SELECTION_SIZE {
            if self.items[i].is_empty() {
                self.items[i] = stack;
                self.selected = i as u8;
                self.set_changed();
                return true;
            }
        }
        // No empty slot, replace current slot
        self.items[self.selected as usize] = stack;
        self.set_changed();
        true
    }

    /// Gets the item in the specified hand.
    #[must_use]
    pub fn get_item_in_hand(&self, hand: InteractionHand) -> &ItemStack {
        match hand {
            InteractionHand::MainHand => self.get_selected_item(),
            InteractionHand::OffHand => self.get_offhand_item(),
        }
    }

    /// Gets the item in the specified hand.
    #[must_use]
    pub fn get_item_in_hand_mut(&mut self, hand: InteractionHand) -> &mut ItemStack {
        match hand {
            InteractionHand::MainHand => self.get_selected_item_mut(),
            InteractionHand::OffHand => self.get_offhand_item_mut(),
        }
    }

    /// Sets the item in the specified hand.
    pub fn set_item_in_hand(&mut self, hand: InteractionHand, item: ItemStack) {
        match hand {
            InteractionHand::MainHand => self.set_selected_item(item),
            InteractionHand::OffHand => self.set_offhand_item(item),
        }
    }

    /// Shrinks the item in the specified hand and records inventory/equipment changes.
    pub fn shrink_item_in_hand(&mut self, hand: InteractionHand, amount: i32) {
        if amount <= 0 || self.get_item_in_hand(hand).is_empty() {
            return;
        }

        self.get_item_in_hand_mut(hand).shrink(amount);
        self.set_changed();
    }

    /// Splits items from the specified hand and records inventory/equipment changes.
    pub fn split_item_in_hand(&mut self, hand: InteractionHand, amount: i32) -> ItemStack {
        if amount <= 0 || self.get_item_in_hand(hand).is_empty() {
            return ItemStack::empty();
        }

        let result = self.get_item_in_hand_mut(hand).split(amount);
        self.set_changed();
        result
    }

    /// Damages the held item and records inventory/equipment changes.
    pub fn hurt_item_in_hand(
        &mut self,
        hand: InteractionHand,
        amount: i32,
        has_infinite_materials: bool,
    ) {
        if amount <= 0 || self.get_item_in_hand(hand).is_empty() {
            return;
        }

        let changed = {
            let item = self.get_item_in_hand_mut(hand);
            let previous_item = item.item();
            let previous_count = item.count();
            let previous_damage = item.get_damage_value();

            let _ = item.hurt_and_break(amount, has_infinite_materials);

            item.item() != previous_item
                || item.count() != previous_count
                || item.get_damage_value() != previous_damage
        };

        if changed {
            self.set_changed();
        }
    }

    /// Mutates the held item and records inventory/equipment changes if its stack state changed.
    pub fn mutate_item_in_hand<R>(
        &mut self,
        hand: InteractionHand,
        f: impl FnOnce(&mut ItemStack) -> R,
    ) -> R {
        let slot = match hand {
            InteractionHand::MainHand => EquipmentSlot::MainHand,
            InteractionHand::OffHand => EquipmentSlot::OffHand,
        };
        self.with_equipment_item_mut(slot, f)
    }

    /// Damages the held item and converts it to `replacement_item` if it breaks.
    ///
    /// Mirrors vanilla `ItemStack.hurtAndConvertOnBreak` for hand-held player items.
    pub fn hurt_and_convert_item_in_hand_on_break(
        &mut self,
        hand: InteractionHand,
        amount: i32,
        replacement_item: ItemRef,
        has_infinite_materials: bool,
    ) {
        if amount <= 0 || self.get_item_in_hand(hand).is_empty() {
            return;
        }

        let changed = {
            let item = self.get_item_in_hand_mut(hand);
            let previous_item = item.item();
            let previous_count = item.count();
            let previous_damage = item.get_damage_value();

            if item.hurt_and_break(amount, has_infinite_materials) && item.is_empty() {
                item.set_item(&replacement_item.key);
                item.set_count(1);
                if item.is_damageable_item() {
                    item.set_damage_value(0);
                }
            }

            item.item() != previous_item
                || item.count() != previous_count
                || item.get_damage_value() != previous_damage
        };

        if changed {
            self.set_changed();
        }
    }

    /// Swaps the selected main-hand item with the offhand item.
    ///
    /// Returns true when the visible hand contents changed.
    pub fn swap_hands(&mut self) -> bool {
        if ItemStack::matches(self.get_selected_item(), self.get_offhand_item()) {
            return false;
        }

        let main_hand = EntityEquipment::take(self, EquipmentSlot::MainHand);
        let offhand = EntityEquipment::take(self, EquipmentSlot::OffHand);
        let _ = EntityEquipment::set(self, EquipmentSlot::MainHand, offhand);
        let _ = EntityEquipment::set(self, EquipmentSlot::OffHand, main_hand);
        true
    }

    /// Attempts to equip the held item into the target equipment slot.
    pub fn try_swap_with_equipment_slot(
        &mut self,
        hand: InteractionHand,
        slot: EquipmentSlot,
        has_infinite_materials: bool,
    ) -> EquipmentSwapResult {
        let in_hand = self.get_item_in_hand(hand);
        if in_hand.is_empty() {
            return EquipmentSwapResult::Fail;
        }

        let in_equipment_slot = EntityEquipment::get_ref(self, slot);
        if ItemStack::is_same_item_same_components(in_hand, in_equipment_slot) {
            return EquipmentSwapResult::Fail;
        }

        if !has_infinite_materials
            && in_equipment_slot
                .has_enchantment_effect(EnchantmentEffectComponent::PreventArmorChange)
        {
            return EquipmentSwapResult::Fail;
        }

        if in_hand.count() <= 1 {
            self.swap_single_item_with_equipment_slot(hand, slot, has_infinite_materials);
            return EquipmentSwapResult::Success(ItemStack::empty());
        }

        let to_equip = in_hand.copy_with_count(1);
        if !has_infinite_materials {
            self.get_item_in_hand_mut(hand).shrink(1);
        }
        let mut overflow = EntityEquipment::set(self, slot, to_equip);
        if !overflow.is_empty() && self.add(&mut overflow) {
            overflow = ItemStack::empty();
        }

        EquipmentSwapResult::Success(overflow)
    }

    /// Repairs a random damaged equipped item with `REPAIR_WITH_XP`, returning leftover XP.
    pub fn repair_random_equipped_item_with_xp(&mut self, amount: i32) -> i32 {
        let mut remaining = amount;

        loop {
            let candidates = self.repair_with_xp_candidate_slots();
            if candidates.is_empty() {
                return remaining;
            }

            let selected = rand::random_range(0..candidates.len());
            let slot = candidates[selected];
            let item = EntityEquipment::get_mut(self, slot);
            let to_repair = item
                .apply_unconditional_enchantment_value_effects(
                    EnchantmentEffectComponent::RepairWithXp,
                    remaining as f32,
                )
                .max(0.0) as i32;
            if to_repair <= 0 {
                return 0;
            }

            let damage = item.get_damage_value();
            let repair = to_repair.min(damage);
            if repair <= 0 {
                return 0;
            }

            item.set_damage_value(damage - repair);
            self.set_changed();

            remaining -= repair * remaining / to_repair;
            if remaining <= 0 {
                return 0;
            }
        }
    }

    fn swap_single_item_with_equipment_slot(
        &mut self,
        hand: InteractionHand,
        slot: EquipmentSlot,
        has_infinite_materials: bool,
    ) {
        if has_infinite_materials {
            let held = self
                .get_item_in_hand(hand)
                .copy_with_count(self.get_item_in_hand(hand).count());
            let previous = EntityEquipment::set(self, slot, held);
            if !previous.is_empty() {
                self.set_item_in_hand(hand, previous);
            }
            return;
        }

        let held = self.take_item_in_hand(hand);
        let previous = EntityEquipment::set(self, slot, held);
        self.set_item_in_hand(hand, previous);
    }

    fn repair_with_xp_candidate_slots(&self) -> Vec<EquipmentSlot> {
        let mut slots = Vec::new();
        for slot in EquipmentSlot::ALL {
            let item = EntityEquipment::get_ref(self, slot);
            if !item.is_damaged() {
                continue;
            }

            let Some(enchantments) = item.get_enchantments() else {
                continue;
            };
            for (key, level) in enchantments.iter() {
                if *level == 0 {
                    continue;
                }
                let Some(enchantment) = REGISTRY.enchantments.by_key(key) else {
                    continue;
                };
                if enchantment
                    .effects
                    .has(EnchantmentEffectComponent::RepairWithXp)
                    && enchantment.matching_slot(slot)
                {
                    slots.push(slot);
                }
            }
        }
        slots
    }

    fn take_item_in_hand(&mut self, hand: InteractionHand) -> ItemStack {
        match hand {
            InteractionHand::MainHand => EntityEquipment::take(self, EquipmentSlot::MainHand),
            InteractionHand::OffHand => EntityEquipment::take(self, EquipmentSlot::OffHand),
        }
    }
}

impl Player {
    fn take_open_menu_for_callback(
        &self,
        expected_container_id: Option<i32>,
    ) -> Result<Menu, OpenMenuUnavailable> {
        let mut open_menu = self.open_menu.lock();
        if open_menu.dispatch.is_some() {
            return Err(OpenMenuUnavailable::Unavailable);
        }

        let Some(menu) = open_menu.menu.as_ref() else {
            return Err(OpenMenuUnavailable::Closed);
        };
        if expected_container_id.is_some_and(|expected| i32::from(menu.container_id()) != expected)
        {
            return Err(OpenMenuUnavailable::Unavailable);
        }

        let container_id = menu.container_id();
        let Some(menu) = open_menu.menu.take() else {
            return Err(OpenMenuUnavailable::Unavailable);
        };
        open_menu.dispatch = Some(OpenMenuDispatch {
            container_id,
            actions: Vec::new(),
        });
        Ok(menu)
    }

    fn finish_open_menu_callback(&self, menu: Menu) {
        let actions = {
            let mut open_menu = self.open_menu.lock();
            let Some(dispatch) = open_menu.dispatch.take() else {
                open_menu.menu = Some(menu);
                return;
            };
            if let Some(terminal_removal) = open_menu.terminal_removal.as_mut() {
                Self::queue_deferred_menus(terminal_removal, dispatch.actions);
                drop(open_menu);
                self.finish_terminal_menu_main_cleanup(Some(menu));
                return;
            }
            open_menu.menu = Some(menu);
            open_menu.active_open_operations += 1;
            dispatch.actions
        };

        self.run_deferred_menu_actions(actions);
    }

    fn finish_open_menu_removal(&self) {
        let actions = {
            let mut open_menu = self.open_menu.lock();
            let Some(dispatch) = open_menu.dispatch.take() else {
                return;
            };
            if let Some(terminal_removal) = open_menu.terminal_removal.as_mut() {
                Self::queue_deferred_menus(terminal_removal, dispatch.actions);
                drop(open_menu);
                self.finish_terminal_menu_main_cleanup(None);
                return;
            }
            open_menu.active_open_operations += 1;
            dispatch.actions
        };

        self.run_deferred_menu_actions(actions);
    }

    fn run_deferred_menu_actions(&self, actions: Vec<DeferredMenuAction>) {
        for action in actions {
            match action {
                DeferredMenuAction::Close { send_packet } => {
                    if send_packet {
                        self.close_container();
                    } else {
                        self.do_close_container();
                    }
                }
                DeferredMenuAction::Open(prepared) => {
                    let PreparedMenu { title, menu } = *prepared;
                    self.open_prepared_menu(title, menu);
                }
            }
        }

        self.finish_menu_open_operation();
    }

    fn queue_deferred_menus(
        terminal_removal: &mut TerminalMenuRemoval,
        actions: Vec<DeferredMenuAction>,
    ) {
        terminal_removal
            .pending_menus
            .extend(actions.into_iter().filter_map(|action| match action {
                DeferredMenuAction::Open(prepared) => Some(prepared.menu),
                DeferredMenuAction::Close { .. } => None,
            }));
    }

    fn begin_menu_open_operation(&self) -> bool {
        let mut open_menu = self.open_menu.lock();
        if open_menu.terminal_removal.is_some() {
            return false;
        }
        open_menu.active_open_operations += 1;
        true
    }

    fn finish_menu_open_operation(&self) {
        {
            let mut open_menu = self.open_menu.lock();
            debug_assert!(open_menu.active_open_operations > 0);
            open_menu.active_open_operations -= 1;
        }
        self.try_finish_terminal_menu_removal();
    }

    /// Attempts to pick up nearby item entities.
    ///
    /// Mirrors vanilla's `Player.aiStep()` item pickup logic:
    /// - Calculates pickup area as bounding box inflated by (1.0, 0.5, 1.0)
    /// - Calls `playerTouch()` on each entity in range
    pub(super) fn touch_nearby_items(&self) {
        if self.game_mode() == GameType::Spectator {
            return;
        }

        let pickup_area = self.bounding_box().inflate_xyz(1.0, 0.5, 1.0);
        let world = self.get_world();
        let entities = world.get_entities_in_aabb(&pickup_area);

        let Some(player_arc) = world.players.get_by_entity_id(self.id()) else {
            return;
        };

        for entity in entities {
            if entity.id() == self.id() || entity.is_removed() {
                continue;
            }

            entity.player_touch(&player_arc);
        }
    }

    /// Handles a container button click packet (e.g., enchanting table buttons).
    pub fn handle_container_button_click(&self, packet: SContainerButtonClick) {
        log::debug!(
            "Player {} clicked button {} in container {}",
            self.gameprofile.name,
            packet.button_id,
            packet.container_id
        );
        // TODO: Implement container button click handling
        // This is used for things like:
        // - Enchanting table level selection
        // - Stonecutter recipe selection
        // - Loom pattern selection
        // - Lectern page turning
    }

    /// Handles a container click packet (slot interaction).
    pub fn handle_container_click(&self, packet: SContainerClick) {
        match self.take_open_menu_for_callback(Some(packet.container_id)) {
            Ok(mut menu) => {
                self.process_container_click(&mut menu, packet);
                self.finish_open_menu_callback(menu);
            }
            Err(OpenMenuUnavailable::Closed) => {
                let mut menu = self.inventory_menu.lock();
                if i32::from(menu.behavior().container_id()) == packet.container_id {
                    self.process_container_click(&mut menu, packet);
                }
            }
            Err(OpenMenuUnavailable::Unavailable) => {}
        }
    }

    /// Processes a container click on any menu implementing the Menu trait.
    ///
    /// This is the common implementation shared between inventory menu and
    /// external menus (crafting table, chest, etc.).
    fn process_container_click(&self, menu: &mut Menu, packet: SContainerClick) {
        if self.game_mode() == GameType::Spectator {
            menu.behavior_mut()
                .send_all_data_to_remote(&self.connection);
            return;
        }

        if !menu.still_valid(self) {
            log::debug!(
                "Player {} interacted with invalid menu",
                self.gameprofile.name
            );
            return;
        }

        // Parse and validate the raw click fields once. A malformed click
        // (out-of-range slot, bad button, invalid drag encoding — including
        // the -1 "no slot" clicks Java accepts) is not applied, but the state
        // sync below still runs so the client's prediction gets corrected.
        let click = Click::parse(
            packet.slot_num,
            packet.button_num,
            packet.click_type,
            menu.behavior().slot_count(),
        );
        if click.is_none() {
            log::debug!(
                "Player {} sent malformed container click (slot {}, button {}, {:?})",
                self.gameprofile.name,
                packet.slot_num,
                packet.button_num,
                packet.click_type
            );
        }

        let full_resync_needed = packet.state_id as u32 != menu.behavior().state_id();

        menu.behavior_mut().suppress_remote_updates();

        if let Some(click) = click {
            menu.clicked(click, self);
        }

        for (slot, hash) in packet.changed_slots {
            let slot = slot as usize;
            // Result/fake slots are server-authoritative (their contents are
            // recomputed from a recipe). Don't let the client's prediction set
            // our view of what it knows, or `broadcast_changes` will think the
            // client already has the freshly-crafted result and skip syncing it
            // — leaving the slot blank until the next click forces a resend.
            if menu.behavior().slots().get(slot).is_some_and(Slot::is_fake) {
                menu.behavior_mut().mark_remote_slot_unknown(slot);
                continue;
            }
            menu.behavior_mut().set_remote_slot(slot, hash);
        }

        menu.behavior_mut().set_remote_carried(packet.carried_item);
        menu.behavior_mut().resume_remote_updates();

        if full_resync_needed {
            menu.behavior_mut()
                .send_all_data_to_remote(&self.connection);
        } else {
            menu.behavior_mut().broadcast_changes(&self.connection);
        }
    }

    /// Handles a container close packet.
    ///
    /// Based on Java's `ServerGamePacketListenerImpl::handleContainerClose`.
    pub fn handle_container_close(&self, packet: SContainerClose) {
        log::debug!(
            "Player {} closed container {}",
            self.gameprofile.name,
            packet.container_id
        );

        let open_menu = self.open_menu.lock();
        let closes_open_menu = open_menu
            .menu
            .as_ref()
            .is_some_and(|menu| i32::from(menu.container_id()) == packet.container_id)
            || open_menu
                .dispatch
                .as_ref()
                .is_some_and(|dispatch| i32::from(dispatch.container_id) == packet.container_id);
        drop(open_menu);

        if closes_open_menu {
            self.do_close_container();
            return;
        }

        if packet.container_id == i32::from(INVENTORY_MENU_CONTAINER_ID) {
            let mut menu = self.inventory_menu.lock();
            menu.removed(self);
        }
    }

    /// Handles an anvil rename packet.
    pub fn handle_rename_item(self: &Arc<Self>, packet: SRenameItem) {
        match self.take_open_menu_for_callback(None) {
            Ok(mut menu) => {
                if menu.still_valid(self) {
                    menu.set_item_name(packet.name, self);
                }
                self.finish_open_menu_callback(menu);
            }
            Err(OpenMenuUnavailable::Closed) => {
                log::debug!("rename item without an open menu");
            }
            Err(OpenMenuUnavailable::Unavailable) => {}
        }
    }

    /// Handles a container slot state changed packet (e.g., crafter slot toggle).
    pub fn handle_container_slot_state_changed(&self, packet: SContainerSlotStateChanged) {
        log::debug!(
            "Player {} changed slot {} state to {} in container {}",
            self.gameprofile.name,
            packet.slot_id,
            packet.new_state,
            packet.container_id
        );
        // TODO: Implement slot state change handling
        // This is used for the crafter block to enable/disable slots
    }

    /// Handles a creative mode slot set packet.
    pub fn handle_set_creative_mode_slot(&self, packet: SSetCreativeModeSlot) {
        if self.game_mode() != GameType::Creative {
            return;
        }

        let drop = packet.slot_num < 0;
        let item_stack = packet.item_stack;

        let valid_slot = packet.slot_num >= 1 && packet.slot_num <= 45;
        let valid_data = item_stack.is_empty() || item_stack.count <= item_stack.max_stack_size();

        if valid_slot && valid_data {
            let mut menu = self.inventory_menu.lock();
            let slot_index = packet.slot_num as usize;

            {
                let mut guard = menu.behavior().lock_all_containers();
                if let Some(slot) = menu.behavior().slots().get(slot_index) {
                    let previous = slot.get_item(&guard).clone();
                    slot.set_by_player(&mut guard, item_stack.clone(), &previous);
                }
            }
            if (1..=4).contains(&slot_index) {
                menu.update_crafting_result();
            }
            menu.behavior_mut()
                .set_remote_slot_known(slot_index, &item_stack);
            menu.behavior_mut().broadcast_changes(&self.connection);
        } else if drop && valid_data {
            // TODO: Implement drop spam throttling
            // For now, just drop the item
            if !item_stack.is_empty() {
                // TODO: Actually drop the item into the world
                log::debug!(
                    "Player {} would drop {:?} in creative mode",
                    self.gameprofile.name,
                    item_stack
                );
            }
        }
    }

    /// Sets selected slot
    pub fn handle_set_carried_item(&self, packet: SSetCarriedItem) {
        if self
            .inventory
            .lock()
            .try_set_selected_slot_from_packet(packet.slot)
            .is_err()
        {
            log::warn!(
                "{} tried to set an invalid carried item",
                self.gameprofile.name
            );
        }
    }

    /// Sends all inventory slots to the client (full sync).
    /// This should be called when the player first joins.
    pub fn send_inventory_to_remote(&self) {
        self.inventory_menu
            .lock()
            .behavior_mut()
            .send_all_data_to_remote(&self.connection);
    }

    /// Generates the next container ID (1-100, wrapping around).
    ///
    /// Based on Java's `ServerPlayer::nextContainerCounter`.
    fn next_container_counter(&self) -> u8 {
        self.container_counter.lock().next()
    }

    /// Opens a menu for this player.
    ///
    /// Based on Java's `ServerPlayer::openMenu`.
    ///
    /// # Arguments
    /// * `title` - The display title shown in the open-screen packet.
    /// * `create` - Factory invoked with the allocated container id and the
    ///   player's world; returns the menu to open. The factory runs
    ///   synchronously and must only construct the menu; it must not lock
    ///   containers used by the current menu.
    ///
    /// # Panics
    /// Panics if the created menu has no menu type (i.e. the player's own
    /// inventory menu, which must never be opened via `open_menu`).
    pub fn open_menu(
        &self,
        title: impl Into<TextComponent>,
        create: impl FnOnce(u8, &Arc<World>) -> Menu,
    ) {
        if !self.begin_menu_open_operation() {
            return;
        }
        self.open_menu_inner(title, create);
        self.finish_menu_open_operation();
    }

    fn open_menu_inner(
        &self,
        title: impl Into<TextComponent>,
        create: impl FnOnce(u8, &Arc<World>) -> Menu,
    ) {
        self.do_close_container();

        {
            let open_menu = self.open_menu.lock();
            if open_menu.terminal_removal.is_some() {
                return;
            }
        }

        let container_id = self.next_container_counter();
        let menu = create(container_id, &self.get_world());
        let title = title.into();

        let mut open_menu = self.open_menu.lock();
        if let Some(terminal_removal) = open_menu.terminal_removal.as_mut() {
            terminal_removal.pending_menus.push(menu);
            return;
        }
        if let Some(dispatch) = open_menu.dispatch.as_mut() {
            dispatch
                .actions
                .push(DeferredMenuAction::Open(Box::new(PreparedMenu {
                    title,
                    menu,
                })));
            return;
        }
        drop(open_menu);

        self.open_prepared_menu(title, menu);
    }

    fn open_prepared_menu(&self, title: TextComponent, mut menu: Menu) {
        loop {
            {
                let mut open_menu = self.open_menu.lock();
                if let Some(terminal_removal) = open_menu.terminal_removal.as_mut() {
                    terminal_removal.pending_menus.push(menu);
                    return;
                }
            }

            // A removal hook may have opened another menu while the initiating
            // open call was closing its predecessor.
            self.do_close_container();

            let mut open_menu = self.open_menu.lock();
            if let Some(terminal_removal) = open_menu.terminal_removal.as_mut() {
                terminal_removal.pending_menus.push(menu);
                return;
            }
            if let Some(dispatch) = open_menu.dispatch.as_mut() {
                dispatch
                    .actions
                    .push(DeferredMenuAction::Open(Box::new(PreparedMenu {
                        title,
                        menu,
                    })));
                return;
            }
            if open_menu.menu.is_some() {
                continue;
            }
            open_menu.dispatch = Some(OpenMenuDispatch {
                container_id: menu.container_id(),
                actions: Vec::new(),
            });
            break;
        }

        self.send_packet(COpenScreen {
            container_id: i32::from(menu.container_id()),
            menu_type: menu
                .menu_type()
                .expect("a menu opened via open_menu must declare a menu type"),
            title,
        });

        // Fire on_open before the full sync so anything the menu populates here
        // is included in the first render sent below.
        menu.on_open(self);

        menu.behavior_mut()
            .send_all_data_to_remote(&self.connection);

        self.finish_open_menu_callback(menu);
    }

    /// A shared handle to the 2x2 crafting grid of the always-open inventory
    /// menu.
    pub fn crafting_container(&self) -> Shared<CraftingContainer> {
        let menu = self.inventory_menu.lock();
        let MenuKindType::Inventory(kind) = menu.kind() else {
            unreachable!("a player's inventory_menu is always the Inventory kind");
        };
        kind.crafting_container()
    }

    /// A shared handler for the 2x2 crafting grid of the always-open inventory
    /// menu and its result.
    pub(crate) fn inventory_crafting_handler(&self) -> CraftingHandler {
        let menu = self.inventory_menu.lock();
        let MenuKindType::Inventory(kind) = menu.kind() else {
            unreachable!("a player's inventory_menu is always the Inventory kind");
        };
        kind.crafting_handler()
    }

    /// Closes the currently open container and returns to the inventory menu.
    ///
    /// Based on Java's `ServerPlayer::closeContainer`.
    /// This sends a close packet to the client.
    pub fn close_container(&self) {
        self.close_open_menu(true);
    }

    /// Internal close container logic without sending a packet.
    ///
    /// Based on Java's `ServerPlayer::doCloseContainer`.
    /// Called when the client sends a close packet or when opening a new menu.
    pub fn do_close_container(&self) {
        self.close_open_menu(false);
    }

    /// Removes both the base inventory menu and any external menu.
    ///
    /// This mirrors `Player::remove`: base crafting and carried items are
    /// handled before the external menu, and menu hooks cannot install a
    /// replacement while removal is in progress. The inventory menu remains
    /// reusable because Steel keeps one `Player` across world changes.
    pub fn remove_all_menus(&self) -> MenuRemovalStatus {
        self.remove_all_menus_with_disposition(self.default_menu_item_disposition())
    }

    pub(super) fn remove_all_menus_with_disposition(
        &self,
        disposition: MenuItemDisposition,
    ) -> MenuRemovalStatus {
        let menu = {
            let mut open_menu = self.open_menu.lock();
            if let Some(terminal_removal) = open_menu.terminal_removal.as_mut() {
                terminal_removal.disposition = terminal_removal.disposition.combine(disposition);
                return MenuRemovalStatus::Pending;
            }

            open_menu.terminal_removal = Some(TerminalMenuRemoval {
                disposition,
                main_cleanup_complete: false,
                pending_cleanup_in_progress: false,
                pending_menus: Vec::new(),
            });
            if open_menu.dispatch.is_some() {
                return MenuRemovalStatus::Pending;
            }

            open_menu.menu.take()
        };

        self.finish_terminal_menu_main_cleanup(menu);
        if self.open_menu.lock().terminal_removal.is_none() {
            MenuRemovalStatus::Complete
        } else {
            MenuRemovalStatus::Pending
        }
    }

    fn finish_terminal_menu_main_cleanup(&self, mut menu: Option<Menu>) {
        self.inventory_menu.lock().removed(self);
        if let Some(menu) = menu.as_mut() {
            self.remove_open_menu(menu);
        }

        {
            let mut open_menu = self.open_menu.lock();
            let Some(terminal_removal) = open_menu.terminal_removal.as_mut() else {
                return;
            };
            terminal_removal.main_cleanup_complete = true;
        }
        self.try_finish_terminal_menu_removal();
    }

    fn try_finish_terminal_menu_removal(&self) {
        loop {
            let pending_menus = {
                let mut open_menu = self.open_menu.lock();
                if open_menu.active_open_operations != 0 {
                    return;
                }
                let Some(terminal_removal) = open_menu.terminal_removal.as_mut() else {
                    return;
                };
                if !terminal_removal.main_cleanup_complete {
                    return;
                }
                if terminal_removal.pending_cleanup_in_progress {
                    return;
                }
                if terminal_removal.pending_menus.is_empty() {
                    open_menu.terminal_removal = None;
                    debug_assert!(open_menu.menu.is_none());
                    return;
                }
                terminal_removal.pending_cleanup_in_progress = true;
                mem::take(&mut terminal_removal.pending_menus)
            };

            for mut pending_menu in pending_menus {
                pending_menu.removed(self);
            }

            let mut open_menu = self.open_menu.lock();
            let Some(terminal_removal) = open_menu.terminal_removal.as_mut() else {
                return;
            };
            terminal_removal.pending_cleanup_in_progress = false;
        }
    }

    #[cfg(test)]
    pub(super) fn retry_terminal_menu_removal_for_test(&self) {
        self.try_finish_terminal_menu_removal();
    }

    fn close_open_menu(&self, send_packet: bool) {
        let menu = {
            let mut open_menu = self.open_menu.lock();
            if open_menu.terminal_removal.is_some() {
                return;
            }
            if let Some(dispatch) = open_menu.dispatch.as_mut() {
                dispatch
                    .actions
                    .push(DeferredMenuAction::Close { send_packet });
                return;
            }
            let Some(menu) = open_menu.menu.take() else {
                return;
            };
            open_menu.dispatch = Some(OpenMenuDispatch {
                container_id: menu.container_id(),
                actions: Vec::new(),
            });
            menu
        };

        let mut menu = menu;
        if send_packet {
            self.send_packet(CContainerClose {
                container_id: i32::from(menu.container_id()),
            });
        }
        self.remove_open_menu(&mut menu);
        self.finish_open_menu_removal();
    }

    fn remove_open_menu(&self, menu: &mut Menu) {
        menu.removed(self);
        self.inventory_menu
            .lock()
            .behavior_mut()
            .transfer_state(menu.behavior());
    }

    /// Returns true if the player has an external menu open (not the inventory).
    #[must_use]
    pub fn has_container_open(&self) -> bool {
        let open_menu = self.open_menu.lock();
        open_menu.menu.is_some() || open_menu.dispatch.is_some()
    }

    /// Runs the open menu's per-tick hook, if an external menu is open.
    ///
    /// Scoped to the opened menu; the base inventory menu is not ticked. Called
    /// once per player tick, before syncing inventory changes to the client.
    pub fn tick_open_menu(&self) {
        let Ok(mut menu) = self.take_open_menu_for_callback(None) else {
            return;
        };
        if !menu.still_valid(self) {
            self.close_container();
            self.finish_open_menu_callback(menu);
            return;
        }
        menu.on_tick(self);
        self.finish_open_menu_callback(menu);
    }

    /// Broadcasts inventory changes to the client (incremental sync).
    /// This is called every tick to sync only changed slots.
    pub fn broadcast_inventory_changes(&self) {
        let mut open_menu = self.open_menu.lock();
        if let Some(menu) = open_menu.menu.as_mut() {
            menu.behavior_mut().broadcast_changes(&self.connection);
            return;
        }
        if open_menu.dispatch.is_none() {
            drop(open_menu);
            self.inventory_menu
                .lock()
                .behavior_mut()
                .broadcast_changes(&self.connection);
        }
    }

    /// Removes or counts matching stacks across every location used by vanilla `/clear`.
    pub(crate) fn clear_or_count_matching_items(
        &self,
        predicate: &dyn Fn(&ItemStack) -> bool,
        amount_to_remove: i32,
    ) -> i32 {
        let counting_only = amount_to_remove == 0;
        let mut count = self.inventory.lock().clear_or_count_matching_items(
            predicate,
            amount_to_remove,
            counting_only,
        );

        count += self.inventory_menu.lock().clear_or_count_crafting_items(
            predicate,
            amount_to_remove - count,
            counting_only,
        );

        let has_open_menu = {
            let mut open_menu = self.open_menu.lock();
            if let Some(menu) = open_menu.menu.as_mut() {
                let behavior = menu.behavior_mut();
                count += clear_or_count_matching_stack(
                    behavior.carried_mut(),
                    predicate,
                    amount_to_remove - count,
                    counting_only,
                );
                if behavior.carried().is_empty() {
                    *behavior.carried_mut() = ItemStack::empty();
                }
                true
            } else {
                open_menu.dispatch.is_some()
            }
        };
        if !has_open_menu {
            let mut inventory_menu = self.inventory_menu.lock();
            let behavior = inventory_menu.behavior_mut();
            count += clear_or_count_matching_stack(
                behavior.carried_mut(),
                predicate,
                amount_to_remove - count,
                counting_only,
            );
            if behavior.carried().is_empty() {
                *behavior.carried_mut() = ItemStack::empty();
            }
        }

        self.inventory_menu.lock().update_crafting_result();
        self.broadcast_inventory_changes();
        count
    }

    /// Drops an item from the player's selected hotbar slot.
    ///
    /// Based on Java's `ServerPlayer.drop(boolean all)`.
    ///
    /// - `all`: If true, drops the entire stack (Ctrl+Q). If false, drops one item (Q).
    pub fn drop_from_selected(&self, all: bool) {
        if !self.can_drop_items() {
            return;
        }

        let removed = {
            let mut inventory = self.inventory.lock();
            let selected_count = inventory.get_selected_item().count();
            if selected_count == 0 {
                return;
            }
            inventory.split_item_in_hand(
                InteractionHand::MainHand,
                if all { selected_count } else { 1 },
            )
        };

        let _ = self.drop_item(removed, false, true);
    }

    /// Drops an item into the world.
    ///
    /// Based on Java's `LivingEntity.drop(ItemStack, boolean randomly, boolean thrownFromHand)`.
    ///
    /// - `throw_randomly`: If true, the item is thrown in a random direction.
    ///   If false, it's thrown in the direction the player is facing.
    /// - `thrown_from_hand`: If true, sets the thrower and uses a longer pickup delay.
    #[must_use]
    pub fn drop_item(
        &self,
        item: ItemStack,
        throw_randomly: bool,
        thrown_from_hand: bool,
    ) -> Option<Arc<ItemEntity>> {
        if item.is_empty() {
            return None;
        }

        let pos = self.position();
        let (yaw, pitch) = self.rotation();

        let spawn_y = self.get_eye_y() - 0.3;

        let velocity = if throw_randomly {
            let power = rand::random::<f32>() * 0.5;
            let angle = rand::random::<f32>() * TAU;
            DVec3::new(
                f64::from(-angle.sin() * power),
                0.2,
                f64::from(angle.cos() * power),
            )
        } else {
            let pitch_rad = pitch.to_radians();
            let yaw_rad = yaw.to_radians();

            let sin_pitch = pitch_rad.sin();
            let cos_pitch = pitch_rad.cos();
            let sin_yaw = yaw_rad.sin();
            let cos_yaw = yaw_rad.cos();

            let angle_offset = rand::random::<f32>() * TAU;
            let power_offset = 0.02 * rand::random::<f32>();

            DVec3::new(
                f64::from(-sin_yaw * cos_pitch * 0.3)
                    + f64::from(angle_offset.cos() * power_offset),
                f64::from(-sin_pitch * 0.3 + 0.1)
                    + f64::from((rand::random::<f32>() - rand::random::<f32>()) * 0.1),
                f64::from(cos_yaw * cos_pitch * 0.3) + f64::from(angle_offset.sin() * power_offset),
            )
        };

        let spawn_pos = DVec3::new(pos.x, spawn_y, pos.z);

        let entity = self
            .get_world()
            .spawn_item_with_velocity(spawn_pos, item, velocity)?;
        entity.set_pickup_delay(40);
        if thrown_from_hand {
            entity.set_thrower(self.gameprofile.id);
        }
        Some(entity)
    }

    /// Returns true if the player can drop items.
    ///
    /// Based on Java's `Player.canDropItems()`.
    /// Returns false if the player is dead, removed, or has a flag preventing item drops.
    #[must_use]
    pub fn can_drop_items(&self) -> bool {
        !self.is_removed()
        // TODO: Check if player is alive (health > 0)
    }

    /// Returns whether items from a closing menu (crafting grid, anvil inputs,
    /// cursor) should be placed back into the inventory instead of dropped into
    /// the world.
    ///
    /// Matches vanilla's `AbstractContainerMenu.dropOrPlaceInInventory`: a
    /// disconnected player or one removed for any reason except a world change
    /// drops the items.
    #[must_use]
    pub fn returns_menu_items_to_inventory(&self) -> bool {
        if let Some(disposition) = self
            .open_menu
            .lock()
            .terminal_removal
            .as_ref()
            .map(|terminal_removal| terminal_removal.disposition)
        {
            return disposition == MenuItemDisposition::ReturnToInventory;
        }

        self.default_menu_item_disposition() == MenuItemDisposition::ReturnToInventory
    }

    fn default_menu_item_disposition(&self) -> MenuItemDisposition {
        let removed_outside_world_change =
            self.is_removed() && self.removal_reason() != Some(RemovalReason::ChangedWorld);
        if removed_outside_world_change || self.connection.closed() {
            MenuItemDisposition::Drop
        } else {
            MenuItemDisposition::ReturnToInventory
        }
    }

    /// Tries to add an item to the player's inventory, dropping it if it doesn't fit.
    ///
    /// Based on Java's `Inventory.placeItemBackInInventory`.
    pub fn add_item_or_drop(&self, mut item: ItemStack) {
        if item.is_empty() {
            return;
        }

        let added = self.inventory.lock().add(&mut item);
        if !added || !item.is_empty() {
            let _ = self.drop_item(item, false, false);
        }
    }

    /// Tries to add an item to the player's inventory using an existing lock guard,
    /// dropping it if it doesn't fit.
    ///
    /// Use this variant when you already hold a `ContainerLockGuard` that includes
    /// the player's inventory to avoid deadlocks.
    pub fn add_item_or_drop_with_guard(&self, guard: &mut ContainerLockGuard, mut item: ItemStack) {
        if item.is_empty() {
            return;
        }

        let inv_id = ContainerId::from_arc(&self.inventory);
        let should_drop = if let Some(inv) = guard.get_mut(inv_id) {
            let added = inv.add(&mut item);
            !added || !item.is_empty()
        } else {
            true
        };
        if should_drop {
            let _ = guard.run_unlocked(|| self.drop_item(item, false, false));
        }
    }
}

impl PlayerInventory {
    /// Applies vanilla `ItemUtils.createFilledResult` to a held item.
    ///
    /// Mutates the held stack and inventory, returning only the result stack that
    /// should be dropped by the caller. Creative inventory insertion discards
    /// leftover result items instead of dropping them.
    pub fn apply_filled_result(
        &mut self,
        hand: InteractionHand,
        mut result_stack: ItemStack,
        has_infinite_materials: bool,
        limit_creative_stack_size: bool,
    ) -> ItemStack {
        if limit_creative_stack_size && has_infinite_materials {
            if !self.contains_stack(&result_stack) {
                let _ = self.add(&mut result_stack);
            }
            return ItemStack::empty();
        }

        if !has_infinite_materials {
            self.shrink_item_in_hand(hand, 1);
        }

        if self.get_item_in_hand(hand).is_empty() {
            self.set_item_in_hand(hand, result_stack);
            return ItemStack::empty();
        }

        let added = self.add(&mut result_stack);
        if added || has_infinite_materials {
            ItemStack::empty()
        } else {
            result_stack
        }
    }
}

/// Static empty item stack for returning references to invalid slots.
static EMPTY_ITEM: LazyLock<ItemStack> = LazyLock::new(ItemStack::empty);

/// Error returned when a carried-item packet selects a non-hotbar slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidHotbarSlot;

impl PlayerInventory {
    const fn equipment_slot_index(&self, slot: EquipmentSlot) -> usize {
        equipment_to_slot(slot, self.selected)
    }
}

impl EntityEquipment for PlayerInventory {
    fn get_ref(&self, slot: EquipmentSlot) -> &ItemStack {
        &self.items[self.equipment_slot_index(slot)]
    }

    fn get_mut(&mut self, slot: EquipmentSlot) -> &mut ItemStack {
        let inventory_index = self.equipment_slot_index(slot);
        &mut self.items[inventory_index]
    }

    fn set(&mut self, slot: EquipmentSlot, stack: ItemStack) -> ItemStack {
        let inventory_index = self.equipment_slot_index(slot);
        let old = mem::replace(&mut self.items[inventory_index], stack);
        Container::set_changed(self);
        old
    }

    fn take(&mut self, slot: EquipmentSlot) -> ItemStack {
        let inventory_index = self.equipment_slot_index(slot);
        let old = mem::take(&mut self.items[inventory_index]);
        if !old.is_empty() {
            Container::set_changed(self);
        }
        old
    }

    fn clear(&mut self) {
        let mut changed = false;
        for slot in EquipmentSlot::ALL {
            let inventory_index = self.equipment_slot_index(slot);
            if self.items[inventory_index].is_empty() {
                continue;
            }

            self.items[inventory_index] = ItemStack::empty();
            changed = true;
        }
        if changed {
            Container::set_changed(self);
        }
    }
}

impl Container for PlayerInventory {
    fn items(&self) -> &[ItemStack] {
        &self.items
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        &mut self.items
    }

    fn get_container_size(&self) -> usize {
        Self::CONTAINER_SIZE
    }

    /// Adds an item to the player's main inventory (slots 0-35 only).
    ///
    /// Overrides the default `Container::add()` to prevent items from being
    /// placed in armor or equipment slots. Matches vanilla's `Inventory.add()`
    /// behavior which only adds to `this.items` (the 36 main slots).
    fn add(&mut self, stack: &mut ItemStack) -> bool {
        if stack.is_empty() {
            return true;
        }

        let max_size = self.get_max_stack_size_for_item(stack);
        let mut changed = false;

        // Vanilla prioritizes the selected slot, then an existing compatible
        // offhand stack, before scanning the remaining main inventory.
        if stack.is_stackable() {
            let selected = self.selected as usize;
            for slot in [selected, Self::SLOT_OFFHAND] {
                if stack.is_empty() {
                    if changed {
                        self.set_changed();
                    }
                    return true;
                }
                let existing = &mut self.items[slot];
                if !existing.is_empty() && ItemStack::is_same_item_same_components(existing, stack)
                {
                    let space = max_size - existing.count();
                    if space > 0 {
                        let to_add = stack.count().min(space);
                        existing.grow(to_add);
                        stack.shrink(to_add);
                        changed = true;
                    }
                }
            }

            for slot in 0..Self::INVENTORY_SIZE {
                if stack.is_empty() {
                    if changed {
                        self.set_changed();
                    }
                    return true;
                }
                if slot == selected {
                    continue;
                }
                let existing = &mut self.items[slot];
                if !existing.is_empty() && ItemStack::is_same_item_same_components(existing, stack)
                {
                    let space = max_size - existing.count();
                    if space > 0 {
                        let to_add = stack.count().min(space);
                        existing.grow(to_add);
                        stack.shrink(to_add);
                        changed = true;
                    }
                }
            }
        }

        // Second pass: try empty slots in main inventory only
        for slot in 0..Self::INVENTORY_SIZE {
            if stack.is_empty() {
                if changed {
                    self.set_changed();
                }
                return true;
            }
            if self.items[slot].is_empty() {
                let to_place = stack.count().min(max_size);
                self.items[slot] = stack.split(to_place);
                changed = true;
            }
        }

        if changed {
            self.set_changed();
        }
        stack.is_empty()
    }

    fn get_item(&self, slot: usize) -> &ItemStack {
        if slot < Self::CONTAINER_SIZE {
            &self.items[slot]
        } else {
            &EMPTY_ITEM
        }
    }

    fn get_item_mut(&mut self, slot: usize) -> &mut ItemStack {
        assert!(slot < Self::CONTAINER_SIZE, "Invalid slot index: {slot}");
        &mut self.items[slot]
    }

    fn set_item(&mut self, slot: usize, stack: ItemStack) {
        if slot == self.selected as usize {
            let _ = EntityEquipment::set(self, EquipmentSlot::MainHand, stack);
            return;
        }
        if let Some(equipment_slot) = slot_to_equipment(slot) {
            let _ = EntityEquipment::set(self, equipment_slot, stack);
            return;
        }
        if slot < Self::INVENTORY_SIZE {
            self.items[slot] = stack;
        }
        self.set_changed();
    }

    fn is_empty(&self) -> bool {
        self.items.iter().all(ItemStack::is_empty)
    }

    fn set_changed(&mut self) {
        self.times_changed = self.times_changed.wrapping_add(1);
    }

    fn clear_content(&mut self) -> i32 {
        let mut count = 0;
        for item in &mut self.items {
            count += item.count();
            *item = ItemStack::empty();
        }
        if count > 0 {
            self.set_changed();
        }
        count
    }

    fn clear_content_matching(&mut self, predicate: &mut dyn FnMut(&mut ItemStack) -> bool) -> i32 {
        let mut count = 0;
        for item in &mut self.items {
            if predicate(item) {
                count += item.count();
                *item = ItemStack::empty();
            }
        }
        if count > 0 {
            self.set_changed();
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use std::ptr;

    use steel_registry::test_support::init_test_registry;
    use steel_registry::vanilla_items;
    use steel_utils::Identifier;

    use super::*;

    #[test]
    fn vanilla_inventory_nbt_contains_main_slots_only() {
        init_test_registry();
        let mut inventory = PlayerInventory::new();
        inventory.items[2] = ItemStack::new(&vanilla_items::STONE);
        inventory.set(
            EquipmentSlot::Head,
            ItemStack::new(&vanilla_items::DIAMOND_HELMET),
        );

        let NbtList::Compound(items) = inventory.to_vanilla_inventory_nbt() else {
            panic!("player inventory should serialize as a compound list");
        };

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].get("Slot"), Some(&NbtTag::Byte(2)));
        assert_eq!(
            items[0].string("id").map(ToString::to_string),
            Some("minecraft:stone".to_owned())
        );
    }

    #[test]
    fn add_marks_changed_when_stack_fills_existing_slot() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.items[0] = ItemStack::with_count(&vanilla_items::OAK_LOG, 63);
        let before = inventory.get_times_changed();

        let mut stack = ItemStack::new(&vanilla_items::OAK_LOG);
        assert!(inventory.add(&mut stack));

        assert!(stack.is_empty());
        assert_eq!(inventory.items[0].count(), 64);
        assert_ne!(inventory.get_times_changed(), before);
    }

    #[test]
    fn add_to_selected_existing_slot_marks_inventory_changed() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.items[0] = ItemStack::with_count(&vanilla_items::OAK_LOG, 63);
        let before = inventory.get_times_changed();

        let mut stack = ItemStack::new(&vanilla_items::OAK_LOG);
        assert!(inventory.add(&mut stack));

        assert_eq!(inventory.get_selected_item().count(), 64);
        assert_ne!(inventory.get_times_changed(), before);
    }

    #[test]
    fn add_to_empty_selected_slot_marks_inventory_changed() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        let before = inventory.get_times_changed();

        let mut stack = ItemStack::with_count(&vanilla_items::OAK_LOG, 3);
        assert!(inventory.add(&mut stack));

        assert_eq!(inventory.get_selected_item().count(), 3);
        assert_ne!(inventory.get_times_changed(), before);
    }

    #[test]
    fn add_merges_into_existing_offhand_stack_before_main_inventory() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.set(
            EquipmentSlot::OffHand,
            ItemStack::with_count(&vanilla_items::OAK_LOG, 63),
        );

        let mut stack = ItemStack::new(&vanilla_items::OAK_LOG);
        assert!(inventory.add(&mut stack));

        assert!(stack.is_empty());
        assert_eq!(inventory.get_ref(EquipmentSlot::OffHand).count(), 64);
        assert!(inventory.get_items().iter().all(ItemStack::is_empty));
    }

    #[test]
    fn contains_stack_compares_components() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        let mut damaged_in_inventory = ItemStack::new(&vanilla_items::DIAMOND_PICKAXE);
        damaged_in_inventory.set_damage_value(3);
        inventory.items[0] = damaged_in_inventory;

        let mut damaged_search = ItemStack::new(&vanilla_items::DIAMOND_PICKAXE);
        damaged_search.set_damage_value(3);
        let undamaged_search = ItemStack::new(&vanilla_items::DIAMOND_PICKAXE);

        assert!(inventory.contains_stack(&damaged_search));
        assert!(!inventory.contains_stack(&undamaged_search));
    }

    #[test]
    fn filled_result_replaces_single_survival_hand_stack() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.set_selected_item(ItemStack::new(&vanilla_items::WATER_BUCKET));

        let overflow = inventory.apply_filled_result(
            InteractionHand::MainHand,
            ItemStack::new(&vanilla_items::BUCKET),
            false,
            true,
        );

        assert!(overflow.is_empty());
        assert_eq!(
            inventory.get_selected_item(),
            &ItemStack::new(&vanilla_items::BUCKET)
        );
    }

    #[test]
    fn filled_result_adds_result_for_stacked_survival_hand_stack() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.set_selected_item(ItemStack::with_count(&vanilla_items::BUCKET, 2));

        let overflow = inventory.apply_filled_result(
            InteractionHand::MainHand,
            ItemStack::new(&vanilla_items::WATER_BUCKET),
            false,
            true,
        );

        assert!(overflow.is_empty());
        assert_eq!(
            inventory.get_selected_item(),
            &ItemStack::new(&vanilla_items::BUCKET)
        );
        assert_eq!(
            inventory.get_item(1),
            &ItemStack::new(&vanilla_items::WATER_BUCKET)
        );
    }

    #[test]
    fn filled_result_creative_limited_keeps_matching_held_stack() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.set_selected_item(ItemStack::new(&vanilla_items::WATER_BUCKET));

        let overflow = inventory.apply_filled_result(
            InteractionHand::MainHand,
            ItemStack::new(&vanilla_items::WATER_BUCKET),
            true,
            true,
        );

        assert!(overflow.is_empty());
        assert_eq!(
            inventory.get_selected_item(),
            &ItemStack::new(&vanilla_items::WATER_BUCKET)
        );
        assert_eq!(
            (0..PlayerInventory::INVENTORY_SIZE)
                .filter(|&slot| !inventory.items[slot].is_empty())
                .count(),
            1
        );
    }

    #[test]
    fn filled_result_creative_limited_adds_missing_result_without_consuming_hand() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.set_selected_item(ItemStack::with_count(&vanilla_items::BUCKET, 16));

        let overflow = inventory.apply_filled_result(
            InteractionHand::MainHand,
            ItemStack::new(&vanilla_items::WATER_BUCKET),
            true,
            true,
        );

        assert!(overflow.is_empty());
        assert_eq!(
            inventory.get_selected_item(),
            &ItemStack::with_count(&vanilla_items::BUCKET, 16)
        );
        assert_eq!(
            inventory.get_item(1),
            &ItemStack::new(&vanilla_items::WATER_BUCKET)
        );
    }

    #[test]
    fn filled_result_empty_result_still_consumes_survival_hand_stack() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.set_selected_item(ItemStack::new(&vanilla_items::BUCKET));

        let overflow = inventory.apply_filled_result(
            InteractionHand::MainHand,
            ItemStack::empty(),
            false,
            true,
        );

        assert!(overflow.is_empty());
        assert!(inventory.get_selected_item().is_empty());
    }

    #[test]
    fn filled_result_creative_unlimited_discards_unadded_result() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.set_selected_item(ItemStack::new(&vanilla_items::LAVA_BUCKET));
        for slot in 1..PlayerInventory::INVENTORY_SIZE {
            inventory.items[slot] = ItemStack::with_count(&vanilla_items::OAK_LOG, 64);
        }

        let overflow = inventory.apply_filled_result(
            InteractionHand::MainHand,
            ItemStack::new(&vanilla_items::WATER_BUCKET),
            true,
            false,
        );

        assert!(overflow.is_empty());
        assert_eq!(
            inventory.get_selected_item(),
            &ItemStack::new(&vanilla_items::LAVA_BUCKET)
        );
    }

    #[test]
    fn clear_content_counts_equipment_items() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.items[0] = ItemStack::with_count(&vanilla_items::OAK_LOG, 3);
        inventory.set(
            EquipmentSlot::Head,
            ItemStack::new(&vanilla_items::DIAMOND_HELMET),
        );

        assert_eq!(inventory.clear_content(), 4);
        assert!(inventory.is_empty());
    }

    #[test]
    fn container_traversal_matches_visible_slot_indices() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        let size = inventory.get_container_size();
        for slot in 0..size {
            inventory.set_item(
                slot,
                ItemStack::with_count(&vanilla_items::OAK_LOG, slot as i32 + 1),
            );
        }

        let iterated_counts: Vec<_> = inventory.iter().map(ItemStack::count).collect();
        let indexed_counts: Vec<_> = (0..size)
            .map(|slot| inventory.get_item(slot).count())
            .collect();
        assert_eq!(iterated_counts, indexed_counts);
        assert_eq!(iterated_counts.len(), size);

        let mut mutable_count = 0;
        for (slot, item) in inventory.iter_mut().enumerate() {
            mutable_count += 1;
            item.set_count((size - slot) as i32);
        }
        assert_eq!(mutable_count, size);
        for slot in 0..size {
            assert_eq!(inventory.get_item(slot).count(), (size - slot) as i32);
        }

        let mut predicate_visits = 0;
        inventory.clear_content_matching(&mut |_| {
            predicate_visits += 1;
            false
        });
        assert_eq!(predicate_visits, size);
    }

    #[test]
    fn equipment_trait_aliases_vanilla_container_indices() {
        let inventory = PlayerInventory::new();
        assert_eq!(inventory.items().len(), PlayerInventory::CONTAINER_SIZE);
        assert_eq!(
            inventory.get_container_size(),
            PlayerInventory::CONTAINER_SIZE
        );
        assert!(ptr::eq(
            inventory.get_ref(EquipmentSlot::MainHand),
            inventory.get_item(0)
        ));

        for (equipment_slot, container_slot) in [
            (EquipmentSlot::Feet, 36),
            (EquipmentSlot::Legs, 37),
            (EquipmentSlot::Chest, 38),
            (EquipmentSlot::Head, 39),
            (EquipmentSlot::OffHand, 40),
            (EquipmentSlot::Body, 41),
            (EquipmentSlot::Saddle, 42),
        ] {
            assert!(ptr::eq(
                inventory.get_ref(equipment_slot),
                inventory.get_item(container_slot)
            ));
        }
    }

    #[test]
    fn mutable_container_slice_exposes_all_logical_slots() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        {
            let items = inventory.items_mut();
            items[0] = ItemStack::new(&vanilla_items::STICK);
            items[39] = ItemStack::new(&vanilla_items::DIAMOND_HELMET);
        }
        inventory.set_changed();

        assert!(
            inventory
                .get_ref(EquipmentSlot::MainHand)
                .is(&vanilla_items::STICK)
        );
        assert!(
            inventory
                .get_ref(EquipmentSlot::Head)
                .is(&vanilla_items::DIAMOND_HELMET)
        );
    }

    #[test]
    fn main_inventory_search_does_not_use_equipment_slots() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        for slot in 0..PlayerInventory::INVENTORY_SIZE {
            inventory.items[slot] = ItemStack::with_count(&vanilla_items::OAK_LOG, 64);
        }
        inventory.set(EquipmentSlot::Head, ItemStack::new(&vanilla_items::STONE));

        assert_eq!(inventory.get_free_slot(), -1);
        assert_eq!(
            inventory.find_slot_matching_item(&ItemStack::new(&vanilla_items::STONE)),
            -1
        );
    }

    #[test]
    fn equipment_main_hand_follows_selected_slot() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.items[0] = ItemStack::new(&vanilla_items::STICK);
        inventory.items[1] = ItemStack::new(&vanilla_items::OAK_LOG);

        assert!(
            inventory
                .get_ref(EquipmentSlot::MainHand)
                .is(&vanilla_items::STICK)
        );
        inventory.set_selected_slot(1);
        assert!(
            inventory
                .get_ref(EquipmentSlot::MainHand)
                .is(&vanilla_items::OAK_LOG)
        );
        assert!(inventory.items[0].is(&vanilla_items::STICK));
    }

    #[test]
    fn non_empty_equipment_items_uses_selected_item_as_main_hand() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        let main_hand = ItemStack::with_count(&vanilla_items::OAK_LOG, 2);
        let head = ItemStack::new(&vanilla_items::DIAMOND_HELMET);
        inventory.items[0] = main_hand.clone();
        inventory.set(EquipmentSlot::Head, head.clone());

        let items = inventory.non_empty_items();

        assert_eq!(items.len(), 2);
        assert!(items.contains(&(EquipmentSlot::MainHand, main_hand)));
        assert!(items.contains(&(EquipmentSlot::Head, head)));
    }

    #[test]
    fn packet_selected_slot_rejects_invalid_values_without_wrapping() {
        let mut inventory = PlayerInventory::new();

        assert!(inventory.try_set_selected_slot_from_packet(8).is_ok());
        assert_eq!(inventory.get_selected_slot(), 8);

        assert_eq!(
            inventory.try_set_selected_slot_from_packet(9),
            Err(InvalidHotbarSlot)
        );
        assert_eq!(inventory.get_selected_slot(), 8);

        assert_eq!(
            inventory.try_set_selected_slot_from_packet(-1),
            Err(InvalidHotbarSlot)
        );
        assert_eq!(inventory.get_selected_slot(), 8);

        assert_eq!(
            inventory.try_set_selected_slot_from_packet(256),
            Err(InvalidHotbarSlot)
        );
        assert_eq!(inventory.get_selected_slot(), 8);
    }

    #[test]
    fn shrink_item_in_hand_marks_inventory_changed() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.set_selected_item(ItemStack::with_count(&vanilla_items::OAK_LOG, 3));
        inventory.set_offhand_item(ItemStack::with_count(&vanilla_items::SHIELD, 2));

        let before = inventory.get_times_changed();
        inventory.shrink_item_in_hand(InteractionHand::MainHand, 1);

        assert_eq!(inventory.get_selected_item().count(), 2);
        assert_ne!(inventory.get_times_changed(), before);

        let before = inventory.get_times_changed();
        inventory.shrink_item_in_hand(InteractionHand::OffHand, 1);

        assert_eq!(inventory.get_offhand_item().count(), 1);
        assert_ne!(inventory.get_times_changed(), before);
    }

    #[test]
    fn split_item_in_hand_marks_inventory_changed() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.set_selected_item(ItemStack::with_count(&vanilla_items::OAK_LOG, 3));

        let before = inventory.get_times_changed();
        let split = inventory.split_item_in_hand(InteractionHand::MainHand, 1);

        assert_eq!(split, ItemStack::with_count(&vanilla_items::OAK_LOG, 1));
        assert_eq!(inventory.get_selected_item().count(), 2);
        assert_ne!(inventory.get_times_changed(), before);
    }

    #[test]
    fn mutating_only_held_item_components_marks_inventory_changed() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.set_selected_item(ItemStack::new(&vanilla_items::DIAMOND_SWORD));

        let before = inventory.get_times_changed();
        inventory.mutate_item_in_hand(InteractionHand::MainHand, |stack| {
            stack.set_enchantments(&[(Identifier::vanilla_static("sharpness"), 1)], false);
        });

        assert_ne!(inventory.get_times_changed(), before);
        assert_eq!(
            inventory
                .get_selected_item()
                .get_enchantment_level(&Identifier::vanilla_static("sharpness")),
            1
        );
    }

    #[test]
    fn hurt_item_in_hand_marks_inventory_changed() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.set_selected_item(ItemStack::new(&vanilla_items::SHEARS));

        let before = inventory.get_times_changed();
        inventory.hurt_item_in_hand(InteractionHand::MainHand, 1, false);

        let main_hand = inventory.get_selected_item();
        assert!(main_hand.is(&vanilla_items::SHEARS));
        assert_eq!(main_hand.get_damage_value(), 1);
        assert_ne!(inventory.get_times_changed(), before);
    }

    #[test]
    fn hurt_and_convert_item_in_hand_damages_without_breaking() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.set_offhand_item(ItemStack::new(&vanilla_items::CARROT_ON_A_STICK));

        let before = inventory.get_times_changed();
        inventory.hurt_and_convert_item_in_hand_on_break(
            InteractionHand::OffHand,
            1,
            &vanilla_items::FISHING_ROD,
            false,
        );

        let offhand = inventory.get_offhand_item();
        assert!(offhand.is(&vanilla_items::CARROT_ON_A_STICK));
        assert_eq!(offhand.get_damage_value(), 1);
        assert_ne!(inventory.get_times_changed(), before);
    }

    #[test]
    fn hurt_and_convert_item_in_hand_replaces_broken_item() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.set_selected_item(ItemStack::new(&vanilla_items::CARROT_ON_A_STICK));
        let max_damage = inventory.get_selected_item().get_max_damage();
        inventory
            .get_selected_item_mut()
            .set_damage_value(max_damage - 1);

        let before = inventory.get_times_changed();
        inventory.hurt_and_convert_item_in_hand_on_break(
            InteractionHand::MainHand,
            7,
            &vanilla_items::FISHING_ROD,
            false,
        );

        let main_hand = inventory.get_selected_item();
        assert!(main_hand.is(&vanilla_items::FISHING_ROD));
        assert_eq!(main_hand.count(), 1);
        assert_eq!(main_hand.get_damage_value(), 0);
        assert_ne!(inventory.get_times_changed(), before);
    }

    #[test]
    fn swap_hands_swaps_selected_and_offhand() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        let main_hand = ItemStack::with_count(&vanilla_items::OAK_LOG, 3);
        let offhand = ItemStack::new(&vanilla_items::SHIELD);
        inventory.set_selected_item(main_hand.clone());
        inventory.set_offhand_item(offhand.clone());

        assert!(inventory.swap_hands());

        assert_eq!(inventory.get_selected_item(), &offhand);
        assert_eq!(inventory.get_offhand_item(), &main_hand);
    }

    #[test]
    fn equippable_single_item_moves_to_empty_armor_slot() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.set_selected_item(ItemStack::new(&vanilla_items::DIAMOND_HELMET));

        let result = inventory.try_swap_with_equipment_slot(
            InteractionHand::MainHand,
            EquipmentSlot::Head,
            false,
        );

        assert_eq!(result, EquipmentSwapResult::Success(ItemStack::empty()));
        assert!(inventory.get_selected_item().is_empty());
        assert_eq!(
            inventory.get_ref(EquipmentSlot::Head),
            &ItemStack::new(&vanilla_items::DIAMOND_HELMET)
        );
    }

    #[test]
    fn equippable_swap_respects_prevent_armor_change_effect() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        let mut bound_helmet = ItemStack::new(&vanilla_items::DIAMOND_HELMET);
        bound_helmet.set_enchantments(&[(Identifier::vanilla_static("binding_curse"), 1)], false);
        inventory.set_selected_item(ItemStack::new(&vanilla_items::CARVED_PUMPKIN));
        inventory.set(EquipmentSlot::Head, bound_helmet.copy_with_count(1));

        let result = inventory.try_swap_with_equipment_slot(
            InteractionHand::MainHand,
            EquipmentSlot::Head,
            false,
        );

        assert_eq!(result, EquipmentSwapResult::Fail);
        assert_eq!(
            inventory.get_selected_item(),
            &ItemStack::new(&vanilla_items::CARVED_PUMPKIN)
        );
        assert_eq!(inventory.get_ref(EquipmentSlot::Head), &bound_helmet);
    }

    #[test]
    fn repair_with_xp_repairs_damaged_mending_item() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        let mut pickaxe = ItemStack::new(&vanilla_items::DIAMOND_PICKAXE);
        pickaxe.set_damage_value(10);
        pickaxe.set_enchantments(&[(Identifier::vanilla_static("mending"), 1)], false);
        inventory.set_selected_item(pickaxe);
        let before = inventory.get_times_changed();

        let remaining = inventory.repair_random_equipped_item_with_xp(3);

        assert_eq!(remaining, 0);
        assert_eq!(inventory.get_selected_item().get_damage_value(), 4);
        assert_ne!(inventory.get_times_changed(), before);
    }

    #[test]
    fn repair_with_xp_returns_leftover_when_item_is_fully_repaired() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        let mut pickaxe = ItemStack::new(&vanilla_items::DIAMOND_PICKAXE);
        pickaxe.set_damage_value(3);
        pickaxe.set_enchantments(&[(Identifier::vanilla_static("mending"), 1)], false);
        inventory.set_selected_item(pickaxe);

        let remaining = inventory.repair_random_equipped_item_with_xp(5);

        assert_eq!(remaining, 4);
        assert_eq!(inventory.get_selected_item().get_damage_value(), 0);
    }

    #[test]
    fn equippable_stack_moves_one_item_and_returns_old_equipment_to_inventory() {
        init_test_registry();

        let mut inventory = PlayerInventory::new();
        inventory.set_selected_item(ItemStack::with_count(&vanilla_items::CARVED_PUMPKIN, 2));
        inventory.set(
            EquipmentSlot::Head,
            ItemStack::new(&vanilla_items::DIAMOND_HELMET),
        );

        let result = inventory.try_swap_with_equipment_slot(
            InteractionHand::MainHand,
            EquipmentSlot::Head,
            false,
        );

        assert_eq!(result, EquipmentSwapResult::Success(ItemStack::empty()));
        assert_eq!(inventory.get_selected_item().count(), 1);
        assert_eq!(
            inventory.get_ref(EquipmentSlot::Head),
            &ItemStack::new(&vanilla_items::CARVED_PUMPKIN)
        );
        assert!(
            inventory
                .get_items()
                .iter()
                .any(|stack| stack.is(&vanilla_items::DIAMOND_HELMET))
        );
    }
}
