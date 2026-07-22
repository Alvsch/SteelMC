//! A menu can be considered everything that's shown on the screen.
//! It consists of slots, slots consist of a view into a single inventory and position.
//! When you have a chest open for example a chest menu is shown, consisting of the chests slots and the players inventory slots.
//!
//! A menu is always the middle man between the server and the client.
//! This means that when the player doesn't have any menu open it still has its own inventory menu open.
//!
//! A menu holds 2 important structures:
//! - All slots for that menu
//! - The clients perception of the itemstacks
//!
//! This makes it so every time we run a sync (once per tick) we can compare the actual itemstacks
//! with the clients perception of the itemstacks.
//! And if there are mismatches we can send the correct itemstacks to the client.
//!
//! The client also sends the itemstacks it thinks it has on interaction, so this makes it so we only update the client if they mismatch.

use std::{mem, sync::Arc};

use steel_protocol::{
    packet_traits::{ClientPacket, EncodedPacket},
    packets::game::{
        CContainerSetContent, CContainerSetData, CContainerSetSlot, CSetCursorItem, HashedPatchMap,
        HashedStack,
    },
    utils::ConnectionProtocol,
};
use steel_registry::{
    REGISTRY, RegistryEntry, RegistryExt as _, data_components::DataComponentPatch,
    item_stack::ItemStack, menu_type::MenuTypeRef,
};

use crate::{
    inventory::{
        click::{DragKind, MouseButton, QuickCraft, can_item_quick_replace},
        lock::{ContainerId, ContainerLockGuard, ContainerRef},
        menu::builder::{FillDirection, MenuInstanceId},
        slots::{Slot, SlotType},
    },
    player::{Player, PlayerConnection, connection::NetworkConnection},
};

/// Shared behavior and state for all menu types.
pub struct MenuBehavior {
    /// The slots in this menu.
    slots: Vec<SlotType>,
    /// The client's perception of the itemstacks.
    remote_slots: Vec<RemoteSlot>,
    /// The item being carried by the cursor.
    carried: ItemStack,
    /// The client's perception of the carried item.
    remote_carried: RemoteSlot,
    /// The container ID (0 for player inventory).
    container_id: u8,
    /// Incremented every time the server and client mismatch.
    state_id: u32,
    /// None for player inventory. Some for all other menus.
    menu_type: Option<MenuTypeRef>,
    /// When true, remote updates are suppressed (during click handling).
    suppress_remote_updates: bool,
    /// The kind of drag in progress, or `None` when idle (replaces vanilla's
    /// `quickcraftStatus`/`quickcraftType` ints).
    quickcraft: Option<DragKind>,
    /// Slots involved in the current quickcraft operation.
    quickcraft_slots: Vec<usize>,
    /// Data slots (for furnace progress, enchanting levels, etc.).
    data_slots: Vec<i16>,
    /// The client's perception of the data slot values.
    remote_data_slots: Vec<i16>,
    container_refs: Vec<ContainerRef>,
    /// Identity stamp tying [`Section`](crate::inventory::Section) /
    /// [`DataSlot`](crate::inventory::DataSlot) handles to this menu.
    instance: MenuInstanceId,
}

impl MenuBehavior {
    /// Creates a new menu behavior with the given slots. Crate-internal:
    /// menus are assembled by [`MenuBuilder::build`](crate::inventory::MenuBuilder::build).
    #[must_use]
    pub(crate) fn new(
        instance: MenuInstanceId,
        slots: Vec<SlotType>,
        container_id: u8,
        menu_type: Option<MenuTypeRef>,
        container_refs: Vec<ContainerRef>,
    ) -> Self {
        let slot_count = slots.len();
        Self {
            instance,
            slots,
            remote_slots: vec![RemoteSlot::Unknown; slot_count],
            carried: ItemStack::empty(),
            remote_carried: RemoteSlot::Unknown,
            container_id,
            state_id: 0,
            menu_type,
            suppress_remote_updates: false,
            quickcraft: None,
            quickcraft_slots: Vec::new(),
            data_slots: Vec::new(),
            remote_data_slots: Vec::new(),
            container_refs,
        }
    }

    /// Locks all containers referenced by slots in this menu.
    #[must_use]
    pub fn lock_all_containers(&self) -> ContainerLockGuard {
        ContainerLockGuard::lock_all(&self.container_refs)
    }

    /// The slots of this menu, fixed at build time.
    #[must_use]
    pub fn slots(&self) -> &[SlotType] {
        &self.slots
    }

    /// The item carried by the cursor.
    #[must_use]
    pub const fn carried(&self) -> &ItemStack {
        &self.carried
    }

    /// The item carried by the cursor.
    #[must_use]
    pub const fn carried_mut(&mut self) -> &mut ItemStack {
        &mut self.carried
    }

