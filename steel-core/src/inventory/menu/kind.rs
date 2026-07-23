//! The [`MenuKind`] hooks and their dispatch enum [`MenuKindType`].

use std::fmt;

use steel_registry::item_stack::ItemStack;

use crate::inventory::menu::behavior::MenuBehavior;
use crate::inventory::menu::kinds::{AnvilKind, BasicKind, ChestKind, CraftingKind, InventoryKind};
use crate::{inventory::lock::ContainerLockGuard, player::Player};

use enum_dispatch::enum_dispatch;

use crate::inventory::click::{Click, ClickOutcome, QuickCraft};

/// Per-menu behavior that isn't shared: recompute-on-change, validity, close
/// cleanup, and the optional shift-click override.
#[enum_dispatch]
pub trait MenuKind: Send + Sync {
    /// Recompute recipe-driven slots after a click touched a real slot.
    fn slots_changed(
        &mut self,
        _behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
    }

    /// Extra cleanup on close beyond [`Menu::removed`].
    fn removed(&mut self, _behavior: &mut MenuBehavior, _player: &Player) {}

    /// Runs after initial contents are built but before they're sent, so
    /// anything populated here appears in the first render.
    fn on_open(
        &mut self,
        _behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
    }

    /// Runs once per tick per viewer while open, before changes are synced.
    fn on_tick(
        &mut self,
        _behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
    }

    /// Runs for every non-drag click before default handling. Return
    /// [`ClickOutcome::Consume`] to treat the slot as a button, or
    /// [`ClickOutcome::Fallthrough`] for default handling.
    fn on_slot_clicked(
        &mut self,
        _behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        _click: Click,
        _player: &Player,
    ) -> ClickOutcome {
        ClickOutcome::Fallthrough
    }

    /// Runs for each drag phase before default handling. Return
    /// [`ClickOutcome::Consume`] to cancel the drag.
    fn on_drag(
        &mut self,
        _behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        _action: QuickCraft,
        _player: &Player,
    ) -> ClickOutcome {
        ClickOutcome::Fallthrough
    }

    /// Returns true if a drag may distribute items into `slot_index`.
    fn can_drag_to(&self, _slot_index: usize) -> bool {
        true
    }

    /// Returns true if this menu is still valid for the player.
    fn still_valid(&self, _behavior: &MenuBehavior, _player: &Player) -> bool {
        true
    }

    /// Returns true if an item may be taken from `slot_index` during pickup-all.
    fn can_take_item_for_pick_all(&self, _carried: &ItemStack, _slot_index: usize) -> bool {
        true
    }

    /// Shift-click override. Return `Some` to fully handle the quick-move, or
    /// `None` to fall back to the route table.
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

/// Static dispatch over vanilla menu kinds, with a boxed escape hatch for plugins.
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
    /// Plain vanilla menu with no per-kind behavior.
    Basic(BasicKind),
    /// Plugin-defined menu logic.
    Custom(Box<dyn MenuKind>),
}

impl MenuKindType {
    /// Wraps a plugin-defined [`MenuKind`] into [`MenuKindType::Custom`].
    #[must_use]
    pub fn custom(kind: impl MenuKind + 'static) -> Self {
        Self::Custom(Box::new(kind))
    }
}

impl fmt::Debug for MenuKindType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Inventory(_) => "Inventory",
            Self::Chest(_) => "Chest",
            Self::Crafting(_) => "Crafting",
            Self::Anvil(_) => "Anvil",
            Self::Basic(_) => "Basic",
            Self::Custom(_) => "Custom",
        };
        f.debug_struct(name).finish_non_exhaustive()
    }
}

// `Box` not `Arc` because `MenuKind` methods take `&mut self`.
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

    fn on_drag(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        action: QuickCraft,
        player: &Player,
    ) -> ClickOutcome {
        (**self).on_drag(behavior, guard, action, player)
    }

    fn can_drag_to(&self, slot_index: usize) -> bool {
        (**self).can_drag_to(slot_index)
    }
}
