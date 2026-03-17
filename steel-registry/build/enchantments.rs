use std::fs;

use heck::ToShoutySnakeCase;
use proc_macro2::{Ident, Literal, Span, TokenStream};
use quote::quote;
use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
struct EnchantmentJson {
    id: u32,
    name: String,
    max_level: u32,
    min_cost: CostJson,
    max_cost: CostJson,
    anvil_cost: i32,
    weight: u32,
    slots: Vec<String>,
}

#[derive(Deserialize, Debug, Clone)]
struct CostJson {
    base: i32,
    per_level_above_first: i32,
}

#[derive(Deserialize, Debug)]
struct EnchantmentsFile {
    enchantments: Vec<EnchantmentJson>,
}

fn slot_to_tokens(slot: &str) -> TokenStream {
    match slot {
        "any" => quote! { EquipmentSlotGroup::Any },
        "hand" => quote! { EquipmentSlotGroup::Hand },
        "mainhand" => quote! { EquipmentSlotGroup::Mainhand },
        "offhand" => quote! { EquipmentSlotGroup::Offhand },
        "armor" => quote! { EquipmentSlotGroup::Armor },
        "head" => quote! { EquipmentSlotGroup::Head },
        "chest" => quote! { EquipmentSlotGroup::Chest },
        "legs" => quote! { EquipmentSlotGroup::Legs },
        "feet" => quote! { EquipmentSlotGroup::Feet },
        "body" => quote! { EquipmentSlotGroup::Body },
        other => panic!("Unknown equipment slot group: {other}"),
    }
}

pub(crate) fn build() -> TokenStream {
    println!("cargo:rerun-if-changed=build_assets/enchantments.json");

    let content = fs::read_to_string("build_assets/enchantments.json")
        .expect("Failed to read enchantments.json");
    let file: EnchantmentsFile =
        serde_json::from_str(&content).expect("Failed to parse enchantments.json");

    let mut enchantments = file.enchantments;
    enchantments.sort_by_key(|e| e.id);

    let mut stream = TokenStream::new();

    stream.extend(quote! {
        use crate::enchantment::{
            Enchantment, EnchantmentCost, EnchantmentRegistry, EquipmentSlotGroup,
        };
        use steel_utils::Identifier;
    });

    let mut register_stream = TokenStream::new();

    for ench in &enchantments {
        let const_ident = Ident::new(&ench.name.to_shouty_snake_case(), Span::call_site());
        let name = &ench.name;

        let max_level = Literal::u32_unsuffixed(ench.max_level);
        let min_cost_base = Literal::i32_unsuffixed(ench.min_cost.base);
        let min_cost_per = Literal::i32_unsuffixed(ench.min_cost.per_level_above_first);
        let max_cost_base = Literal::i32_unsuffixed(ench.max_cost.base);
        let max_cost_per = Literal::i32_unsuffixed(ench.max_cost.per_level_above_first);
        let anvil_cost = Literal::i32_unsuffixed(ench.anvil_cost);
        let weight = Literal::u32_unsuffixed(ench.weight);

        let slots: Vec<TokenStream> = ench.slots.iter().map(|s| slot_to_tokens(s)).collect();

        stream.extend(quote! {
            pub static #const_ident: Enchantment = Enchantment {
                key: Identifier::vanilla_static(#name),
                max_level: #max_level,
                min_cost: EnchantmentCost { base: #min_cost_base, per_level_above_first: #min_cost_per },
                max_cost: EnchantmentCost { base: #max_cost_base, per_level_above_first: #max_cost_per },
                anvil_cost: #anvil_cost,
                weight: #weight,
                slots: &[#(#slots),*],
            };
        });

        register_stream.extend(quote! {
            registry.register(&#const_ident);
        });
    }

    stream.extend(quote! {
        pub fn register_enchantments(registry: &mut EnchantmentRegistry) {
            #register_stream
        }
    });

    stream
}