    /// The container ID (0 for the player inventory).
    #[must_use]
    pub const fn container_id(&self) -> u8 {
        self.container_id
    }

    /// The menu type, or `None` for the player's own inventory.
    #[must_use]
    pub const fn menu_type(&self) -> Option<MenuTypeRef> {
        self.menu_type
    }

    /// The current state ID.
    #[must_use]
    pub const fn state_id(&self) -> u32 {
        self.state_id
    }

    /// The kind of drag in progress, or `None` when idle.
    #[must_use]
    pub const fn quickcraft(&self) -> Option<DragKind> {
        self.quickcraft
    }

    /// The identity stamp of the menu this behavior belongs to.
    pub(crate) const fn instance(&self) -> MenuInstanceId {
        self.instance
    }

    /// Adds a data slot to the menu with an initial value.
    /// Returns the index of the added data slot.
    pub(crate) fn add_data_slot(&mut self, initial_value: i16) -> usize {
        let index = self.data_slots.len();
        self.data_slots.push(initial_value);
        self.remote_data_slots.push(0);
        index
    }

    /// Gets the value of a data slot.
    #[must_use]
    pub fn get_data(&self, index: usize) -> Option<i16> {
        self.data_slots.get(index).copied()
    }

    /// Sets the value of a data slot.
    pub fn set_data(&mut self, index: usize, value: i16) {
        if let Some(slot) = self.data_slots.get_mut(index) {
            *slot = value;
        }
    }

    /// Resets the quickcraft state.
    pub fn reset_quick_craft(&mut self) {
        self.quickcraft = None;
        self.quickcraft_slots.clear();
    }

    /// Writes a quick-move remainder back to its source slot with Vanilla's
    /// `setByPlayer`/`setChanged` callback split. Fake slots are recomputed,
    /// so only their change notification fires.
    pub(crate) fn update_quick_move_source(
        &self,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        remaining: &ItemStack,
        previous: &ItemStack,
    ) {
        let slot = &self.slots[slot_index];
        if remaining.is_empty() {
            slot.set_by_player(guard, ItemStack::empty(), previous);
        } else {
            if !slot.is_fake() {
                *slot.get_item_mut(guard) = remaining.clone();
            }
            slot.set_changed(guard);
        }
    }

    /// Moves items from `item_stack` to slots in the range [`start_slot`, `end_slot`),
    /// walking the range in `direction`. Returns true if any items were moved.
    ///
    /// This is used by `quick_move_stack` implementations to distribute items.
    /// Based on Java's `AbstractContainerMenu::moveItemStackTo`.
    pub fn move_item_stack_to(
        &self,
        guard: &mut ContainerLockGuard,
        item_stack: &mut ItemStack,
        start_slot: usize,
        end_slot: usize,
        direction: FillDirection,
    ) -> bool {
        let backwards = direction == FillDirection::Backward;
        let mut anything_changed = false;

        // First pass: try to stack with existing items
        if item_stack.is_stackable() {
            let mut dest_slot = if backwards { end_slot - 1 } else { start_slot };

            while !item_stack.is_empty() {
                if backwards {
                    if dest_slot < start_slot {
                        break;
                    }
                } else if dest_slot >= end_slot {
                    break;
                }

                let slot = &self.slots[dest_slot];
                let target = slot.get_item(guard).clone();

                if !target.is_empty()
                    && ItemStack::is_same_item_same_components(item_stack, &target)
                {
                    let total_stack = target.count + item_stack.count;
                    let max_stack_size = slot.get_max_stack_size_for_item(guard, &target);

                    if total_stack <= max_stack_size {
                        item_stack.set_count(0);
                        slot.get_item_mut(guard).set_count(total_stack);
                        slot.set_changed(guard);
                        anything_changed = true;
                    } else if target.count < max_stack_size {
                        item_stack.shrink(max_stack_size - target.count);
                        slot.get_item_mut(guard).set_count(max_stack_size);
                        slot.set_changed(guard);
                        anything_changed = true;
                    }
                }

                if backwards {
                    if dest_slot == 0 {
                        break;
                    }
                    dest_slot -= 1;
                } else {
                    dest_slot += 1;
                }
            }
        }

        // Second pass: place in empty slots
        if !item_stack.is_empty() {
            let mut dest_slot = if backwards { end_slot - 1 } else { start_slot };

            while if backwards {
                dest_slot >= start_slot
            } else {
                dest_slot < end_slot
            } {
                let slot = &self.slots[dest_slot];
                let target = slot.get_item(guard).clone();

                if target.is_empty() && slot.may_place(item_stack) {
                    let max_stack_size = slot.get_max_stack_size_for_item(guard, item_stack);
                    let to_place = item_stack.count.min(max_stack_size);
                    slot.set_by_player(guard, item_stack.split(to_place), &ItemStack::empty());
                    slot.set_changed(guard);
                    anything_changed = true;
                    break;
                }

                if backwards {
                    if dest_slot == 0 {
                        break;
                    }
                    dest_slot -= 1;
                } else {
                    dest_slot += 1;
                }
            }
        }

        anything_changed
    }

