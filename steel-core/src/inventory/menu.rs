//! A menu can be considered everything that's shown on the screen.
//! It consists of slots, slots consist of a view into a single inventory and position.
//! When you have a chest open for example a chest menu is shown, consisting of the chests slots and the players inventory slots.
//!
//! A menu is always the middle man between the server and the client.
//! This means that when the player doesn't have any menus open it actually has, it always has it's own inventory menu open.
//!
//! A menu holds 3 important structures:
//! - All slots for that menu
//! - All slots as cloned itemstacks
//! - The clients perception of the itemstacks
//!
//! This makes it so every time we run a sync (once per tick) we update the cloned itemstacks.
//! This in turn makes it so we can compare it with the clients perception of the itemstacks.
//! And if there are mismatches we can send the correct itemstacks to the client.
//!
//! The client also sends the itemstacks it thinks it has on interaction, so this makes it so we only update the client if they mismatch.

use std::mem;

use steel_protocol::packet_traits::{ClientPacket, EncodedPacket};
use steel_protocol::packets::game::{
    CContainerSetContent, CContainerSetData, CContainerSetSlot, CSetCursorItem, HashedPatchMap,
    HashedStack,
};
use steel_protocol::utils::ConnectionProtocol;
use steel_registry::{
    REGISTRY, RegistryEntry, RegistryExt, data_components::DataComponentPatch,
    item_stack::ItemStack, menu_type::MenuTypeRef,
};
use steel_utils::types::GameType;

use crate::inventory::slots::slot::{Slot, SlotType};
use crate::{
    inventory::lock::{ContainerId, ContainerLockGuard, ContainerRef},
    player::{Player, PlayerConnection, connection::NetworkConnection},
};
use std::sync::Arc;

use enum_dispatch::enum_dispatch;

use crate::inventory::{
    anvil_menu::AnvilKind,
    chest_menu::ChestKind,
    click::{Click, DragKind, MouseButton, QuickCraft, SwapTarget},
    crafting_menu::CraftingKind,
    inventory_menu::InventoryKind,
    menu_builder::{FillDirection, MenuInstanceId, MenuLayout},
};

/// Represents the server's perception of what the client knows about a slot.
///
/// This can be either:
/// - A full `ItemStack` (when we've sent the item to the client)
/// - A `HashedStack` (when we've received a hash from the client)
/// - Unknown (initial state, always needs sync)
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
    /// Creates an unknown remote slot.
    #[must_use]
    pub const fn unknown() -> Self {
        Self::Unknown
    }

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
            let actual_hash = value.compute_hash();

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

/// `QuickCraft` (drag) type constants.
pub const QUICKCRAFT_TYPE_CHARITABLE: i32 = 0; // Left-click drag (distribute evenly)
/// Right-click drag mode (place one item in each slot).
pub const QUICKCRAFT_TYPE_GREEDY: i32 = 1; // Right-click drag (place one each)
/// Middle-click drag mode (creative only, place full stacks).
pub const QUICKCRAFT_TYPE_CLONE: i32 = 2; // Middle-click drag (creative only, full stacks)

/// Number of slots per row in standard inventory grids.
pub const SLOTS_PER_ROW: usize = 9;

/// Standard slot size in pixels (for UI calculations).
pub const SLOT_SIZE: i32 = 18;

/// Calculates how many items to place per slot during quickcraft.
#[must_use]
pub fn get_quickcraft_place_count(
    slot_count: usize,
    quickcraft_type: i32,
    item: &ItemStack,
) -> i32 {
    match quickcraft_type {
        0 => (item.count as f32 / slot_count as f32).floor() as i32, // Distribute evenly
        1 => 1,                                                      // One per slot
        2 => item.max_stack_size(),                                  // Full stack (creative)
        _ => item.count,
    }
}

/// Checks if an item can be quick-placed into a slot.
/// If `ignore_size` is true, doesn't check if the combined count would exceed max stack size.
#[must_use]
pub fn can_item_quick_replace(
    slot_item: &ItemStack,
    carried: &ItemStack,
    ignore_size: bool,
) -> bool {
    let slot_is_empty = slot_item.is_empty();
    if slot_is_empty {
        return true;
    }
    if !ItemStack::is_same_item_same_components(carried, slot_item) {
        return false;
    }
    let combined = slot_item.count + if ignore_size { 0 } else { carried.count };
    combined <= carried.max_stack_size()
}

