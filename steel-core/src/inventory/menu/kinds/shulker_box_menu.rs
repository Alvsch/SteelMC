use steel_registry::vanilla_menu_types;
use steel_utils::locks::Shared;

use crate::{
    behavior::ITEM_BEHAVIORS,
    inventory::{
        lock::ContainerRef,
        menu::{FillDirection, Menu, MenuBehavior, MenuBuilder, MenuKind, SectionKind},
    },
    player::{Player, player_inventory::PlayerInventory},
};

/// Builds a shulker box menu with 3 rows of 9 slots plus the player inventory.
#[must_use]
pub fn shulker_box(
    inventory: Shared<PlayerInventory>,
    container_id: u8,
    container: impl Into<ContainerRef>,
) -> Menu {
    let container = container.into();

    let mut builder = MenuBuilder::new(&vanilla_menu_types::SHULKER_BOX, container_id);
    let shulker_box = builder.section_with(
        &container,
        27,
        SectionKind::restricted(|_slot, stack| {
            ITEM_BEHAVIORS
                .get_behavior(stack.item())
                .can_fit_inside_container_items()
        }),
    );
    let player = builder.player_inventory(&inventory);

    builder.route(shulker_box, player.all(), FillDirection::Backward);
    builder.route(player.all(), shulker_box, FillDirection::Forward);

    builder.build(ShulkerBoxKind { container })
}

/// Per-menu shulker box state: just the backing container for the validity check.
pub struct ShulkerBoxKind {
    /// The backing container.
    container: ContainerRef,
}

// SAFETY: This Steel-owned key uniquely identifies the concrete menu kind
// within the process.
unsafe impl steel_utils::DowncastType for ShulkerBoxKind {
    const TYPE_KEY: steel_utils::DowncastTypeKey =
        steel_utils::DowncastTypeKey::new("steel:menu/shulker_box");
}

impl MenuKind for ShulkerBoxKind {
    /// Returns true if the backing container is still valid for the player.
    fn still_valid(&self, _behavior: &MenuBehavior, player: &Player) -> bool {
        self.container.still_valid(player)
    }
}