    /// Suppresses remote updates during click handling.
    /// Call this before processing a click.
    pub const fn suppress_remote_updates(&mut self) {
        self.suppress_remote_updates = true;
    }

    /// Resumes remote updates after click handling.
    /// Call this after processing a click.
    pub const fn resume_remote_updates(&mut self) {
        self.suppress_remote_updates = false;
    }

    /// Transfers remote slot state from another menu to this one.
    ///
    /// When a container menu is closed, the inventory menu needs to know what
    /// the client thinks it has in the shared slots (player inventory). Without
    /// this transfer, the inventory menu would think the client has stale data
    /// and would try to resync slots that are actually correct.
    ///
    /// This matches slots by their (`container_id`, `container_slot`) pair, so only
    /// slots that reference the same underlying container position will transfer.
    ///
    /// Based on Java's `AbstractContainerMenu::transferState`.
    pub fn transfer_state(&mut self, other: &MenuBehavior) {
        use rustc_hash::FxHashMap;

        // Build a map of (container_id, container_slot) -> slot_index for the other menu
        let mut other_slots: FxHashMap<(ContainerId, usize), usize> = FxHashMap::default();
        for (slot_index, slot) in other.slots.iter().enumerate() {
            if let Some(key) = slot.container_key() {
                other_slots.insert(key, slot_index);
            }
        }

        for (slot_index, slot) in self.slots.iter().enumerate() {
            if let Some(key) = slot.container_key()
                && let Some(&other_slot_index) = other_slots.get(&key)
            {
                self.remote_slots[slot_index] = other.remote_slots[other_slot_index].clone();
            }
        }
    }

    /// The number of slots in this menu.
    #[must_use]
    pub const fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Increments and returns the new state ID.
    const fn increment_state_id(&mut self) -> u32 {
        self.state_id = self.state_id.wrapping_add(1) & 0x7FFF; // Keep it within 15 bits
        self.state_id
    }

    /// Encodes and sends a packet through the connection.
    fn send_packet<P: ClientPacket>(connection: &Arc<PlayerConnection>, packet: P) {
        let encoded =
            EncodedPacket::from_bare(packet, connection.compression(), ConnectionProtocol::Play)
                .expect("Failed to encode packet");
        connection.send_encoded(encoded);
    }

    /// Sends every slot, the carried item and all data slots to the client and
    /// marks them synced. Used on menu open and whenever the client is known
    /// to be stale (state id mismatch).
    pub fn send_all_data_to_remote(&mut self, connection: &Arc<PlayerConnection>) {
        let guard = self.lock_all_containers();

        let items: Vec<ItemStack> = self
            .slots
            .iter()
            .map(|slot| slot.get_item(&guard).clone())
            .collect();
        drop(guard);
        let state_id = self.increment_state_id();

        for (remote, item) in self.remote_slots.iter_mut().zip(&items) {
            remote.force(item);
        }
        self.remote_carried.force(&self.carried);

        let packet = CContainerSetContent {
            container_id: i32::from(self.container_id),
            state_id: state_id as i32,
            items,
            carried_item: self.carried.clone(),
        };

        Self::send_packet(connection, packet);

        // Send all data slots
        for i in 0..self.data_slots.len() {
            self.remote_data_slots[i] = self.data_slots[i];
            let packet = CContainerSetData {
                container_id: i32::from(self.container_id),
                id: i as i16,
                value: self.data_slots[i],
            };
            Self::send_packet(connection, packet);
        }
    }

    /// Syncs only the changed slots, carried item and data slots to the
    /// client. Called once per tick.
    ///
    /// Based on Java's `AbstractContainerMenu::broadcastChanges`.
    /// Slot content packets increment `state_id`, matching vanilla's
    /// `ContainerSynchronizer::sendSlotChange`.
    pub fn broadcast_changes(&mut self, connection: &Arc<PlayerConnection>) {
        let guard = self.lock_all_containers();

        let mut changed: Vec<(usize, ItemStack)> = Vec::new();
        for index in 0..self.slots.len() {
            let item = self.slots[index].get_item(&guard);
            if self.remote_slots[index].matches(item) {
                // A matched hash is cached as Known to avoid re-hashing next tick.
                if matches!(self.remote_slots[index], RemoteSlot::Hashed(_)) {
                    self.remote_slots[index] = RemoteSlot::Known(item.clone());
                }
            } else {
                changed.push((index, item.clone()));
            }
        }
        drop(guard);

        for (index, item) in changed {
            self.synchronize_slot_to_remote(index, item, connection);
        }

        if self.remote_carried.matches(&self.carried) {
            if matches!(self.remote_carried, RemoteSlot::Hashed(_)) {
                self.remote_carried = RemoteSlot::Known(self.carried.clone());
            }
        } else {
            self.synchronize_carried_to_remote(connection);
        }

        for i in 0..self.data_slots.len() {
            self.synchronize_data_slot_to_remote(i, connection);
        }
    }

