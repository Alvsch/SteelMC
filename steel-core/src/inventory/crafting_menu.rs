//! The crafting table menu (3x3 crafting grid).
//!
//! Slot layout (46 total):
//! - Slot 0: Crafting result
//! - Slots 1-9: 3x3 crafting grid
//! - Slots 10-36: Main inventory (27 slots)
//! - Slots 37-45: Hotbar (9 slots)

use std::sync::Arc;

use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_blocks;
use steel_registry::vanilla_menu_types;
use steel_utils::BlockPos;
use steel_utils::locks::SyncMutex;

use crate::inventory::slots::slot::SyncResultContainer;
use crate::inventory::slots::{CraftingHandler, ResultHandler};
use crate::inventory::{
    FillDirection, MenuBuilder, SyncPlayerInv,
    container::Container,
    crafting::{CraftingContainer, ResultContainer},
    lock::{ContainerLockGuard, ContainerRef},
    menu::{Menu, MenuBehavior, MenuKind},
};
use crate::player::Player;

/// Builds the crafting table menu with a 3x3 crafting grid.
///
/// Based on Java's `CraftingMenu`.
///
/// # Arguments
/// * `inventory` - The player's inventory
/// * `container_id` - The container ID for this menu (1-100)
/// * `block_pos` - The position of the crafting table block
#[must_use]
pub fn crafting(inventory: SyncPlayerInv, container_id: u8, block_pos: BlockPos) -> Menu {
    // Create the crafting containers
    let crafting_container = Arc::new(SyncMutex::new(CraftingContainer::new(3, 3)));
    let result_container: SyncResultContainer = Arc::new(SyncMutex::new(ResultContainer::new()));

    let handler = CraftingHandler::new(crafting_container.clone(), result_container.clone(), 3);

    let mut builder = MenuBuilder::new(&vanilla_menu_types::CRAFTING, container_id);
    let result = builder.result_slot(
        Arc::new(handler.clone()),
        ContainerRef::ResultContainer(result_container.clone()),
    );
    let grid = builder.section(ContainerRef::CraftingContainer(crafting_container), 9);
    let player = builder.player_inventory(&inventory);

    // Vanilla CraftingMenu::quickMoveStack routing.
    builder.route(result, [player.all], FillDirection::Backward);
    builder.route(grid, [player.all], FillDirection::Forward);
    builder.route(player.main, [grid, player.hotbar], FillDirection::Forward);
    builder.route(player.hotbar, [grid, player.main], FillDirection::Forward);
    builder.drain([grid]);

    builder.build(CraftingKind {
        result_container,
        block_pos,
        handler,
    })
}

/// The per-menu part of a crafting table: the result container (cleared on
/// close), the table position (validity), and the recipe handler.
pub struct CraftingKind {
    /// The crafting result container.
    result_container: SyncResultContainer,
    /// The position of the crafting table block.
    block_pos: BlockPos,
    handler: CraftingHandler,
}

impl MenuKind for CraftingKind {
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
    fn still_valid(&self, _behavior: &MenuBehavior, player: &Player) -> bool {
        let world = player.get_world();
        world.get_block_state(self.block_pos).get_block() == &vanilla_blocks::CRAFTING_TABLE
            && player.is_within_block_interaction_range_with_buffer(self.block_pos, 4.0)
    }

    /// Called when the crafting menu is closed. The grid is drained back to the
    /// player by [`Menu::removed`]; here we just clear the virtual result.
    ///
    /// Based on Java's `CraftingMenu::removed` which calls `clearContainer`.
    fn removed(&mut self, _behavior: &mut MenuBehavior, _player: &Player) {
        self.result_container.lock().set_item(0, ItemStack::empty());
    }

    fn slots_changed(
        &mut self,
        _behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
        self.handler.update_result(guard);
    }
}
