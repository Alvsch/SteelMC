use crate::data_components::vanilla_components::INSTRUMENT;
use crate::vanilla_instrument_tags::InstrumentTag;
use crate::vanilla_items;
use crate::{test_support::init_test_registry, vanilla_loot_tables};

use super::*;
use steel_utils::random::legacy_random::LegacyRandom;

fn test_rng() -> LegacyRandom {
    LegacyRandom::from_seed(12_345)
}

static FILL_STONE_FUNCTIONS: [ConditionalLootFunction; 1] = [ConditionalLootFunction {
    function: LootFunction::SetCount {
        count: NumberProvider::Constant(70.0),
        add: false,
    },
    conditions: &[],
}];
static FILL_DIRT_FUNCTIONS: [ConditionalLootFunction; 1] = [ConditionalLootFunction {
    function: LootFunction::SetCount {
        count: NumberProvider::Constant(7.0),
        add: false,
    },
    conditions: &[],
}];
static FILL_STONE_ENTRIES: [LootEntry; 1] = [LootEntry::Item {
    name: Identifier::vanilla_static("stone"),
    weight: 1,
    quality: 0,
    conditions: &[],
    functions: &FILL_STONE_FUNCTIONS,
}];
static FILL_DIRT_ENTRIES: [LootEntry; 1] = [LootEntry::Item {
    name: Identifier::vanilla_static("dirt"),
    weight: 1,
    quality: 0,
    conditions: &[],
    functions: &FILL_DIRT_FUNCTIONS,
}];
static FILL_SWORD_ENTRIES: [LootEntry; 1] = [LootEntry::Item {
    name: Identifier::vanilla_static("iron_sword"),
    weight: 1,
    quality: 0,
    conditions: &[],
    functions: &[],
}];
static FILL_POOLS: [LootPool; 3] = [
    LootPool {
        rolls: NumberProvider::Constant(1.0),
        bonus_rolls: 0.0,
        entries: &FILL_STONE_ENTRIES,
        conditions: &[],
        functions: &[],
    },
    LootPool {
        rolls: NumberProvider::Constant(1.0),
        bonus_rolls: 0.0,
        entries: &FILL_DIRT_ENTRIES,
        conditions: &[],
        functions: &[],
    },
    LootPool {
        rolls: NumberProvider::Constant(1.0),
        bonus_rolls: 0.0,
        entries: &FILL_SWORD_ENTRIES,
        conditions: &[],
        functions: &[],
    },
];
static FILL_TABLE: LootTable = LootTable {
    key: Identifier::new_static("steel", "test/deterministic_fill"),
    loot_type: LootType::Chest,
    pools: &FILL_POOLS,
    functions: &[],
    random_sequence: None,
};

fn init_test_registries() {
    init_test_registry();
}

#[test]
fn number_provider_integer_conversion_matches_java_round() {
    let mut random = test_rng();

    assert_eq!(NumberProvider::Constant(0.5).get_int(&mut random), 1);
    assert_eq!(NumberProvider::Constant(-0.5).get_int(&mut random), 0);
    assert_eq!(
        NumberProvider::Uniform { min: 1.5, max: 1.5 }.get_int(&mut random),
        2
    );
}

#[test]
fn fill_matches_vanilla_split_and_shuffle_order() {
    init_test_registries();
    let mut items = vec![ItemStack::empty(); 9];
    items[4] = ItemStack::new(&vanilla_items::BARRIER);
    let mut random = LegacyRandom::from_seed(42);
    let mut context = LootContext::new(&mut random);

    FILL_TABLE.fill(&mut items, &mut context);

    let actual = items
        .iter()
        .map(|stack| (stack.item().key.path.as_ref(), stack.count()))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        [
            ("stone", 35),
            ("stone", 6),
            ("dirt", 5),
            ("dirt", 1),
            ("barrier", 1),
            ("stone", 9),
            ("iron_sword", 1),
            ("stone", 20),
            ("dirt", 1),
        ]
    );
}

#[test]
fn fill_discards_overflow_without_replacing_occupied_slots() {
    init_test_registries();
    let mut items = vec![ItemStack::new(&vanilla_items::BARRIER), ItemStack::empty()];
    let mut random = LegacyRandom::from_seed(42);
    let mut context = LootContext::new(&mut random);

    FILL_TABLE.fill(&mut items, &mut context);

    assert!(items[0].is(&vanilla_items::BARRIER));
    assert!(!items[1].is_empty());
}

#[test]
fn test_oak_log_loot() {
    init_test_registries();
    let mut rng = test_rng();

    let mut ctx = LootContext::new(&mut rng);
    let items = vanilla_loot_tables::BLOCKS_OAK_LOG.get_random_items(&mut ctx);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].count, 1);
    assert_eq!(items[0].item.key, Identifier::vanilla_static("oak_log"));
}

#[test]
fn set_instrument_selects_from_the_configured_holder_set() {
    init_test_registries();
    let mut rng = test_rng();
    let mut ctx = LootContext::new(&mut rng);
    let mut goat_horn = ItemStack::new(&vanilla_items::GOAT_HORN);
    let function = LootFunction::SetInstrument {
        options: InstrumentOptions::Tag(InstrumentTag::REGULAR_GOAT_HORNS),
    };

    function.apply(&mut goat_horn, &mut ctx);

    let selected = goat_horn
        .get(INSTRUMENT)
        .and_then(|component| component.instrument().as_reference())
        .expect("set_instrument should select a registered instrument");
    assert!(
        REGISTRY
            .instruments
            .is_in_tag(selected, &InstrumentTag::REGULAR_GOAT_HORNS)
    );
}