    /// Sends a data slot update to the client if it has changed.
    /// Based on Java's `AbstractContainerMenu::synchronizeDataSlotToRemote`.
    fn synchronize_data_slot_to_remote(
        &mut self,
        index: usize,
        connection: &Arc<PlayerConnection>,
    ) {
        if self.suppress_remote_updates || index >= self.data_slots.len() {
            return;
        }

        let current = self.data_slots[index];
        let remote = self.remote_data_slots[index];

        if current != remote {
            self.remote_data_slots[index] = current;
            let packet = CContainerSetData {
                container_id: i32::from(self.container_id),
                id: index as i16,
                value: current,
            };
            Self::send_packet(connection, packet);
        }
    }

    /// Sends a single slot update to the client and records the sent stack as
    /// the client's perception.
    /// Based on Java's `AbstractContainerMenu::synchronizeSlotToRemote`.
    fn synchronize_slot_to_remote(
        &mut self,
        slot: usize,
        item: ItemStack,
        connection: &Arc<PlayerConnection>,
    ) {
        if self.suppress_remote_updates {
            return;
        }

        let state_id = self.increment_state_id();

        let packet = CContainerSetSlot {
            container_id: i32::from(self.container_id),
            state_id: state_id as i32,
            slot: slot as i16,
            item_stack: item.clone(),
        };

        Self::send_packet(connection, packet);
        self.remote_slots[slot] = RemoteSlot::Known(item);
    }

    /// Sends the carried item (cursor) to the client.
    /// Based on Java's `AbstractContainerMenu::synchronizeCarriedToRemote`.
    fn synchronize_carried_to_remote(&mut self, connection: &Arc<PlayerConnection>) {
        if self.suppress_remote_updates {
            return;
        }

        let packet = CSetCursorItem {
            item_stack: self.carried.clone(),
        };

        Self::send_packet(connection, packet);
        self.remote_carried.force(&self.carried);
    }

    /// Sets a remote slot to a known `ItemStack`.
    /// Called when we know exactly what the client has (e.g., creative mode set).
    /// Based on Java's `AbstractContainerMenu::setRemoteSlot`.
    pub fn set_remote_slot_known(&mut self, slot: usize, item: &ItemStack) {
        if slot < self.remote_slots.len() {
            self.remote_slots[slot].force(item);
        }
    }

    /// Forgets what the client has in `slot`, forcing a resync on the next broadcast.
    pub fn mark_remote_slot_unknown(&mut self, slot: usize) {
        if slot < self.remote_slots.len() {
            self.remote_slots[slot] = RemoteSlot::Unknown;
        }
    }

    /// Handles the client reporting its own perception of a slot.
    /// Based on Java's `AbstractContainerMenu::setRemoteSlotUnsafe`.
    pub fn set_remote_slot(&mut self, slot: usize, hash: HashedStack) {
        if slot < self.remote_slots.len() {
            self.remote_slots[slot].receive(hash);
        } else {
            log::debug!(
                "Incorrect slot index: {} available slots: {}",
                slot,
                self.remote_slots.len()
            );
        }
    }

    /// Handles a remote carried update from the client.
    /// Based on Java's `AbstractContainerMenu::setRemoteCarried`.
    pub fn set_remote_carried(&mut self, hash: HashedStack) {
        self.remote_carried.receive(hash);
    }

