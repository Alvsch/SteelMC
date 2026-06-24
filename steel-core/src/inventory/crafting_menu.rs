//! The crafting table menu (3x3 crafting grid).
//!
//! Slot layout (46 total):
//! - Slot 0: Crafting result
//! - Slots 1-9: 3x3 crafting grid
//! - Slots 10-36: Main inventory (27 slots)
//! - Slots 37-45: Hotbar (9 slots)

use std::any::Any;
use std::{mem, sync::Arc};

use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::item_stack::ItemStack;
use steel_registry::menu_type::MenuTypeRef;
use steel_registry::vanilla_blocks;
use steel_registry::vanilla_menu_types;
use steel_utils::BlockPos;
use steel_utils::locks::SyncMutex;

use crate::inventory::slots::slot::{SyncCraftingContainer, SyncResultContainer};
use crate::inventory::slots::{CraftingHandler, ResultHandler};
use crate::inventory::{
    BuiltMenu, MenuBuilder, MenuLayout, SyncPlayerInv,
    container::Container,
    crafting::{CraftingContainer, ResultContainer},
    lock::{ContainerLockGuard, ContainerRef},
    menu::{Menu, MenuBehavior},
    menu_provider::MenuInstance,
};
use crate::player::Player;

/// The crafting table menu with a 3x3 crafting grid.
///
/// Based on Java's `CraftingMenu`.
pub struct CraftingMenu {
    behavior: MenuBehavior,
    /// The 3x3 crafting grid container.
    crafting_container: SyncCraftingContainer,
    /// The crafting result container.
    result_container: SyncResultContainer,
    /// The position of the crafting table block.
    block_pos: BlockPos,
    handler: CraftingHandler,
    /// Section ranges and shift-click routes.
    layout: MenuLayout,
}

impl CraftingMenu {
    /// Creates a new crafting menu for a player.
    ///
    /// # Arguments
    /// * `inventory` - The player's inventory
    /// * `container_id` - The container ID for this menu (1-100)
    /// * `block_pos` - The position of the crafting table block
    #[must_use]
    pub fn new(inventory: SyncPlayerInv, container_id: u8, block_pos: BlockPos) -> Self {
        // Create the crafting containers
        let crafting_container: SyncCraftingContainer =
            Arc::new(SyncMutex::new(CraftingContainer::new(3, 3)));
        let result_container: SyncResultContainer =
            Arc::new(SyncMutex::new(ResultContainer::new()));

        let handler = CraftingHandler::new(crafting_container.clone(), result_container.clone(), 3);

        let mut builder = MenuBuilder::new(&vanilla_menu_types::CRAFTING, container_id);
        let result = builder.result_slot(
            Arc::new(handler),
            ContainerRef::ResultContainer(result_container.clone()),
        );
        let grid = builder.section(
            ContainerRef::CraftingContainer(crafting_container.clone()),
            9,
        );
        let player = builder.player_inventory(&inventory);

        // Vanilla CraftingMenu::quickMoveStack routing.
        builder.route(result, [player.all], true);
        builder.route(grid, [player.all], false);
        builder.route(player.main, [grid, player.hotbar], false);
        builder.route(player.hotbar, [grid, player.main], false);
        builder.drain([grid]);

        let BuiltMenu { behavior, layout } = builder.build();

        Self {
            behavior,
            crafting_container: crafting_container.clone(),
            result_container: result_container.clone(),
            block_pos,
            handler: CraftingHandler::new(crafting_container, result_container, 3),
            layout,
        }
    }

    /// Returns the menu type for the crafting table.
    #[must_use]
    pub fn menu_type() -> MenuTypeRef {
        &vanilla_menu_types::CRAFTING
    }

    /// Returns a reference to the crafting container.
    #[must_use]
    pub const fn crafting_container(&self) -> &SyncCraftingContainer {
        &self.crafting_container
    }

    /// Returns a reference to the result container.
    #[must_use]
    pub const fn result_container(&self) -> &SyncResultContainer {
        &self.result_container
    }

    /// Returns the position of the crafting table block.
    #[must_use]
    pub const fn block_pos(&self) -> BlockPos {
        self.block_pos
    }

    /// Returns a `ContainerRef` for the crafting container.
    #[must_use]
    pub fn crafting_container_ref(&self) -> ContainerRef {
        ContainerRef::CraftingContainer(Arc::clone(&self.crafting_container))
    }

    /// Returns a `ContainerRef` for the result container.
    #[must_use]
    pub fn result_container_ref(&self) -> ContainerRef {
        ContainerRef::ResultContainer(Arc::clone(&self.result_container))
    }
}

impl Menu for CraftingMenu {
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

    /// Returns true if the item can be taken from the slot during pickup all.
    /// Prevents taking from the crafting result slot (index 0).
    fn can_take_item_for_pick_all(&self, _carried: &ItemStack, slot_index: usize) -> bool {
        slot_index != 0
    }

    /// Returns true if the player is still within range of the crafting table.
    ///
    /// Based on Java's `CraftingMenu::stillValid` which checks:
    /// 1. The block at the position is still a crafting table
    /// 2. The player is within block interaction range plus vanilla's 4.0 buffer
    fn still_valid(&self, player: &Player) -> bool {
        let world = player.get_world();
        world.get_block_state(self.block_pos).get_block() == &vanilla_blocks::CRAFTING_TABLE
            && player.is_within_block_interaction_range_with_buffer(self.block_pos, 4.0)
    }

    /// Called when the crafting menu is closed.
    /// Returns crafting grid items to the player's inventory.
    ///
    /// Based on Java's `CraftingMenu::removed` which calls `clearContainer`.
    fn removed(&mut self, player: &Player) {
        let carried = mem::take(&mut self.behavior.carried);
        if !carried.is_empty() {
            player.add_item_or_drop(carried);
        }

        // Return the crafting grid to the player; the result is virtual.
        self.layout.return_drained_items(&self.behavior, player);
        self.result_container.lock().set_item(0, ItemStack::empty());
    }

    fn slots_changed(
        &mut self,
        guard: &mut ContainerLockGuard,
        _slot_index: usize,
        _player: &Player,
    ) {
        self.handler.update_result(guard);
    }
}

impl MenuInstance for CraftingMenu {
    fn menu_type(&self) -> MenuTypeRef {
        &vanilla_menu_types::CRAFTING
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
