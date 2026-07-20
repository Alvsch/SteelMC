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

/// The static layout of a built menu: named section ranges and the shift-click
/// route table. Held alongside [`MenuBehavior`] so a menu can drive a generic
/// [`quick_move`](MenuLayout::quick_move) instead of writing one by hand.
pub(crate) struct MenuLayout {
    pub(crate) routes: Vec<Route>,
    pub(crate) drain_sections: Vec<Range<usize>>,
}

impl MenuLayout {
    /// Returns every item in the [`drain`](MenuBuilder::drain) sections to the
    /// player, emptying those slots. Call from `removed` so input grids hand
    /// their contents back on close instead of deleting them.
    ///
    /// When `return_to_inventory` is false (a dead or disconnected player) the
    /// items are dropped into the world instead, mirroring vanilla's
    /// `clearContainer` guard.
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
                let item = mem::take(behavior.slots[slot_index].get_item_mut(&mut guard));
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

    /// Performs a generic shift-click for `slot_index` using the route table.
    ///
    /// This reproduces the hand-written `quick_move_stack` shape: find the route
    /// whose source contains the clicked slot, move the stack into the first
    /// target that accepts it, write the remainder back, and — for fake result
    /// slots — fire `on_take` (returning any crafting remainder to the player).
    ///
    /// Returns the item that was originally in the slot, or empty if nothing
    /// moved, matching [`Menu::quick_move_stack`](crate::inventory::menu::Menu::quick_move_stack).
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

        let clicked = behavior.slots[slot_index].get_item(guard).clone();
        if clicked.is_empty() {
            return ItemStack::empty();
        }

        // Reject stale pickups — e.g. a result slot whose recipe no longer
        // matches the inputs. Normal slots always allow it.
        if !behavior.slots[slot_index].may_pickup(guard, player) {
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

        let slot = &behavior.slots[slot_index];
        if remaining.is_empty() {
            slot.set_by_player(guard, ItemStack::empty(), &clicked);
        } else {
            // Write the un-moved remainder back to the source. Fake/result slots
            // don't store items (their contents are recomputed), so only touch
            // real slots — otherwise the moved portion would be duplicated.
            if !slot.is_fake() {
                slot.set_item(guard, remaining.clone());
            }
            slot.set_changed(guard);
        }

        // Nothing actually left the slot (e.g. the target was full).
        if remaining.count == clicked.count {
            return ItemStack::empty();
        }

        // Result slots need their take callback to fire (recipe consumption,
        // experience, etc.); the remainder is returned to the player.
        if slot.is_fake() {
            let taken = clicked.copy_with_count(clicked.count - remaining.count);
            if let Some(leftover) = slot.on_take(guard, &taken, player) {
                player.add_item_or_drop_with_guard(guard, leftover);
            }
            // Output that didn't fit in the inventory is dropped, matching
            // vanilla's result-slot shift-click (`player.drop(stack, false)`).
            if !remaining.is_empty() {
                let _ = player.drop_item(remaining.clone(), false, true);
            }
        }

        clicked
    }
}