    /// Handles one phase of a quickcraft (drag) operation; `can_drag_to` is
    /// the kind's per-slot veto.
    /// Based on Java's `AbstractContainerMenu::doClick` for `ClickType.QUICK_CRAFT`.
    pub(crate) fn do_quick_craft(
        &mut self,
        action: QuickCraft,
        has_infinite_materials: bool,
        player: &Player,
        can_drag_to: &impl Fn(usize) -> bool,
    ) {
        // Validate the phase against the state machine position: a drag must
        // go Start -> AddSlot* -> End.
        let valid_transition = match action {
            QuickCraft::Start { .. } => self.quickcraft.is_none(),
            QuickCraft::AddSlot { .. } | QuickCraft::End { .. } => self.quickcraft.is_some(),
        };
        if !valid_transition {
            self.reset_quick_craft();
            return;
        }

        // Must have items to drag
        if self.carried.is_empty() {
            self.reset_quick_craft();
            return;
        }

        match action {
            QuickCraft::Start { kind } => {
                // A clone (middle-click) drag requires creative mode.
                if kind == DragKind::Clone && !has_infinite_materials {
                    self.reset_quick_craft();
                    return;
                }
                self.quickcraft = Some(kind);
                self.quickcraft_slots.clear();
            }
            QuickCraft::AddSlot {
                slot: slot_index, ..
            } => {
                let slot = &self.slots[slot_index];

                let guard = self.lock_all_containers();
                let slot_item = slot.get_item(&guard).clone();

                if can_item_quick_replace(&slot_item, &self.carried, true)
                    && slot.may_place(&self.carried)
                    && (self.quickcraft == Some(DragKind::Clone)
                        || self.carried.count > self.quickcraft_slots.len() as i32)
                    && can_drag_to(slot_index)
                    && !self.quickcraft_slots.contains(&slot_index)
                {
                    self.quickcraft_slots.push(slot_index);
                }
            }
            QuickCraft::End { .. } => self.finish_quick_craft(player, can_drag_to),
        }
    }

    /// Distributes the carried items over the dragged slots and resets the
    /// drag state (the `End` phase of [`MenuBehavior::do_quick_craft`]).
    fn finish_quick_craft(&mut self, player: &Player, can_drag_to: &impl Fn(usize) -> bool) {
        let Some(kind) = self.quickcraft else {
            self.reset_quick_craft();
            return;
        };
        // Finishing the drag - distribute items
        if !self.quickcraft_slots.is_empty() {
            if self.quickcraft_slots.len() == 1 {
                // Only one slot - treat as a regular pickup click
                let slot = self.quickcraft_slots[0];
                self.reset_quick_craft();
                // A left drag places like a left click; right and clone
                // drags act as secondary (matching Java's ClickAction).
                let button = if kind == DragKind::Left {
                    MouseButton::Left
                } else {
                    MouseButton::Right
                };
                self.do_pickup(slot, button, player);
                return;
            }

            let source = self.carried.clone();
            if source.is_empty() {
                self.reset_quick_craft();
                return;
            }

            let mut remaining = self.carried.count;
            let quickcraft_slots = self.quickcraft_slots.clone();

            let mut guard = self.lock_all_containers();

            for &slot_index in &quickcraft_slots {
                let slot = &self.slots[slot_index];
                let slot_item = slot.get_item(&guard).clone();

                if can_item_quick_replace(&slot_item, &self.carried, true)
                    && slot.may_place(&self.carried)
                    && (kind == DragKind::Clone
                        || self.carried.count >= quickcraft_slots.len() as i32)
                    && can_drag_to(slot_index)
                {
                    let current_count = if slot_item.is_empty() {
                        0
                    } else {
                        slot_item.count
                    };
                    let max_size = source
                        .max_stack_size()
                        .min(slot.get_max_stack_size_for_item(&guard, &source));
                    let place_count = kind.place_count(quickcraft_slots.len(), &source);
                    let new_count = (place_count + current_count).min(max_size);
                    remaining -= new_count - current_count;

                    let mut new_item = source.clone();
                    new_item.set_count(new_count);
                    slot.set_by_player(&mut guard, new_item, &slot_item);
                }
            }

            let mut new_carried = source;
            new_carried.set_count(remaining);
            self.carried = new_carried;
        }

        self.reset_quick_craft();
    }

    /// Drops the carried stack when the player clicks outside the window.
    /// Based on the `slotId == -999` branch of Java's
    /// `AbstractContainerMenu::doClick` for `ClickType.PICKUP`.
    pub(crate) fn drop_carried(&mut self, button: MouseButton, player: &Player) {
        if self.carried.is_empty() {
            return;
        }
        match button {
            MouseButton::Left => {
                // Left click outside - drop all carried items
                let to_drop = mem::take(&mut self.carried);
                let _ = player.drop_item(to_drop, false, true);
            }
            MouseButton::Right => {
                // Right click outside - drop one carried item
                let _ = player.drop_item(self.carried.split(1), false, true);
            }
        }
    }

