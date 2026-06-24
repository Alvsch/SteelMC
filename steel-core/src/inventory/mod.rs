//! Inventory and container management system.
//!
//! This module provides the core inventory system including containers,
//! menus, crafting, equipment, and recipes.

pub mod anvil_menu;
pub mod chest_menu;
pub mod container;
pub mod crafting;
pub mod crafting_menu;
pub mod equipment;
pub mod inventory_menu;
pub mod lock;
pub mod menu;
pub mod menu_builder;
pub mod menu_provider;
pub mod recipe_manager;
pub mod simple_menu;
pub mod slots;

pub use chest_menu::ChestMenu;
pub use crafting_menu::CraftingMenu;
pub use lock::SyncPlayerInv;
pub use menu_builder::{
    BuiltMenu, DataSlot, MenuBuilder, MenuLayout, PlayerInventorySections, Section,
};
pub use menu_provider::MenuInstance;
