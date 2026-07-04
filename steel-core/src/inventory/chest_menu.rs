//! The chest menu for chest-like containers (chests, barrels, ender chests, shulker boxes).
//!
//! Supports 1-6 rows of 9 slots each. The slot layout is:
//! - Slots 0 to `rows * 9 - 1`: Container slots
//! - Slots `rows * 9` to `rows * 9 + 26`: Main inventory (27 slots)
//! - Slots `rows * 9 + 27` to `rows * 9 + 35`: Hotbar (9 slots)

use steel_registry::menu_type::MenuTypeRef;
use steel_registry::vanilla_menu_types;

use crate::inventory::{
    FillDirection, MenuBuilder, SyncPlayerInv,
    lock::ContainerRef,
    menu::{Menu, MenuBehavior, MenuKind},
};
use crate::player::Player;

/// Number of slots per row in a chest menu.
pub const SLOTS_PER_ROW: usize = 9;

/// Builds a chest-like menu with `rows` rows of 9 slots plus the player
/// inventory.
///
/// Used for chests (3 rows), double chests (6 rows), barrels (3 rows), ender
/// chests (3 rows) and shulker boxes (3 rows). Based on Java's `ChestMenu`.
///
/// # Panics
/// Panics if `rows` is 0 or greater than 6.
#[must_use]
pub fn chest(
    inventory: SyncPlayerInv,
    container_id: u8,
    container: ContainerRef,
    rows: usize,
) -> Menu {
    assert!(
        (1..=6).contains(&rows),
        "Chest rows must be between 1 and 6"
    );

    let mut builder = MenuBuilder::new(menu_type_for_rows(rows), container_id);
    let chest = builder.section(container.clone(), rows * SLOTS_PER_ROW);
    let player = builder.player_inventory(&inventory);

    // Vanilla ChestMenu treats the player inventory as one block both ways.
    builder.route(chest, [player.all], FillDirection::Backward);
    builder.route(player.all, [chest], FillDirection::Forward);

    builder.build(ChestKind { container })
}

/// Returns the appropriate menu type for the given row count.
///
/// # Panics
/// Panics if `rows` is 0 or greater than 6.
#[must_use]
pub fn menu_type_for_rows(rows: usize) -> MenuTypeRef {
    match rows {
        1 => &vanilla_menu_types::GENERIC_9X1,
        2 => &vanilla_menu_types::GENERIC_9X2,
        3 => &vanilla_menu_types::GENERIC_9X3,
        4 => &vanilla_menu_types::GENERIC_9X4,
        5 => &vanilla_menu_types::GENERIC_9X5,
        6 => &vanilla_menu_types::GENERIC_9X6,
        _ => panic!("Invalid row count: {rows}"),
    }
}

/// The per-menu part of a chest menu: just the backing container, used for the
/// validity check. Carried-item return and shift-click routing are handled by
/// [`Menu`] and the route table built by [`MenuBuilder`](crate::inventory::MenuBuilder).
pub struct ChestKind {
    /// Reference to the container (chest, barrel, etc.).
    container: ContainerRef,
}

impl MenuKind for ChestKind {
    /// Returns true if the container is still valid for interaction.
    ///
    /// Delegates to the container's `still_valid` method.
    ///
    /// Note: Java's `ChestMenu::removed` also calls `container.stopOpen(player)`,
    /// but we don't have that callback implemented yet — so there is no `removed`
    /// override here (the default returns the carried item via [`Menu::removed`]).
    fn still_valid(&self, behavior: &MenuBehavior, player: &Player) -> bool {
        let guard = behavior.lock_all_containers();
        guard
            .get(self.container.container_id())
            .is_some_and(|container| container.still_valid(player))
    }
}