    /// Handles pickup click (left/right click to pick up or place items).
    /// Based on Java's `AbstractContainerMenu::doClick` for `ClickType.PICKUP`.
    pub(crate) fn do_pickup(&mut self, slot_index: usize, button: MouseButton, player: &Player) {
        let mut guard = self.lock_all_containers();

        let slot = &self.slots[slot_index];

        // Get the current item in the slot
        let slot_item = slot.get_item(&guard).clone();
        let carried = mem::take(&mut self.carried);

        if slot_item.is_empty() {
            // Slot is empty - place carried items (if allowed)
            if !carried.is_empty() && slot.may_place(&carried) {
                let requested = if button == MouseButton::Left {
                    carried.count
                } else {
                    1
                };
                self.carried = slot.safe_insert(&mut guard, carried, requested);
            } else {
                // Can't place - keep carrying
                self.carried = carried;
            }
        } else if carried.is_empty() {
            // Carried is empty - pick up from slot (if allowed)
            // Use try_remove which enforces allow_modification rules
            // (result slots must be picked up in full, not partially)
            let amount = if button == MouseButton::Left {
                slot_item.count
            } else {
                (slot_item.count + 1) / 2
            };

            // max_amount is i32::MAX for primary action (take all requested)
            // For result slots, try_remove will reject partial takes
            if let Some(taken) = slot.try_remove(&mut guard, amount, i32::MAX, player) {
                if let Some(remainder) = slot.on_take(&mut guard, &taken, player) {
                    // There's a remainder from crafting - add to player inventory or drop
                    player.add_item_or_drop_with_guard(&mut guard, remainder);
                }
                self.carried = taken;
            }
        } else if ItemStack::is_same_item_same_components(&slot_item, &carried) {
            // Same item type - try to stack (if slot allows this item type)
            if slot.may_pickup(&guard, player) && slot.may_place(&carried) {
                let requested = if button == MouseButton::Left {
                    carried.count
                } else {
                    1
                };
                self.carried = slot.safe_insert(&mut guard, carried, requested);
            } else {
                // Can't place this item type in this slot
                // In Java, if items are same type but may_place fails, try to take from slot
                if slot.may_pickup(&guard, player) {
                    // Try to add slot items to carried stack
                    let space = carried.max_stack_size() - carried.count;
                    if space > 0 {
                        if let Some(taken) =
                            slot.try_remove(&mut guard, slot_item.count, space, player)
                        {
                            if let Some(remainder) = slot.on_take(&mut guard, &taken, player) {
                                player.add_item_or_drop_with_guard(&mut guard, remainder);
                            }
                            let mut new_carried = carried;
                            new_carried.grow(taken.count);
                            self.carried = new_carried;
                        } else {
                            self.carried = carried;
                        }
                    } else {
                        self.carried = carried;
                    }
                } else {
                    self.carried = carried;
                }
            }
        } else {
            // Different items - swap (if both operations are allowed)
            if slot.may_pickup(&guard, player) && slot.may_place(&carried) {
                if carried.count <= slot.get_max_stack_size_for_item(&guard, &carried) {
                    slot.set_by_player(&mut guard, carried, &slot_item);
                    self.carried = slot_item;
                } else {
                    self.carried = carried;
                }
            } else {
                self.carried = carried;
            }
        }

        slot.set_changed(&mut guard);
    }

    /// Handles clone (middle-click in creative).
    pub(crate) fn do_clone(&mut self, slot_index: usize, has_infinite_materials: bool) {
        if !has_infinite_materials || !self.carried.is_empty() {
            return;
        }

        let guard = self.lock_all_containers();
        let slot = &self.slots[slot_index];
        let slot_item = slot.get_item(&guard);

        if !slot_item.is_empty() {
            self.carried = slot_item.copy_with_count(slot_item.max_stack_size());
        }
    }

    /// Handles throw (drop key). Q drops a single item; Ctrl+Q
    /// (`whole_stack`) drops the whole stack, repeating while the slot
    /// refills with the same item.
    ///
    /// Based on Java's `AbstractContainerMenu::doClick` for `ClickType.THROW`.
    pub(crate) fn do_throw(&mut self, slot_index: usize, whole_stack: bool, player: &Player) {
        if !self.carried.is_empty() {
            return;
        }

        let mut guard = self.lock_all_containers();
        let slot = &self.slots[slot_index];

        // Check if pickup is allowed (Java's safeTake checks this internally)
        if !slot.may_pickup(&guard, player) {
            return;
        }

        // Java checks player.canDropItems() before each drop
        if !player.can_drop_items() {
            return;
        }

        let amount = if whole_stack {
            slot.get_item(&guard).count
        } else {
            1
        };

        let dropped = slot.safe_take(&mut guard, amount, i32::MAX, player);
        if !dropped.is_empty() {
            let _ = guard.run_unlocked(|| player.drop_item(dropped.clone(), false, true));
        }

        // Ctrl+Q: Keep dropping while the slot has the same item type
        if whole_stack {
            loop {
                // Check may_pickup again for each iteration (Java does this via safeTake)
                if !slot.may_pickup(&guard, player) {
                    break;
                }
                // Java checks player.canDropItems() before each drop
                if !player.can_drop_items() {
                    break;
                }
                let current_item = slot.get_item(&guard).clone();
                if current_item.is_empty() || !ItemStack::is_same_item(&current_item, &dropped) {
                    break;
                }
                let more_dropped = slot.safe_take(&mut guard, current_item.count, i32::MAX, player);
                if more_dropped.is_empty() {
                    break;
                }
                let _ = guard.run_unlocked(|| player.drop_item(more_dropped, false, true));
            }
        }
    }
}

