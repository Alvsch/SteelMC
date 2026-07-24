use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};

use steel_protocol::packet_traits::{CompressionInfo, EncodedPacket};
use steel_registry::{item_stack::ItemStack, test_support::init_test_registry, vanilla_items};
use steel_utils::types::GameType;
use text_components::TextComponent;
use uuid::Uuid;

use super::*;
use crate::{
    config::RuntimeConfig,
    inventory::{
        container::Container as _,
        prelude::{Click, MouseButton},
    },
    permission::{PermissionEntry, PermissionMetadataSet, PermissionSet},
    player::{ClientInformation, GameProfile, PlayerConnection, connection::NetworkConnection},
    server::Server,
    test_support::{fresh_test_world_in_domain, test_world},
};

const TARGET_HOTBAR_START: usize = 27;
const TARGET_ARMOR_START: usize = 36;
const TARGET_CRAFTING_START: usize = 41;
const VIEWER_INVENTORY_START: usize = 45;

struct TestConnection {
    closed: AtomicBool,
}

impl NetworkConnection for TestConnection {
    fn compression(&self) -> Option<CompressionInfo> {
        None
    }

    fn send_encoded(&self, _packet: EncodedPacket) {}

    fn send_encoded_bundle(&self, _packets: Vec<EncodedPacket>) {}

    fn disconnect_with_reason(&self, _reason: TextComponent) {
        self.closed.store(true, Ordering::Release);
    }

    fn tick(&self) {}

