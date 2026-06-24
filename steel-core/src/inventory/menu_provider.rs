//! Menu instances that can be opened by players.

use std::any::Any;

use steel_registry::menu_type::MenuTypeRef;

use crate::inventory::menu::Menu;

/// Trait for menu instances that can be opened by players.
///
/// This extends `Menu` with the additional information needed to send
/// the open screen packet: menu type and container ID.
///
/// Menus are opened via `Player::open_menu`, which takes a title and a factory
/// closure returning a `Box<dyn MenuInstance>` — there is no separate provider
/// trait.
pub trait MenuInstance: Menu + Send + Sync {
    /// Returns the menu type for the open screen packet.
    fn menu_type(&self) -> MenuTypeRef;

    /// Returns the container ID for this menu.
    fn container_id(&self) -> u8;

    /// Returns a reference to the menu as `Any` for downcasting.
    fn as_any(&self) -> &dyn Any;

    /// Returns a mutable reference to the menu as `Any` for downcasting.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