/// What the server believes the client is showing in one slot.
///
/// Starts `Unknown` (always resynced), becomes `Known` when we send the item
/// and `Hashed` when the client reports its own perception. A `Hashed` slot
/// that matches once is cached back to `Known` so later ticks compare
/// structurally instead of re-hashing the components.
#[derive(Debug, Clone, Default)]
pub enum RemoteSlot {
    /// We don't know what the client has (initial state).
    #[default]
    Unknown,
    /// We know the exact `ItemStack` the client should have.
    Known(ItemStack),
    /// We received a hash from the client and verified it matches.
    Hashed(HashedStack),
}

impl RemoteSlot {
    /// Forces the remote slot to a known `ItemStack` state.
    /// Called when we send an item to the client.
    pub fn force(&mut self, item: &ItemStack) {
        *self = Self::Known(item.clone());
    }

    /// Receives a hashed stack from the client.
    /// Called when the client sends us their perception.
    pub fn receive(&mut self, hash: HashedStack) {
        *self = Self::Hashed(hash);
    }

    /// Checks if the remote slot matches the local `ItemStack`.
    #[must_use]
    pub fn matches(&self, local: &ItemStack) -> bool {
        match self {
            Self::Unknown => false,
            Self::Known(remote) => ItemStack::matches(remote, local),
            Self::Hashed(hash) => hashed_stack_matches(hash, local),
        }
    }
}

/// Checks if a hashed stack matches the given `ItemStack`.
fn hashed_stack_matches(hash: &HashedStack, item: &ItemStack) -> bool {
    match hash {
        HashedStack::Empty => {
            if !item.is_empty() {
                log::info!("HashedStack mismatch: client sent Empty, server has {item}");
                return false;
            }
            true
        }
        HashedStack::Item {
            item_id,
            count,
            components,
        } => {
            if item.is_empty() {
                log::info!(
                    "HashedStack mismatch: client sent item_id={item_id} count={count}, server has Empty"
                );
                return false;
            }

            // Check item type and count match
            let local_id = item.item.id() as i32;
            if local_id != *item_id {
                log::info!(
                    "HashedStack mismatch: item_id client={item_id} server={local_id} ({})",
                    item.item.key
                );
                return false;
            }
            if item.count != *count {
                log::info!(
                    "HashedStack mismatch: count client={count} server={} for {}",
                    item.count,
                    item.item.key
                );
                return false;
            }

            // Validate component hashes
            validate_component_hashes(components, item.patch())
        }
    }
}

