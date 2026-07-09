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

use steel_registry::item_stack::ItemStack;

use crate::inventory::menu::behavior::MenuBehavior;
use crate::inventory::menu::kinds::{AnvilKind, BasicKind, ChestKind, CraftingKind, InventoryKind};
use crate::{inventory::lock::ContainerLockGuard, player::Player};

use enum_dispatch::enum_dispatch;

use crate::inventory::click::{Click, ClickOutcome, QuickCraft};

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

    /// Called after the menu is opened and its initial contents have been built,
    /// but before they're sent to the client — so anything populated here appears
    /// in the first render. Use for dynamic population, animations, or a sound.
    /// Bukkit's `InventoryOpenEvent`.
    fn on_open(
        &mut self,
        _behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
    }

    /// Called once per server tick while the menu is open, right before changes
    /// are synced to the client. Use for live/animated menus (timers, updating
    /// icons). Keep it cheap — it runs every tick for every viewer.
    fn on_tick(
        &mut self,
        _behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
    }

    /// Called for every non-drag click before the default handling. Return
    /// [`ClickOutcome::Consume`] to treat the slot as a button and skip the
    /// default pickup/swap/move behavior, or [`ClickOutcome::Fallthrough`] to
    /// let the menu handle it normally. The clicked slot lives inside `click`.
    fn on_slot_clicked(
        &mut self,
        _behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        _click: Click,
        _player: &Player,
    ) -> ClickOutcome {
        ClickOutcome::Fallthrough
    }

    /// Called for each drag (quickcraft) phase before the default handling.
    /// Return [`ClickOutcome::Consume`] to cancel the drag.
    fn on_drag(
        &mut self,
        _behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        _action: QuickCraft,
        _player: &Player,
    ) -> ClickOutcome {
        ClickOutcome::Fallthrough
    }

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
    /// All vanilla menu kind
    Basic(BasicKind),
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

    fn on_open(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        player: &Player,
    ) {
        (**self).on_open(behavior, guard, player);
    }

    fn on_tick(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        player: &Player,
    ) {
        (**self).on_tick(behavior, guard, player);
    }

    fn on_slot_clicked(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        click: Click,
        player: &Player,
    ) -> ClickOutcome {
        (**self).on_slot_clicked(behavior, guard, click, player)
    }
}