/// Shared behavior and state for all menu types.
pub struct MenuBehavior {
    /// The slots in this menu.
    pub slots: Vec<SlotType>,
    /// Cloned itemstacks from the actual slots (updated each sync).
    pub last_slots: Vec<ItemStack>,
    /// The client's perception of the itemstacks.
    pub remote_slots: Vec<RemoteSlot>,
    /// The item being carried by the cursor.
    pub carried: ItemStack,
    /// The client's perception of the carried item.
    pub remote_carried: RemoteSlot,
    /// The container ID (0 for player inventory).
    pub container_id: u8,
    /// Incremented every time the server and client mismatch.
    pub state_id: u32,
    /// None for player inventory. Some for all other menus.
    pub menu_type: Option<MenuTypeRef>,
    /// When true, remote updates are suppressed (during click handling).
    suppress_remote_updates: bool,
    /// Current quickcraft drag type (-1 if not dragging).
    pub quickcraft_type: i32,
    /// Current quickcraft status/phase (0 = not started, 1 = adding slots, 2 = ending).
    pub quickcraft_status: i32,
    /// Slots involved in the current quickcraft operation.
    pub quickcraft_slots: Vec<usize>,
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
            last_slots: vec![ItemStack::empty(); slot_count],
            remote_slots: vec![RemoteSlot::Unknown; slot_count],
            carried: ItemStack::empty(),
            remote_carried: RemoteSlot::Unknown,
            container_id,
            state_id: 0,
            menu_type,
            suppress_remote_updates: false,
            quickcraft_type: -1,
            quickcraft_status: 0,
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
        self.quickcraft_status = 0;
        self.quickcraft_slots.clear();
    }

    /// Returns true if a slot can be dragged to during quickcraft.
    /// Menus can override this via the Menu trait.
    #[expect(clippy::unused_self, reason = "this is an api function")]
    #[must_use]
    pub const fn can_drag_to(&self, _slot_index: usize) -> bool {
        true
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

    /// Returns the current state ID.
    #[must_use]
    pub const fn get_state_id(&self) -> u32 {
        self.state_id
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

        // Transfer state for matching slots
        for (slot_index, slot) in self.slots.iter().enumerate() {
            if let Some(key) = slot.container_key()
                && let Some(&other_slot_index) = other_slots.get(&key)
            {
                // Transfer last_slots (the cached item state)
                self.last_slots[slot_index] = other.last_slots[other_slot_index].clone();
                // Transfer remote_slots (client's perception)
                self.remote_slots[slot_index] = other.remote_slots[other_slot_index].clone();
            }
        }
    }

    /// Returns the number of slots in this menu.
    #[must_use]
    pub const fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Gets a reference to a slot by index.
    #[must_use]
    pub fn get_slot(&self, index: usize) -> Option<&SlotType> {
        self.slots.get(index)
    }

    /// Gets the carried item (cursor).
    #[must_use]
    pub const fn get_carried(&self) -> &ItemStack {
        &self.carried
    }

    /// Sets the carried item (cursor).
    pub fn set_carried(&mut self, item: ItemStack) {
        self.carried = item;
    }

    /// Increments and returns the new state ID.
    pub const fn increment_state_id(&mut self) -> u32 {
        self.state_id = self.state_id.wrapping_add(1) & 0x7FFF; // Keep it within 15 bits
        self.state_id
    }

    /// Updates `last_slots` from actual slot contents.
    /// Call this once per tick before comparing with `remote_slots`.
    pub fn update_last_slots(&mut self, guard: &ContainerLockGuard) {
        for (i, slot) in self.slots.iter().enumerate() {
            self.last_slots[i] = slot.get_item(guard).clone();
        }
    }

    /// Checks if a slot has changed compared to remote perception.
    /// Returns true if slot needs to be synced to client.
    #[must_use]
    pub fn slot_needs_sync(&self, index: usize) -> bool {
        if index >= self.last_slots.len() || index >= self.remote_slots.len() {
            return false;
        }
        !self.remote_slots[index].matches(&self.last_slots[index])
    }

    /// Marks a slot as synced (updates remote perception).
    pub fn mark_slot_synced(&mut self, index: usize) {
        if index < self.last_slots.len() && index < self.remote_slots.len() {
            self.remote_slots[index].force(&self.last_slots[index]);
        }
    }

    /// Checks if carried item needs sync.
    #[must_use]
    pub fn carried_needs_sync(&self) -> bool {
        !self.remote_carried.matches(&self.carried)
    }

    /// Marks carried as synced.
    pub fn mark_carried_synced(&mut self) {
        self.remote_carried.force(&self.carried);
    }

    /// Encodes and sends a packet through the connection.
    fn send_packet<P: ClientPacket>(connection: &Arc<PlayerConnection>, packet: P) {
        let encoded =
            EncodedPacket::from_bare(packet, connection.compression(), ConnectionProtocol::Play)
                .expect("Failed to encode packet");
        connection.send_encoded(encoded);
    }

    /// Sends all slots and carried item to the client (full sync).
    /// This is called when:
    /// - A menu is first opened
    /// - The client requests a full refresh
    /// - After certain operations that may have desynced the client
    pub fn send_all_data_to_remote(&mut self, connection: &Arc<PlayerConnection>) {
        let guard = self.lock_all_containers();

        // First, update last_slots from actual slot contents
        self.update_last_slots(&guard);
        let state_id = self.increment_state_id();

        // Send full container content
        let packet = CContainerSetContent {
            container_id: i32::from(self.container_id),
            state_id: state_id as i32,
            items: self.last_slots.clone(),
            carried_item: self.carried.clone(),
        };

        Self::send_packet(connection, packet);

        // Mark all slots and carried as synced
        for i in 0..self.last_slots.len() {
            self.remote_slots[i].force(&self.last_slots[i]);
        }
        self.remote_carried.force(&self.carried);

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

    /// Broadcasts changes to the client (incremental sync).
    /// This is called every tick to sync only changed slots.
    ///
    /// Based on Java's `AbstractContainerMenu::broadcastChanges`.
    /// Slot content packets increment `state_id`, matching vanilla's
    /// `ContainerSynchronizer::sendSlotChange`.
    pub fn broadcast_changes(&mut self, connection: &Arc<PlayerConnection>) {
        let guard = self.lock_all_containers();

        // Update last_slots from actual slot contents
        self.update_last_slots(&guard);

        // Check each slot for changes
        for i in 0..self.last_slots.len() {
            if self.slot_needs_sync(i) {
                self.synchronize_slot_to_remote(i, connection);
            }
        }

        // Check carried item
        if self.carried_needs_sync() {
            self.synchronize_carried_to_remote(connection);
        }

        // Check data slots for changes
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

    /// Sends a single slot update to the client.
    /// Based on Java's `AbstractContainerMenu::synchronizeSlotToRemote`.
    fn synchronize_slot_to_remote(&mut self, slot: usize, connection: &Arc<PlayerConnection>) {
        if self.suppress_remote_updates || slot >= self.last_slots.len() {
            return;
        }

        let item = self.last_slots[slot].clone();
        let state_id = self.increment_state_id();

        let packet = CContainerSetSlot {
            container_id: i32::from(self.container_id),
            state_id: state_id as i32,
            slot: slot as i16,
            item_stack: item,
        };

        Self::send_packet(connection, packet);
        self.mark_slot_synced(slot);
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
        self.mark_carried_synced();
    }

    /// Sets a remote slot to a known `ItemStack`.
    /// Called when we know exactly what the client has (e.g., creative mode set).
    /// Based on Java's `AbstractContainerMenu::setRemoteSlot`.
    pub fn set_remote_slot_known(&mut self, slot: usize, item: &ItemStack) {
        if slot < self.remote_slots.len() {
            self.remote_slots[slot].force(item);
        }
    }

    /// Handles a remote slot update from the client.
    /// This is called when the client sends us their perception of a slot.
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

    /// Broadcasts full state to client.
    /// This triggers slot listeners for all slots and then sends all data to remote.
    /// Based on Java's `AbstractContainerMenu::broadcastFullState`.
    ///
    /// Note: This does NOT increment `state_id` - it just forces a full resync.
    pub fn broadcast_full_state(&mut self, connection: &Arc<PlayerConnection>) {
        self.send_all_data_to_remote(connection);
    }

    /// Handles one phase of a quickcraft (drag) operation.
    /// Based on Java's `AbstractContainerMenu::doClick` for `ClickType.QUICK_CRAFT`.
    pub(crate) fn do_quick_craft(
        &mut self,
        action: QuickCraft,
        has_infinite_materials: bool,
        player: &Player,
    ) {
        // Validate the phase against the state machine position: a drag must
        // go Start -> AddSlot* -> End (`quickcraft_status` 0 -> 1 -> reset).
        let valid_transition = match action {
            QuickCraft::Start { .. } => self.quickcraft_status == 0,
            QuickCraft::AddSlot { .. } | QuickCraft::End { .. } => self.quickcraft_status == 1,
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
                self.quickcraft_type = kind.quickcraft_type();
                self.quickcraft_status = 1;
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
                    && (self.quickcraft_type == QUICKCRAFT_TYPE_CLONE
                        || self.carried.count > self.quickcraft_slots.len() as i32)
                    && self.can_drag_to(slot_index)
                    && !self.quickcraft_slots.contains(&slot_index)
                {
                    self.quickcraft_slots.push(slot_index);
                }
            }
            QuickCraft::End { .. } => self.finish_quick_craft(player),
        }
    }

    /// Distributes the carried items over the dragged slots and resets the
    /// drag state (the `End` phase of [`MenuBehavior::do_quick_craft`]).
    fn finish_quick_craft(&mut self, player: &Player) {
        // Finishing the drag - distribute items
        if !self.quickcraft_slots.is_empty() {
            if self.quickcraft_slots.len() == 1 {
                // Only one slot - treat as a regular pickup click
                let slot = self.quickcraft_slots[0];
                self.reset_quick_craft();
                // A left drag places like a left click; right and clone
                // drags act as secondary (matching Java's ClickAction).
                let button = if self.quickcraft_type == QUICKCRAFT_TYPE_CHARITABLE {
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
                    && (self.quickcraft_type == QUICKCRAFT_TYPE_CLONE
                        || self.carried.count >= quickcraft_slots.len() as i32)
                    && self.can_drag_to(slot_index)
                {
                    let current_count = if slot_item.is_empty() {
                        0
                    } else {
                        slot_item.count
                    };
                    let max_size = source
                        .max_stack_size()
                        .min(slot.get_max_stack_size_for_item(&guard, &source));
                    let place_count = get_quickcraft_place_count(
                        quickcraft_slots.len(),
                        self.quickcraft_type,
                        &source,
                    );
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
                player.drop_item(to_drop, false, true);
            }
            MouseButton::Right => {
                // Right click outside - drop one carried item
                player.drop_item(self.carried.split(1), false, true);
            }
        }
    }

    /// Handles pickup click (left/right click to pick up or place items).
    /// Based on Java's `AbstractContainerMenu::doClick` for `ClickType.PICKUP`.
    #[expect(
        clippy::too_many_lines,
        reason = "splitting would hurt readability of the click-handling state machine"
    )]
    pub(crate) fn do_pickup(&mut self, slot_index: usize, button: MouseButton, player: &Player) {
        let mut guard = self.lock_all_containers();

        let slot = &self.slots[slot_index];

        // Get the current item in the slot
        let slot_item = slot.get_item(&guard).clone();
        let mut carried = mem::take(&mut self.carried);

        if slot_item.is_empty() {
            // Slot is empty - place carried items (if allowed)
            if !carried.is_empty() && slot.may_place(&carried) {
                let max_for_slot = slot.get_max_stack_size_for_item(&guard, &carried);
                let requested = if button == MouseButton::Left {
                    carried.count
                } else {
                    1
                };
                let amount = requested.min(max_for_slot);

                let to_place = carried.split(amount);
                if !carried.is_empty() {
                    self.carried = carried;
                }

                slot.set_by_player(&mut guard, to_place, &ItemStack::empty());
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
            if slot.may_place(&carried) {
                if button == MouseButton::Left {
                    // Left click - add as many as possible to slot
                    let max = slot.get_max_stack_size_for_item(&guard, &carried);
                    let space = max - slot_item.count;
                    let to_add = space.min(carried.count);

                    if to_add > 0 {
                        slot.get_item_mut(&mut guard)
                            .set_count(slot_item.count + to_add);
                        let remaining = carried.count - to_add;
                        if remaining > 0 {
                            let mut new_carried = carried;
                            new_carried.set_count(remaining);
                            self.carried = new_carried;
                        }
                    } else {
                        self.carried = carried;
                    }
                } else {
                    // Right click - add one to slot
                    let max = slot.get_max_stack_size_for_item(&guard, &carried);
                    if slot_item.count < max {
                        slot.get_item_mut(&mut guard).set_count(slot_item.count + 1);
                        let remaining = carried.count - 1;
                        if remaining > 0 {
                            let mut new_carried = carried;
                            new_carried.set_count(remaining);
                            self.carried = new_carried;
                        }
                    } else {
                        self.carried = carried;
                    }
                }
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
            player.drop_item(dropped.clone(), false, true);
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
                player.drop_item(more_dropped, false, true);
            }
        }
    }
}

/// A menu opened by a player: all the shared click machinery plus one
/// [`MenuKind`].
///
/// This is the single concrete menu type — there is no `trait Menu`. It owns
/// the [`MenuBehavior`] (slots, sync state), the `MenuLayout` (sections,
/// shift-click routes, drain list), and a [`MenuKindType`] which is the only
/// per-menu part (recipe recompute, validity, close cleanup). Every click
/// handler lives here as an inherent method.
pub struct Menu {
    behavior: MenuBehavior,
    layout: MenuLayout,
    kind: MenuKindType,
}

/// The per-menu behavior that isn't shared: recompute-on-change, validity,
/// close cleanup, and the optional shift-click override.
///
/// Every method has a default, so a trivial storage menu needs to implement
/// none of them. Dispatched through [`MenuKindType`] (static dispatch for the
/// vanilla variants, boxed for plugins), mirroring
/// [`SlotType`](crate::inventory::slots::slot::SlotType) /
/// [`ResultHandler`](crate::inventory::slots::ResultHandler).
#[enum_dispatch]
pub trait MenuKind: Send + Sync {
    /// Recompute recipe-driven slots after a slot changed (crafting result,
    /// anvil result). Called after every click that touched a real slot.
    fn slots_changed(
        &mut self,
        _behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
    }

    /// Extra cleanup on close, beyond returning the carried item and draining
    /// the input sections (both handled by [`Menu::removed`]) — e.g. clearing a
    /// virtual result container.
    fn removed(&mut self, _behavior: &mut MenuBehavior, _player: &Player) {}

    /// Returns true if this menu is still valid for the player (backing block
    /// still present, player still in range).
    fn still_valid(&self, _behavior: &MenuBehavior, _player: &Player) -> bool {
        true
    }

    /// Returns true if an item may be taken from `slot_index` during a
    /// double-click pickup-all. Override to protect result slots.
    fn can_take_item_for_pick_all(&self, _carried: &ItemStack, _slot_index: usize) -> bool {
        true
    }

    /// Shift-click override. Return `Some` to fully handle the quick-move (the
    /// inventory menu's armor/offhand auto-equip does this); return `None` to
    /// fall back to the declarative route table (`MenuLayout::quick_move`).
    fn quick_move(
        &mut self,
        _behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        _slot_index: usize,
        _player: &Player,
    ) -> Option<ItemStack> {
        None
    }
}

/// Static dispatch over the vanilla menu kinds, with a boxed escape hatch for
/// plugins. Mirrors [`SlotType`](crate::inventory::slots::slot::SlotType).
#[enum_dispatch(MenuKind)]
pub enum MenuKindType {
    /// The always-open player inventory (2×2 grid, armor, offhand).
    Inventory(InventoryKind),
    /// A chest-like container (chest, barrel, ender chest, shulker box).
    Chest(ChestKind),
    /// A crafting table (3×3 grid + result).
    Crafting(CraftingKind),
    /// An anvil (two inputs + result + level-cost data slot).
    Anvil(AnvilKind),
    /// Plugin-defined menu logic.
    Custom(Box<dyn MenuKind>),
}

// Mirror of `impl Slot for Arc<dyn Slot>` in slot.rs, needed for the `Custom`
// variant. It's `Box`, not `Arc`, because `MenuKind` methods take `&mut self`
// and `Arc` only hands out shared references.
impl MenuKind for Box<dyn MenuKind> {
    fn slots_changed(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        player: &Player,
    ) {
        (**self).slots_changed(behavior, guard, player);
    }

    fn removed(&mut self, behavior: &mut MenuBehavior, player: &Player) {
        (**self).removed(behavior, player);
    }

    fn still_valid(&self, behavior: &MenuBehavior, player: &Player) -> bool {
        (**self).still_valid(behavior, player)
    }

    fn can_take_item_for_pick_all(&self, carried: &ItemStack, slot_index: usize) -> bool {
        (**self).can_take_item_for_pick_all(carried, slot_index)
    }

    fn quick_move(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        player: &Player,
    ) -> Option<ItemStack> {
        (**self).quick_move(behavior, guard, slot_index, player)
    }
}

impl Menu {
    /// Assembles a menu from its parts. Crate-internal: the only way to obtain
    /// a `Menu` is [`MenuBuilder::build`](crate::inventory::MenuBuilder::build),
    /// which guarantees the layout's slot ranges match the behavior's slots.
    pub(crate) const fn from_parts(
        behavior: MenuBehavior,
        layout: MenuLayout,
        kind: MenuKindType,
    ) -> Self {
        Self {
            behavior,
            layout,
            kind,
        }
    }

    /// Returns a reference to the shared menu behavior.
    #[must_use]
    pub const fn behavior(&self) -> &MenuBehavior {
        &self.behavior
    }

    /// Returns a mutable reference to the shared menu behavior.
    pub const fn behavior_mut(&mut self) -> &mut MenuBehavior {
        &mut self.behavior
    }

    /// Returns a reference to this menu's kind.
    #[must_use]
    pub const fn kind(&self) -> &MenuKindType {
        &self.kind
    }

    /// Returns a mutable reference to this menu's kind.
    pub const fn kind_mut(&mut self) -> &mut MenuKindType {
        &mut self.kind
    }

    /// The container ID for this menu (0 for the player inventory).
    #[must_use]
    pub const fn container_id(&self) -> u8 {
        self.behavior.container_id
    }

    /// The menu type for the open-screen packet, or `None` for the player's own
    /// inventory (which is never opened via `open_menu`).
    #[must_use]
    pub const fn menu_type(&self) -> Option<MenuTypeRef> {
        self.behavior.menu_type
    }

    /// Returns true if this menu is still valid for the player.
    #[must_use]
    pub fn still_valid(&self, player: &Player) -> bool {
        self.kind.still_valid(&self.behavior, player)
    }

    /// Returns true if the item can be taken from the slot during pickup all.
    #[must_use]
    pub fn can_take_item_for_pick_all(&self, carried: &ItemStack, slot_index: usize) -> bool {
        self.kind.can_take_item_for_pick_all(carried, slot_index)
    }

    /// Called when the menu is closed/removed. Hands the carried item and the
    /// input sections back to the player, then runs the kind's extra cleanup.
    ///
    /// Mirrors vanilla `AbstractContainerMenu.removed` / `clearContainer`: the
    /// items go back into the inventory only if the player is alive and still
    /// connected, otherwise they are dropped into the world (see
    /// [`Player::returns_menu_items_to_inventory`]).
    pub fn removed(&mut self, player: &Player) {
        let return_to_inventory = player.returns_menu_items_to_inventory();

        let carried = mem::take(&mut self.behavior.carried);
        if !carried.is_empty() {
            if return_to_inventory {
                player.add_item_or_drop(carried);
            } else {
                player.drop_item(carried, false, false);
            }
        }
        self.layout
            .return_drained_items(&self.behavior, player, return_to_inventory);

        let Self { behavior, kind, .. } = self;
        kind.removed(behavior, player);
    }

    /// Applies an anvil rename to this menu; a no-op unless it is an anvil menu.
    ///
    /// Replaces the old `as_any_mut().downcast_mut::<AnvilMenu>()` path with a
    /// plain match on the kind.
    pub fn set_anvil_item_name(&mut self, name: String, player: &Arc<Player>) {
        let Self { behavior, kind, .. } = self;
        if let MenuKindType::Anvil(anvil) = kind {
            anvil.set_item_name(behavior, name, player);
        }
    }

    /// Recomputes recipe-driven slots after a change (delegates to the kind).
    fn slots_changed(&mut self, guard: &mut ContainerLockGuard, player: &Player) {
        let Self { behavior, kind, .. } = self;
        kind.slots_changed(behavior, guard, player);
    }

    /// Handles shift-click (quick move) for a slot: the kind's override if it
    /// provides one, otherwise the declarative route table.
    ///
    /// Returns the item originally in the slot, or empty if nothing was moved.
    /// Based on Java's `AbstractContainerMenu::quickMoveStack`.
    fn quick_move_stack(
        &mut self,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        player: &Player,
    ) -> ItemStack {
        let Self {
            behavior,
            layout,
            kind,
        } = self;
        if let Some(result) = kind.quick_move(behavior, guard, slot_index, player) {
            result
        } else {
            layout.quick_move(behavior, guard, slot_index, player)
        }
    }

    /// Handles a click action in this menu.
    ///
    /// Clicks are parsed and validated at the packet boundary via
    /// [`Click::parse`], so every slot index here is already in range.
    /// Based on Java's `AbstractContainerMenu::clicked` and `doClick`.
    ///
    /// TODO: Add `tryItemClickBehaviorOverride` for bundle item support.
    pub fn clicked(&mut self, click: Click, player: &Player) {
        let has_infinite_materials = player.game_mode() == GameType::Creative;
        if let Click::QuickCraft(action) = click {
            self.behavior_mut()
                .do_quick_craft(action, has_infinite_materials, player);
        } else {
            // Any non-quickcraft click resets quickcraft state if in progress
            if self.behavior().quickcraft_status != 0 {
                self.behavior_mut().reset_quick_craft();
            }
            match click {
                Click::Pickup { slot, button } => {
                    self.behavior_mut().do_pickup(slot, button, player);
                }
                Click::DropCarried { button } => {
                    self.behavior_mut().drop_carried(button, player);
                }
                Click::QuickMove { slot } => {
                    self.do_quick_move(slot, player);
                }
                Click::Swap { slot, with } => {
                    self.do_swap(slot, with, player);
                }
                Click::Clone { slot } => {
                    self.behavior_mut().do_clone(slot, has_infinite_materials);
                }
                Click::Throw { slot, whole_stack } => {
                    self.behavior_mut().do_throw(slot, whole_stack, player);
                }
                Click::PickupAll { slot, direction } => {
                    self.do_pickup_all(slot, direction, player);
                }
                Click::QuickCraft(_) => unreachable!(),
            }
        }
        // Recompute recipe-driven slots after the click. Slot-carrying clicks
        // are in range by construction; a QuickCraft (drag) distributes its
        // items on the end phase without a slot, so recompute on any non-empty
        // menu too — otherwise the result stays stale after a drag-place into
        // a grid.
        let should_recompute = match click {
            Click::DropCarried { .. } => false,
            Click::QuickCraft(_) => !self.behavior().slots.is_empty(),
            _ => true,
        };
        if should_recompute {
            let mut guard = self.behavior().lock_all_containers();
            self.slots_changed(&mut guard, player);
        }
    }

    /// Handles quick move (shift-click).
    /// Based on Java's `AbstractContainerMenu::doClick` for `ClickType.QUICK_MOVE`.
    fn do_quick_move(&mut self, slot_index: usize, player: &Player) {
        let mut guard = self.behavior().lock_all_containers();

        // Check if slot allows pickup
        if !self.behavior().slots[slot_index].may_pickup(&guard, player) {
            return;
        }

        // Get the initial item for comparison
        let initial_item = self.behavior().slots[slot_index].get_item(&guard).clone();
        if initial_item.is_empty() {
            return;
        }

        // Call quick_move_stack in a loop while the item type remains the same
        let mut result = self.quick_move_stack(&mut guard, slot_index, player);

        while !result.is_empty() {
            let current_item = self.behavior().slots[slot_index].get_item(&guard).clone();
            if !ItemStack::is_same_item(&current_item, &result) {
                break;
            }
            result = self.quick_move_stack(&mut guard, slot_index, player);
        }
    }

    /// Handles swap (number keys to swap with a hotbar slot, or the
    /// swap-hands key for the offhand).
    ///
    /// Based on Java's `AbstractContainerMenu::doClick` for `ClickType.SWAP`.
    fn do_swap(&mut self, slot_index: usize, with: SwapTarget, player: &Player) {
        let mut guard = self.behavior().lock_all_containers();

        // Get the player inventory container ID from the player's inventory arc
        let player_inv_id = ContainerId::from_arc(&player.inventory);

        let behavior = self.behavior();
        let target_slot = &behavior.slots[slot_index];
        let inventory_slot = with.inventory_slot();

        // Get items from target slot (menu) and source (player inventory via guard)
        let target_item = target_slot.get_item(&guard).clone();
        let source_item = guard
            .get(player_inv_id)
            .map_or_else(ItemStack::empty, |inv| inv.get_item(inventory_slot).clone());

        if source_item.is_empty() && target_item.is_empty() {
            return;
        }

        if source_item.is_empty() {
            // Move from target to inventory
            if target_slot.may_pickup(&guard, player)
                && let Some(taken) =
                    target_slot.try_remove(&mut guard, target_item.count, i32::MAX, player)
            {
                if let Some(inv) = guard.get_mut(player_inv_id) {
                    inv.set_item(inventory_slot, taken);
                }
                if let Some(remainder) = target_slot.on_take(&mut guard, &target_item, player) {
                    player.add_item_or_drop_with_guard(&mut guard, remainder);
                }
            }
        } else if target_item.is_empty() {
            // Move from inventory to target
            if target_slot.may_place(&source_item) {
                let max_size = target_slot.get_max_stack_size_for_item(&guard, &source_item);
                if source_item.count > max_size {
                    // Split the stack
                    target_slot.set_by_player(
                        &mut guard,
                        source_item.copy_with_count(max_size),
                        &ItemStack::empty(),
                    );
                    if let Some(inv) = guard.get_mut(player_inv_id) {
                        inv.get_item_mut(inventory_slot).shrink(max_size);
                    }
                } else {
                    // Move entire stack
                    if let Some(inv) = guard.get_mut(player_inv_id) {
                        inv.set_item(inventory_slot, ItemStack::empty());
                    }
                    target_slot.set_by_player(&mut guard, source_item, &ItemStack::empty());
                }
            }
        } else {
            // Swap items between target and inventory
            if target_slot.may_pickup(&guard, player) && target_slot.may_place(&source_item) {
                let max_size = target_slot.get_max_stack_size_for_item(&guard, &source_item);
                if source_item.count > max_size {
                    // Source is too big - place partial and add target to inventory
                    target_slot.set_by_player(
                        &mut guard,
                        source_item.copy_with_count(max_size),
                        &target_item,
                    );
                    if let Some(remainder) = target_slot.on_take(&mut guard, &target_item, player) {
                        player.add_item_or_drop_with_guard(&mut guard, remainder);
                    }
                    // Try to add target item to inventory, drop if can't fit
                    if let Some(inv) = guard.get_mut(player_inv_id) {
                        inv.get_item_mut(inventory_slot).shrink(max_size);
                    }
                    player.add_item_or_drop_with_guard(&mut guard, target_item);
                } else {
                    // Simple swap
                    if let Some(inv) = guard.get_mut(player_inv_id) {
                        inv.set_item(inventory_slot, target_item.clone());
                    }
                    target_slot.set_by_player(&mut guard, source_item, &target_item);
                    if let Some(remainder) = target_slot.on_take(&mut guard, &target_item, player) {
                        player.add_item_or_drop_with_guard(&mut guard, remainder);
                    }
                }
            }
        }
    }

    /// Handles pickup all (double-click).
    /// Collects matching items from all slots into the carried stack.
    /// Based on Java's `AbstractContainerMenu::doClick` for `ClickType.PICKUP_ALL`.
    fn do_pickup_all(&mut self, slot_index: usize, direction: FillDirection, player: &Player) {
        let mut guard = self.behavior().lock_all_containers();

        let behavior = self.behavior();
        let slot = &behavior.slots[slot_index];
        let slot_has_item = !slot.get_item(&guard).is_empty();
        let slot_may_pickup = slot.may_pickup(&guard, player);

        // Can only pickup all if carried is not empty and (slot is empty or can't be picked up)
        // Java: if (!carried.isEmpty() && (!slotxx.hasItem() || !slotxx.mayPickup(player)))
        if behavior.carried.is_empty() || (slot_has_item && slot_may_pickup) {
            return;
        }

        let max_stack = behavior.carried.max_stack_size();
        let carried_item = behavior.carried.clone();
        let slot_count = behavior.slots.len();

        // Determine iteration direction (Java uses button == 0 for forward,
        // button == 1 for reverse).
        let (start, step): (i32, i32) = match direction {
            FillDirection::Forward => (0, 1),
            FillDirection::Backward => (slot_count as i32 - 1, -1),
        };

        // Two passes: first collect non-full stacks, then full stacks
        for pass in 0..2 {
            let mut i = start;
            while i >= 0 && i < slot_count as i32 && self.behavior().carried.count < max_stack {
                let target_slot = &self.behavior().slots[i as usize];
                let target_item = target_slot.get_item(&guard).clone();

                // Java checks: target.hasItem() && canItemQuickReplace(target, carried, true)
                //              && target.mayPickup(player) && this.canTakeItemForPickAll(carried, target)
                if !target_item.is_empty()
                    && can_item_quick_replace(&target_item, &carried_item, true)
                    && target_slot.may_pickup(&guard, player)
                    && self.can_take_item_for_pick_all(&carried_item, i as usize)
                {
                    // First pass: skip full stacks; Second pass: include full stacks
                    if pass != 0 || target_item.count != target_item.max_stack_size() {
                        let can_take = max_stack - self.behavior().carried.count;
                        let to_take = target_item.count.min(can_take);
                        let removed = target_slot.safe_take(&mut guard, to_take, can_take, player);
                        self.behavior_mut().carried.grow(removed.count);
                    }
                }

                i += step;
            }
        }
    }
}