/// Validates that the hashed component patch matches the local patch.
fn validate_component_hashes(hashed: &HashedPatchMap, patch: &DataComponentPatch) -> bool {
    use rustc_hash::FxHashSet;
    use steel_registry::data_components::ComponentPatchEntry;

    // Check removed components match
    let local_removed: FxHashSet<i32> = patch
        .iter_removed()
        .filter_map(|k| REGISTRY.data_components.id_from_key(k).map(|id| id as i32))
        .collect();
    let hashed_removed: FxHashSet<i32> = hashed.removed_components.iter().copied().collect();

    if local_removed != hashed_removed {
        log::info!(
            "HashedStack mismatch: removed components differ - client={hashed_removed:?} server={local_removed:?}"
        );
        return false;
    }

    // Check added component hashes
    // For each component in our patch, verify the client sent the correct hash
    for (key, entry) in patch.iter() {
        if let ComponentPatchEntry::Set(value) = entry {
            let Some(id) = REGISTRY.data_components.id_from_key(key) else {
                continue; // Unknown component, skip
            };
            let id = id as i32;

            let Some(&expected_hash) = hashed.added_components.get(&id) else {
                // Client didn't send hash for this component
                log::info!(
                    "HashedStack mismatch: client missing hash for component {key} (id={id})"
                );
                return false;
            };

            // Compute the hash of the component value using proper HashOps format
            let Some(component_type) = REGISTRY.data_components.by_id(id as usize) else {
                log::info!("HashedStack mismatch: component {key} has no registry entry");
                return false;
            };
            let Ok(actual_hash) = component_type.compute_hash(value) else {
                log::info!("HashedStack mismatch: component {key} is not persistently hashable");
                return false;
            };

            if actual_hash != expected_hash {
                log::info!(
                    "HashedStack mismatch: component {key} hash differs - client={expected_hash} server={actual_hash}"
                );
                return false;
            }
        }
    }

    // Check that the client didn't send extra components we don't have
    for &id in hashed.added_components.keys() {
        let Some(key) = REGISTRY.data_components.get_key_by_id(id as usize) else {
            log::info!("HashedStack mismatch: client sent unknown component id={id}");
            return false; // Unknown component ID from client
        };
        if !matches!(patch.get_entry(key), Some(ComponentPatchEntry::Set(_))) {
            log::info!(
                "HashedStack mismatch: client claims component {key} exists but server doesn't have it"
            );
            return false; // Client claims component exists but we don't have it
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use std::slice;
    use std::sync::Arc;

    use steel_registry::{item_stack::ItemStack, test_support::init_test_registry, vanilla_items};
    use steel_utils::{DowncastType, DowncastTypeKey, locks::SyncMutex};

    use crate::inventory::{
        container::Container,
        lock::ContainerRef,
        menu::{Menu, builder::MenuBuilder, kind::MenuKindType, kinds::BasicKind},
        slots::{NormalSlot, Slot as _, SlotType},
    };

    struct RecordingContainer {
        item: ItemStack,
        set_item_calls: usize,
        set_changed_calls: usize,
    }

    // SAFETY: This test-only key uniquely identifies `RecordingContainer`.
    unsafe impl DowncastType for RecordingContainer {
        const TYPE_KEY: DowncastTypeKey =
            DowncastTypeKey::new("steel:test/container/quick_move_recording");
    }

    impl Container for RecordingContainer {
        fn items(&self) -> &[ItemStack] {
            slice::from_ref(&self.item)
        }

        fn items_mut(&mut self) -> &mut [ItemStack] {
            slice::from_mut(&mut self.item)
        }

        fn set_item(&mut self, _slot: usize, stack: ItemStack) {
            self.item = stack;
            self.set_item_calls += 1;
        }

        fn set_changed(&mut self) {
            self.set_changed_calls += 1;
        }
    }

    fn recording_menu() -> (Menu, ContainerRef) {
        init_test_registry();
        let container = Arc::new(SyncMutex::new(RecordingContainer {
            item: ItemStack::with_count(&vanilla_items::STONE, 5),
            set_item_calls: 0,
            set_changed_calls: 0,
        }));
        let container_ref = ContainerRef::from(container);
        let mut builder = MenuBuilder::new(None, 1);
        builder.custom_section(
            [SlotType::Normal(NormalSlot::new(container_ref.clone(), 0))],
            [container_ref.clone()],
        );
        (
            builder.build(MenuKindType::Basic(BasicKind {})),
            container_ref,
        )
    }

    #[test]
    fn quick_move_source_persists_cloned_remainder_with_vanilla_callbacks() {
        let (menu, container_ref) = recording_menu();
        let behavior = menu.behavior();
        let container_id = container_ref.container_id();
        let mut guard = behavior.lock_all_containers();
        let previous = ItemStack::with_count(&vanilla_items::STONE, 5);
        let remainder = ItemStack::with_count(&vanilla_items::STONE, 2);

        behavior.update_quick_move_source(&mut guard, 0, &remainder, &previous);
        let state = guard
            .get_typed::<RecordingContainer>(container_id)
            .expect("recording container should remain locked");
        assert_eq!(state.item.count(), 2);
        assert_eq!(state.set_item_calls, 0);
        assert_eq!(state.set_changed_calls, 1);

        behavior.update_quick_move_source(&mut guard, 0, &ItemStack::empty(), &remainder);
        let state = guard
            .get_typed::<RecordingContainer>(container_id)
            .expect("recording container should remain locked");
        assert!(state.item.is_empty());
        assert_eq!(state.set_item_calls, 1);
        assert_eq!(state.set_changed_calls, 2);
    }

    #[test]
    fn safe_insert_uses_set_by_player_before_the_menu_notification() {
        let (menu, container_ref) = recording_menu();
        let behavior = menu.behavior();
        let container_id = container_ref.container_id();
        let mut guard = behavior.lock_all_containers();

        let remainder = behavior.slots()[0].safe_insert(
            &mut guard,
            ItemStack::with_count(&vanilla_items::STONE, 3),
            3,
        );
        behavior.slots()[0].set_changed(&mut guard);

        assert!(remainder.is_empty());
        let state = guard
            .get_typed::<RecordingContainer>(container_id)
            .expect("recording container should remain locked");
        assert_eq!(state.item.count(), 8);
        assert_eq!(state.set_item_calls, 1);
        assert_eq!(state.set_changed_calls, 2);
    }
}