#[test]
fn test_diamond_ore_loot_no_silk_touch() {
    // Without silk touch, diamond ore should drop diamond (not the ore block)
    init_test_registries();
    let mut rng = test_rng();

    let mut ctx = LootContext::new(&mut rng);
    let items = vanilla_loot_tables::BLOCKS_DIAMOND_ORE.get_random_items(&mut ctx);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].count, 1);
    // Without silk touch enchantment, diamond ore drops diamond
    assert_eq!(items[0].item.key, Identifier::vanilla_static("diamond"));
}

#[test]
fn test_grass_block_loot_no_silk_touch() {
    // Without silk touch, grass block should drop dirt
    init_test_registries();
    let mut rng = test_rng();

    let mut ctx = LootContext::new(&mut rng);
    let items = vanilla_loot_tables::BLOCKS_GRASS_BLOCK.get_random_items(&mut ctx);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].count, 1);
    // Without silk touch, grass block drops dirt
    assert_eq!(items[0].item.key, Identifier::vanilla_static("dirt"));
}

#[test]
fn test_stone_loot_no_silk_touch() {
    // Without silk touch, stone should drop cobblestone
    init_test_registries();
    let mut rng = test_rng();

    let mut ctx = LootContext::new(&mut rng);
    let items = vanilla_loot_tables::BLOCKS_STONE.get_random_items(&mut ctx);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].count, 1);
    // Without silk touch, stone drops cobblestone
    assert_eq!(items[0].item.key, Identifier::vanilla_static("cobblestone"));
}

#[test]
fn test_pig_loot_drops_raw_porkchop_when_not_on_fire() {
    init_test_registries();
    let mut rng = test_rng();
    let pig_key = Identifier::vanilla_static("pig");
    let pig = EntityRef {
        entity_type: Some(&pig_key),
        flags: EntityRefFlags::default(),
        equipment: None,
        custom_name: None,
    };

    let mut ctx = LootContext::new(&mut rng).with_this_entity(pig);
    let items = vanilla_loot_tables::ENTITIES_PIG.get_random_items(&mut ctx);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].item.key, Identifier::vanilla_static("porkchop"));
    assert!((1..=3).contains(&items[0].count));
}

#[test]
fn test_pig_loot_smelt_condition_uses_entity_fire_flag() {
    init_test_registries();
    let mut rng = test_rng();
    let pig_key = Identifier::vanilla_static("pig");
    let pig = EntityRef {
        entity_type: Some(&pig_key),
        flags: EntityRefFlags {
            is_on_fire: true,
            ..EntityRefFlags::default()
        },
        equipment: None,
        custom_name: None,
    };

    let mut ctx = LootContext::new(&mut rng).with_this_entity(pig);
    let items = vanilla_loot_tables::ENTITIES_PIG.get_random_items(&mut ctx);

    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].item.key,
        Identifier::vanilla_static("cooked_porkchop")
    );
    assert!((1..=3).contains(&items[0].count));
}

#[test]
fn test_explosion_decay_function() {
    // Test the explosion_decay function directly
    init_test_registries();

    // explosion_decay reduces count based on 1/radius probability per item
    let cond_func = ConditionalLootFunction {
        function: LootFunction::ExplosionDecay,
        conditions: &[],
    };

    let mut total_survived = 0;
    let initial_count = 10;
    let mut rng = test_rng();

    for _ in 0..100 {
        let mut ctx = LootContext::new(&mut rng).with_explosion(4.0);
        let mut item = ItemStack::with_count(&crate::vanilla_items::STONE, initial_count);
        cond_func.function.apply(&mut item, &mut ctx);
        total_survived += item.count;
    }

    // With 10 items each trial, 100 trials = 1000 items total
    // Each has 25% (1/4.0) chance to survive = ~250 expected
    // Allow for variance: 150-350 range
    assert!(
        total_survived > 150 && total_survived < 350,
        "Expected ~250 items with explosion decay (25% of 1000), got {total_survived}"
    );
}

#[test]
fn ominous_bottle_amplifier_function_clamps_to_persistent_range() {
    use crate::data_components::vanilla_components::OMINOUS_BOTTLE_AMPLIFIER;

    init_test_registries();
    for (provided, expected) in [(-3.0, 0), (2.0, 2), (9.0, 4)] {
        let mut rng = test_rng();
        let mut context = LootContext::new(&mut rng);
        let mut item = ItemStack::new(&crate::vanilla_items::OMINOUS_BOTTLE);
        LootFunction::SetOminousBottleAmplifier {
            amplifier: NumberProvider::Constant(provided),
        }
        .apply(&mut item, &mut context);

        assert_eq!(
            item.get(OMINOUS_BOTTLE_AMPLIFIER)
                .map(|amplifier| amplifier.value()),
            Some(expected)
        );
    }
}

#[test]
fn test_survives_explosion_condition() {
    init_test_registries();

    // Test that survives_explosion condition works
    // Gravel has survives_explosion on its alternatives
    let mut survived = 0;
    let mut rng = test_rng();
    for _ in 0..100 {
        let mut ctx = LootContext::new(&mut rng).with_explosion(4.0);
        let items = vanilla_loot_tables::BLOCKS_GRAVEL.get_random_items(&mut ctx);
        if !items.is_empty() {
            survived += 1;
        }
    }

    // With radius 4.0, ~25% should survive
    assert!(
        survived > 10 && survived < 50,
        "Expected ~25% survival rate, got {survived}%"
    );
}
