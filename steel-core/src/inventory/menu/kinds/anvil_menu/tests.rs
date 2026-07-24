use std::sync::{Arc, Weak};

use glam::DVec3;
use steel_protocol::packet_traits::{CompressionInfo, EncodedPacket};
use steel_registry::{
    item_stack::ItemStack, test_support::init_test_registry, vanilla_blocks, vanilla_enchantments,
    vanilla_items,
};
use steel_utils::{
    BlockPos, ChunkPos,
    types::{GameType, UpdateFlags},
};
use text_components::TextComponent;
use uuid::Uuid;

use super::anvil;
use crate::{
    behavior::init_behaviors,
    config::RuntimeConfig,
    entity::Entity as _,
    inventory::{
        click::{Click, MouseButton},
        container::Container as _,
        menu::{Menu, MenuKindType},
    },
    player::{
        ClientInformation, GameProfile, Player, PlayerConnection, connection::NetworkConnection,
    },
    server::Server,
    test_support::{fresh_test_world, insert_ready_full_chunk},
    world::World,
};

struct TestConnection;

impl NetworkConnection for TestConnection {
    fn compression(&self) -> Option<CompressionInfo> {
        None
    }

    fn send_encoded(&self, _packet: EncodedPacket) {}

    fn send_encoded_bundle(&self, _packets: Vec<EncodedPacket>) {}

    fn disconnect_with_reason(&self, _reason: TextComponent) {}

    fn tick(&self) {}

    fn latency(&self) -> i32 {
        0
    }

    fn close(&self) {}

    fn closed(&self) -> bool {
        false
    }
}

fn test_config() -> Arc<RuntimeConfig> {
    Arc::new(RuntimeConfig {
        max_players: 1,
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

fn test_player(world: Arc<World>) -> Arc<Player> {
    let connection = Arc::new(PlayerConnection::Other(Box::new(TestConnection)));
    Arc::new_cyclic(|weak_player| {
        Player::new(
            GameProfile {
                id: Uuid::from_u128(1),
                name: "AnvilTester".to_owned(),
                properties: Vec::new(),
                profile_actions: None,
            },
            connection,
            world,
            Weak::<Server>::new(),
            test_config(),
            1,
            weak_player,
            ClientInformation::default(),
        )
    })
}

fn test_anvil(key: &'static str) -> (Arc<World>, Arc<Player>, BlockPos, Menu) {
    init_test_registry();
    init_behaviors();
    let world = fresh_test_world(key);
    let pos = BlockPos::new(0, 64, 0);
    insert_ready_full_chunk(&world, ChunkPos::from_block_pos(pos));
    assert!(world.set_block(
        pos,
        vanilla_blocks::ANVIL.default_state(),
        UpdateFlags::UPDATE_ALL,
    ));
    let player = test_player(Arc::clone(&world));
    player.base().set_position_local(DVec3::new(0.5, 64.0, 0.5));
    let menu = anvil(Arc::clone(&player.inventory), 1, pos, &world);
    (world, player, pos, menu)
}

#[test]
fn validity_requires_anvil_tag_and_interaction_range() {
    let (world, player, pos, menu) = test_anvil("anvil_menu_validity");
    assert!(menu.still_valid(&player));

    assert!(world.set_block(
        pos,
        vanilla_blocks::CHIPPED_ANVIL.default_state(),
        UpdateFlags::UPDATE_ALL,
    ));
    assert!(menu.still_valid(&player));

    let current_world = fresh_test_world("anvil_menu_validity_current_world");
    insert_ready_full_chunk(&current_world, ChunkPos::from_block_pos(pos));
    player.set_world(Arc::clone(&current_world));
    assert!(menu.still_valid(&player));

    assert!(world.set_block(
        pos,
        vanilla_blocks::AIR.default_state(),
        UpdateFlags::UPDATE_ALL,
    ));
    assert!(current_world.set_block(
        pos,
        vanilla_blocks::ANVIL.default_state(),
        UpdateFlags::UPDATE_ALL,
    ));
    assert!(!menu.still_valid(&player));

    assert!(world.set_block(
        pos,
        vanilla_blocks::ANVIL.default_state(),
        UpdateFlags::UPDATE_ALL,
    ));
    player
        .base()
        .set_position_local(DVec3::new(20.0, 64.0, 0.5));
    assert!(!menu.still_valid(&player));
}

#[test]
fn sacrifice_enchantments_conflict_with_earlier_merges() {
    let (_world, player, _pos, mut menu) = test_anvil("anvil_menu_enchantment_conflict");
    let (input_container, result_container) = match menu.kind() {
        MenuKindType::Anvil(kind) => (
            Arc::clone(&kind.input_container),
            Arc::clone(&kind.result_container),
        ),
        _ => panic!("anvil builder should create an anvil menu"),
    };

    let mut book = ItemStack::new(&vanilla_items::ENCHANTED_BOOK);
    book.set_enchantments(
        &[
            (vanilla_enchantments::SHARPNESS.key.clone(), 1),
            (vanilla_enchantments::SMITE.key.clone(), 1),
        ],
        false,
    );
    {
        let mut input = input_container.lock();
        input.set_item(0, ItemStack::new(&vanilla_items::DIAMOND_SWORD));
        input.set_item(1, book);
    }

    menu.set_item_name("", &player);

    let result = result_container.lock().get_item(0).clone();
    let Some(enchantments) = result.get_enchantments_for_crafting() else {
        panic!("anvil result should contain one compatible damage enchantment");
    };
    let damage_enchantment_count = [
        &vanilla_enchantments::SHARPNESS.key,
        &vanilla_enchantments::SMITE.key,
    ]
    .into_iter()
    .filter(|key| enchantments.get_level(key) > 0)
    .count();
    assert_eq!(damage_enchantment_count, 1);
}

#[test]
fn rename_only_result_preserves_unused_second_input() {
    let (_world, player, _pos, mut menu) = test_anvil("anvil_menu_rename_only");
    player.restore_game_modes(GameType::Creative, None);
    let input_container = match menu.kind() {
        MenuKindType::Anvil(kind) => Arc::clone(&kind.input_container),
        _ => panic!("anvil builder should create an anvil menu"),
    };
    {
        let mut input = input_container.lock();
        input.set_item(0, ItemStack::new(&vanilla_items::DIAMOND_SWORD));
        input.set_item(1, ItemStack::new(&vanilla_items::DIAMOND_SWORD));
    }

    menu.set_item_name("Renamed", &player);
    menu.clicked(
        Click::Pickup {
            slot: 2,
            button: MouseButton::Left,
        },
        &player,
    );

    let input = input_container.lock();
    assert!(input.get_item(0).is_empty());
    assert!(input.get_item(1).is(&vanilla_items::DIAMOND_SWORD));
    assert!(menu.behavior().carried().is(&vanilla_items::DIAMOND_SWORD));
}
