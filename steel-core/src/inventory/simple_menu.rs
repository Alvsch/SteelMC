//! A Simple Container

use steel_registry::item_stack::ItemStack;

use crate::inventory::container::Container;

/// A Simple Container
pub struct SimpleContainer {
    items: Vec<ItemStack>,
}

impl SimpleContainer {
    /// Creates a new Simple Container
    #[must_use]
    pub fn new(size: usize) -> Self {
        Self {
            items: vec![ItemStack::empty(); size],
        }
    }

    /// Creates a Simple Container with already initialized items
    #[must_use]
    pub const fn from_items(items: Vec<ItemStack>) -> Self {
        Self { items }
    }
}

impl Container for SimpleContainer {
    fn items(&self) -> &[ItemStack] {
        &self.items
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        &mut self.items
    }

    #[doc = " Marks this container as changed (dirty) for saving/syncing."]
    fn set_changed(&mut self) {}
}