    fn latency(&self) -> i32 {
        0
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

fn test_config() -> Arc<RuntimeConfig> {
    Arc::new(RuntimeConfig {
        max_players: 2,
        view_distance: 2,
        simulation_distance: 2,
        max_chained_neighbor_updates: 1_000_000,
        online_mode: false,
        auth_server: None,
        profile_server: None,
        encryption: false,
        allow_flight: false,
        motd: String::new(),
        use_favicon: false,
        favicon: String::new(),
        enforce_secure_chat: false,
        chat_spam_threshold_seconds: 10,
        command_spam_threshold_seconds: 10,
        compression: None,
        server_links: None,
        packet_workers: Some(1),
        chunk_generation_threads: Some(1),
        chunk_encoding_threads: Some(1),
    })
}

fn test_player(uuid: u128, name: &str, entity_id: i32) -> Arc<Player> {
    init_test_registry();
    let connection = Arc::new(PlayerConnection::Other(Box::new(TestConnection {
        closed: AtomicBool::new(false),
    })));
    let world = Arc::clone(test_world());
    let config = test_config();
    Arc::new_cyclic(|weak_player| {
        Player::new(
            GameProfile {
                id: Uuid::from_u128(uuid),
                name: name.to_owned(),
                properties: Vec::new(),
                profile_actions: None,
            },
            connection,
            world,
            Weak::<Server>::new(),
            config,
            entity_id,
            weak_player,
            ClientInformation::default(),
        )
    })
}

fn permission_key(value: &str) -> PermissionKey {
    match PermissionKey::parse(value) {
        Ok(key) => key,
        Err(error) => panic!("test permission key should parse: {error}"),
    }
}

fn set_permissions(player: &Player, effective: PermissionSet) {
    player.set_permission_state(
        Vec::new(),
        PermissionSet::new(),
        PermissionMetadataSet::new(),
        effective,
        PermissionMetadataSet::new(),
    );
}

#[test]
fn base_and_modify_permissions_grant_the_expected_access() {
    let Ok((access, modify)) = invsee_permissions() else {
        panic!("built-in invsee permissions should parse");
    };
    let readonly = PermissionSet::from_entries([
        PermissionEntry::allow(permission_key(INVSEE_PERMISSION)),
        PermissionEntry::deny(permission_key(MODIFY_PERMISSION)),
    ]);
    assert!(readonly.allows(&access));
    assert!(!readonly.allows(&modify));

    let modifier = PermissionSet::from_entries([
        PermissionEntry::deny(permission_key(INVSEE_PERMISSION)),
        PermissionEntry::allow(permission_key(MODIFY_PERMISSION)),
    ]);
    assert!(modifier.allows(&access));
    assert!(modifier.allows(&modify));
}

#[test]
fn readonly_target_slots_reject_pickup_and_creative_clone() {
    let source = test_player(1, "Viewer", 1);
    let target = test_player(2, "Target", 2);
    source.restore_game_modes(GameType::Creative, None);
    target
        .inventory
        .lock()
        .set_item(0, ItemStack::with_count(&vanilla_items::STONE, 5));
    let Ok((access, _)) = invsee_permissions() else {
        panic!("built-in invsee permissions should parse");
    };
    let mut menu = invsee(1, &source, &target, false, access);

    menu.clicked(
        Click::Pickup {
            slot: TARGET_HOTBAR_START,
            button: MouseButton::Left,
        },
        &source,
    );
    menu.clicked(
        Click::Clone {
            slot: TARGET_HOTBAR_START,
        },
        &source,
    );

    assert_eq!(target.inventory.lock().get_item(0).count(), 5);
    assert!(menu.behavior().carried().is_empty());
}

#[test]
fn modify_view_can_fully_edit_target_armor_slots() {
    let source = test_player(8, "Viewer", 8);
    let target = test_player(9, "Target", 9);
    let Ok((_, modify)) = invsee_permissions() else {
        panic!("built-in invsee permissions should parse");
    };
    let mut menu = invsee(1, &source, &target, true, modify);
    *menu.behavior_mut().carried_mut() = ItemStack::new(&vanilla_items::STONE);

    menu.clicked(
        Click::Pickup {
            slot: TARGET_ARMOR_START,
            button: MouseButton::Left,
        },
        &source,
    );

    assert!(
        target
            .inventory
            .lock()
            .get_item(39)
            .is(&vanilla_items::STONE)
    );
    assert!(menu.behavior().carried().is_empty());
}

#[test]
fn modify_view_moves_inventory_items_in_both_directions() {
    let source = test_player(8, "Viewer", 8);
    let target = test_player(9, "Target", 9);
    target
        .inventory
        .lock()
        .set_item(0, ItemStack::with_count(&vanilla_items::STONE, 5));
    let Ok((_, modify)) = invsee_permissions() else {
        panic!("built-in invsee permissions should parse");
    };
    let mut menu = invsee(1, &source, &target, true, modify);

    menu.clicked(
        Click::QuickMove {
            slot: TARGET_HOTBAR_START,
        },
        &source,
    );
    assert!(target.inventory.lock().get_item(0).is_empty());
    assert_eq!(source.inventory.lock().get_item(8).count(), 5);

    menu.clicked(
        Click::QuickMove {
            slot: VIEWER_INVENTORY_START + 35,
        },
        &source,
    );
    assert!(source.inventory.lock().get_item(8).is_empty());
    assert_eq!(target.inventory.lock().get_item(9).count(), 5);
}

#[test]
fn modify_view_extracts_but_cannot_insert_crafting_items() {
    let source = test_player(3, "Viewer", 3);
    let target = test_player(4, "Target", 4);
    let handler = target.inventory_crafting_handler();
    handler
        .crafting_container()
        .lock()
        .set_item(0, ItemStack::new(&vanilla_items::OAK_LOG));
    let Ok((_, modify)) = invsee_permissions() else {
        panic!("built-in invsee permissions should parse");
    };
    let mut menu = invsee(1, &source, &target, true, modify);
    menu.on_open(&source);

    {
        let result = handler.result_container();
        let result = result.lock();
        assert!(result.get_item(0).is(&vanilla_items::OAK_PLANKS));
        assert_eq!(result.get_item(0).count(), 4);
    }

    menu.clicked(
        Click::Pickup {
            slot: TARGET_CRAFTING_START,
            button: MouseButton::Left,
        },
        &source,
    );
    assert!(handler.crafting_container().lock().get_item(0).is_empty());
    assert!(handler.result_container().lock().get_item(0).is_empty());
    assert!(menu.behavior().carried().is(&vanilla_items::OAK_LOG));

    menu.clicked(
        Click::Pickup {
            slot: TARGET_CRAFTING_START,
            button: MouseButton::Left,
        },
        &source,
    );
    assert!(handler.crafting_container().lock().get_item(0).is_empty());
    assert!(menu.behavior().carried().is(&vanilla_items::OAK_LOG));
}

#[test]
fn self_invsee_quick_move_does_not_rearrange_the_aliased_inventory() {
    let player = test_player(5, "SelfViewer", 5);
    player
        .inventory
        .lock()
        .set_item(0, ItemStack::with_count(&vanilla_items::STONE, 5));
    let Ok((_, modify)) = invsee_permissions() else {
        panic!("built-in invsee permissions should parse");
    };
    let mut menu = invsee(1, &player, &player, true, modify);

    menu.clicked(
        Click::QuickMove {
            slot: TARGET_HOTBAR_START,
        },
        &player,
    );
    menu.clicked(
        Click::QuickMove {
            slot: VIEWER_INVENTORY_START + TARGET_HOTBAR_START,
        },
        &player,
    );

    let inventory = player.inventory.lock();
    assert_eq!(inventory.get_item(0).count(), 5);
    assert!(
        (1..=40).all(|slot| inventory.get_item(slot).is_empty()),
        "self quick-move must not relocate the source stack"
    );
}

#[test]
fn open_menu_revalidates_permissions_and_target_lifecycle() {
    let source = test_player(6, "Viewer", 6);
    let target = test_player(7, "Target", 7);
    let Ok((access, modify)) = invsee_permissions() else {
        panic!("built-in invsee permissions should parse");
    };

    set_permissions(
        &source,
        PermissionSet::from_entries([PermissionEntry::allow(permission_key(MODIFY_PERMISSION))]),
    );
    let modify_menu = invsee(1, &source, &target, true, modify);
    assert!(modify_menu.still_valid(&source));

    set_permissions(
        &source,
        PermissionSet::from_entries([PermissionEntry::allow(permission_key(INVSEE_PERMISSION))]),
    );
    assert!(!modify_menu.still_valid(&source));

    let readonly_menu = invsee(2, &source, &target, false, access);
    assert!(readonly_menu.still_valid(&source));
    assert!(target.begin_domain_switch());
    assert!(!readonly_menu.still_valid(&source));
    target.finish_domain_switch();
    assert!(readonly_menu.still_valid(&source));

    target.set_world(fresh_test_world_in_domain("other", "invsee_domain"));
    assert!(!readonly_menu.still_valid(&source));
    target.set_world(Arc::clone(test_world()));
    assert!(readonly_menu.still_valid(&source));

    target.close_connection();
    assert!(!readonly_menu.still_valid(&source));
}
