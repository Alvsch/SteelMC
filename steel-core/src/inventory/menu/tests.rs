use std::sync::Arc;

use steel_registry::{
    item_stack::ItemStack, test_support::init_test_registry, vanilla_items, vanilla_menu_types,
};
use steel_utils::locks::IntoShared as _;
use uuid::Uuid;

use super::{MenuBuilder, kinds::BasicKind};
use crate::{
    inventory::{
        click::{Click, SwapTarget},
        container::{Container as _, SimpleContainer},
    },
    test_support::{TestPlayerBuilder, fresh_test_world},
};

#[test]
fn swap_locks_player_inventory_when_menu_has_no_inventory_slots() {
    init_test_registry();
    let world = fresh_test_world("menu_swap_without_inventory_slots");
    let player =
        TestPlayerBuilder::new(Arc::clone(&world), Uuid::from_u128(1), "SwapTester", 1).build();
    let container = SimpleContainer::new(45).into_shared();
    container
        .lock()
        .set_item(0, ItemStack::new(&vanilla_items::STONE));

    let mut builder = MenuBuilder::new(&vanilla_menu_types::GENERIC_9X1, 1);
    let menu_slots = builder.section(container.clone(), 45);
    let mut menu = builder.build(BasicKind {});

    menu.clicked(
        Click::Swap {
            slot: menu_slots.start(),
            with: SwapTarget::Hotbar(0),
        },
        &player,
    );

    assert!(container.lock().get_item(0).is_empty());
    assert!(
        player
            .inventory
            .lock()
            .get_item(0)
            .is(&vanilla_items::STONE)
    );
}
