//! The chest menu for chest-like containers (chests, barrels, ender chests, shulker boxes).
//!
//! Supports 1-6 rows of 9 slots each. The slot layout is:
//! - Slots 0 to `rows * 9 - 1`: Container slots
//! - Slots `rows * 9` to `rows * 9 + 26`: Main inventory (27 slots)
//! - Slots `rows * 9 + 27` to `rows * 9 + 35`: Hotbar (9 slots)

use std::{any::Any, mem};

use steel_registry::item_stack::ItemStack;
use steel_registry::menu_type::MenuTypeRef;
use steel_registry::vanilla_menu_types;

use crate::inventory::{
    BuiltMenu, MenuBuilder, MenuLayout, SyncPlayerInv,
    lock::{ContainerLockGuard, ContainerRef},
    menu::{Menu, MenuBehavior},
    menu_provider::MenuInstance,
};
use crate::player::Player;

/// Number of slots per row in a chest menu.
pub const SLOTS_PER_ROW: usize = 9;

/// A menu for chest-like containers.
///
/// This menu is used for chests (3 rows), double chests (6 rows), barrels (3 rows),
/// ender chests (3 rows), and shulker boxes (3 rows).
///
/// Based on Java's `ChestMenu`.
pub struct ChestMenu {
    behavior: MenuBehavior,
    /// Reference to the container (chest, barrel, etc.).
    container: ContainerRef,
    /// Number of rows in the container (1-6).
    rows: usize,
    /// Section ranges and shift-click routes.
    layout: MenuLayout,
}

impl ChestMenu {
    /// Creates a new chest menu with the specified number of rows.
    ///
    /// # Arguments
    /// * `inventory` - The player's inventory
    /// * `container_id` - The container ID for this menu (1-100)
    /// * `container` - Reference to the container (chest, barrel, etc.)
    /// * `rows` - Number of rows (1-6)
    ///
    /// # Panics
    /// Panics if `rows` is 0 or greater than 6.
    #[must_use]
    pub fn new(
        inventory: SyncPlayerInv,
        container_id: u8,
        container: ContainerRef,
        rows: usize,
    ) -> Self {
        assert!(
            (1..=6).contains(&rows),
            "Chest rows must be between 1 and 6"
        );

        let mut builder = MenuBuilder::new(Self::menu_type_for_rows(rows), container_id);
        let chest = builder.section(container.clone(), rows * SLOTS_PER_ROW);
        let player = builder.player_inventory(&inventory);

        // Vanilla ChestMenu treats the player inventory as one block both ways.
        builder.route(chest, [player.all], true);
        builder.route(player.all, [chest], false);

        let BuiltMenu { behavior, layout } = builder.build();

        Self {
            behavior,
            container,
            rows,
            layout,
        }
    }

    /// Creates a 1-row chest menu.
    #[must_use]
    pub fn one_row(inventory: SyncPlayerInv, container_id: u8, container: ContainerRef) -> Self {
        Self::new(inventory, container_id, container, 1)
    }

    /// Creates a 2-row chest menu.
    #[must_use]
    pub fn two_rows(inventory: SyncPlayerInv, container_id: u8, container: ContainerRef) -> Self {
        Self::new(inventory, container_id, container, 2)
    }

    /// Creates a 3-row chest menu (standard chest, barrel, ender chest, shulker box).
    #[must_use]
    pub fn three_rows(inventory: SyncPlayerInv, container_id: u8, container: ContainerRef) -> Self {
        Self::new(inventory, container_id, container, 3)
    }

    /// Creates a 4-row chest menu.
    #[must_use]
    pub fn four_rows(inventory: SyncPlayerInv, container_id: u8, container: ContainerRef) -> Self {
        Self::new(inventory, container_id, container, 4)
    }

    /// Creates a 5-row chest menu.
    #[must_use]
    pub fn five_rows(inventory: SyncPlayerInv, container_id: u8, container: ContainerRef) -> Self {
        Self::new(inventory, container_id, container, 5)
    }

    /// Creates a 6-row chest menu (double chest).
    #[must_use]
    pub fn six_rows(inventory: SyncPlayerInv, container_id: u8, container: ContainerRef) -> Self {
        Self::new(inventory, container_id, container, 6)
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

    /// Returns the number of rows in this chest menu.
    #[must_use]
    pub const fn rows(&self) -> usize {
        self.rows
    }

    /// Returns a reference to the container.
    #[must_use]
    pub const fn container(&self) -> &ContainerRef {
        &self.container
    }
}

impl Menu for ChestMenu {
    fn behavior(&self) -> &MenuBehavior {
        &self.behavior
    }

    fn behavior_mut(&mut self) -> &mut MenuBehavior {
        &mut self.behavior
    }

    /// Handles shift-click (quick move) for a slot via the declarative routes.
    fn quick_move_stack(
        &mut self,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        player: &Player,
    ) -> ItemStack {
        self.layout
            .quick_move(&self.behavior, guard, slot_index, player)
    }

    /// Returns true if the container is still valid for interaction.
    ///
    /// Delegates to the container's `still_valid` method.
    fn still_valid(&self, player: &Player) -> bool {
        let guard = self.behavior.lock_all_containers();
        guard
            .get(self.container.container_id())
            .is_some_and(|container| container.still_valid(player))
    }

    /// Called when the menu is closed.
    ///
    /// Returns the carried item to the player inventory.
    /// Note: Java's `ChestMenu::removed` also calls `container.stopOpen(player)`,
    /// but we don't have that callback implemented yet.
    fn removed(&mut self, player: &Player) {
        let carried = mem::take(&mut self.behavior.carried);
        if !carried.is_empty() {
            player.add_item_or_drop(carried);
        }
    }
}

impl MenuInstance for ChestMenu {
    fn menu_type(&self) -> MenuTypeRef {
        Self::menu_type_for_rows(self.rows)
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
