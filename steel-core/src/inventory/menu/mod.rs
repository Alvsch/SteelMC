//! Contains the Menu API

mod behavior;
mod builder;
mod kind;
pub mod kinds;
mod layout;

pub use behavior::{MenuBehavior, RemoteSlot};
pub use builder::{DataSlot, FillDirection, MenuBuilder, PlayerInventorySections, Section};
pub use kind::{MenuKind, MenuKindType};
pub(crate) use layout::MenuLayout;

use std::mem;

use steel_registry::{item_stack::ItemStack, menu_type::MenuTypeRef};
use steel_utils::types::GameType;

use crate::inventory::slots::slot::Slot;
use crate::{
    inventory::lock::{ContainerId, ContainerLockGuard},
    player::Player,
};
use std::sync::Arc;

use crate::inventory::click::{Click, ClickOutcome, SwapTarget, can_item_quick_replace};

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

impl Menu {
    /// Assembles a menu from its parts. Crate-internal: the only way to obtain
    /// a `Menu` is [`MenuBuilder::build`](crate::inventory::MenuBuilder::build),
    /// which guarantees the layout's slot ranges match the behavior's slots.
    pub(super) const fn from_parts(
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

    /// Runs the kind's `on_open` hook. Called after the menu's contents are
    /// built but before they are sent to the client.
    pub fn on_open(&mut self, player: &Player) {
        let mut guard = self.behavior().lock_all_containers();
        let Self { behavior, kind, .. } = self;
        kind.on_open(behavior, &mut guard, player);
    }

    /// Runs the kind's `on_tick` hook. Called once per server tick while open.
    pub fn on_tick(&mut self, player: &Player) {
        let mut guard = self.behavior().lock_all_containers();
        let Self { behavior, kind, .. } = self;
        kind.on_tick(behavior, &mut guard, player);
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
            let outcome = {
                let mut guard = self.behavior().lock_all_containers();
                let Self { behavior, kind, .. } = self;
                kind.on_drag(behavior, &mut guard, action, player)
            };
            if outcome == ClickOutcome::Consume {
                self.behavior_mut().reset_quick_craft();
            } else {
                self.behavior_mut()
                    .do_quick_craft(action, has_infinite_materials, player);
            }
        } else {
            // Any non-quickcraft click resets quickcraft state if in progress
            if self.behavior().quickcraft.is_some() {
                self.behavior_mut().reset_quick_craft();
            }

            // Menu-defined click hook (buttons). If the menu consumes the click,
            // skip the default pickup/swap/move handling. The guard is dropped
            // before the default arms below, which re-lock the same containers.
            let outcome = {
                let mut guard = self.behavior().lock_all_containers();
                let Self { behavior, kind, .. } = self;
                kind.on_slot_clicked(behavior, &mut guard, click, player)
            };

            if outcome == ClickOutcome::Fallthrough {
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
