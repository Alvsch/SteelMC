use std::{mem, range::Range};

use steel_registry::item_stack::ItemStack;

use crate::{
    inventory::{
        lock::ContainerLockGuard,
        menu::{behavior::MenuBehavior, builder::Route},
        slots::Slot,
    },
    player::Player,
};

/// Static layout of a built menu: section ranges and the shift-click route table.
pub(crate) struct MenuLayout {
    pub(crate) routes: Vec<Route>,
    pub(crate) drain_sections: Vec<Range<usize>>,
}

impl MenuLayout {
    /// Returns every item in the drain sections to the player, emptying those slots.
    ///
    /// When `return_to_inventory` is false the items are dropped into the world.
    pub fn return_drained_items(
        &self,
        behavior: &MenuBehavior,
        player: &Player,
        return_to_inventory: bool,
    ) {
        if self.drain_sections.is_empty() {
            return;
        }

        let mut guard = behavior.lock_all_containers();
        for range in &self.drain_sections {
            for slot_index in *range {
                let item = mem::take(behavior.slots()[slot_index].get_item_mut(&mut guard));
                if item.is_empty() {
                    continue;
                }
                if return_to_inventory {
                    player.add_item_or_drop_with_guard(&mut guard, item);
                } else {
                    let _ = player.drop_item(item, false, false);
                }
            }
        }
    }

    /// Generic shift-click for `slot_index` via the route table.
    ///
    /// Returns the item originally in the slot, or empty if nothing moved.
    pub fn quick_move(
        &self,
        behavior: &MenuBehavior,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        player: &Player,
    ) -> ItemStack {
        let Some(route) = self.routes.iter().find(|r| r.from.contains(&slot_index)) else {
            return ItemStack::empty();
        };

        let clicked = behavior.slots()[slot_index].get_item(guard).clone();
        if clicked.is_empty() {
            return ItemStack::empty();
        }

        // Reject stale pickups like a result slot whose recipe no longer matches.
        if !behavior.slots()[slot_index].may_pickup(guard, player) {
            return ItemStack::empty();
        }

        let mut remaining = clicked.clone();
        let moved = route.targets.iter().any(|target| {
            behavior.move_item_stack_to(
                guard,
                &mut remaining,
                target.start,
                target.end,
                route.direction,
            )
        });
        if !moved {
            return ItemStack::empty();
        }

        behavior.update_quick_move_source(guard, slot_index, &remaining, &clicked);

        // Nothing left the slot.
        if remaining.count == clicked.count {
            return ItemStack::empty();
        }

        let slot = &behavior.slots()[slot_index];
        if let Some(leftover) = slot.on_take(guard, &remaining, player) {
            player.add_item_or_drop_with_guard(guard, leftover);
        }
        // Output that didn't fit is dropped.
        if slot.is_fake() && !remaining.is_empty() {
            let _ = guard.run_unlocked(|| player.drop_item(remaining.clone(), false, true));
        }

        clicked
    }
}
