use simdnbt::{
    FromNbtTag, ToNbtTag,
    borrow::NbtTag as BorrowedNbtTag,
    owned::{NbtCompound, NbtTag as OwnedNbtTag},
};
use steel_utils::{
    Identifier,
    hash::{ComponentHasher, HashComponent, HashEntry},
};

use crate::{
    REGISTRY, RegistryExt, TaggedRegistryExt,
    data_components::{Component, ComponentData},
    items::ItemRef,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Repairable {
    Item { item: ItemRef },
    Tag { tag: Identifier },
}

impl Repairable {
    pub fn is_valid_repair_item(&self, item_ref: ItemRef) -> bool {
        match self {
            Repairable::Item { item } => item == &item_ref,
            Repairable::Tag { tag } => REGISTRY.items.is_in_tag(item_ref, tag),
        }
    }
}

impl Component for Repairable {
    fn into_data(self) -> ComponentData {
        ComponentData::Repairable(self)
    }

    fn from_data(data: ComponentData) -> Option<Self> {
        match data {
            ComponentData::Repairable(v) => Some(v),
            _ => None,
        }
    }

    fn from_data_ref(data: &ComponentData) -> Option<&Self> {
        match data {
            ComponentData::Repairable(v) => Some(v),
            _ => None,
        }
    }
}

impl ToNbtTag for Repairable {
    fn to_nbt_tag(self) -> simdnbt::owned::NbtTag {
        let mut compound = NbtCompound::new();
        match self {
            Repairable::Item { item } => {
                compound.insert("items", item.key.to_string());
            }
            Repairable::Tag { tag } => {
                compound.insert("items", tag);
            }
        }

        OwnedNbtTag::Compound(compound)
    }
}

impl FromNbtTag for Repairable {
    fn from_nbt_tag(tag: BorrowedNbtTag) -> Option<Self> {
        if let Some(compound) = tag.compound()
            && let Some(items_tag) = compound.get("items")
            && let Some(item) = items_tag.string()
            && !item.is_empty()
        {
            let item_str = item.to_str();
            if item_str.starts_with("#") {
                let ident = item_str.split_at(1).1.parse::<Identifier>().ok()?;

                if REGISTRY
                    .items
                    .tag_keys()
                    .find(|tag| *tag == &ident)
                    .is_some()
                {
                    return Some(Self::Tag { tag: ident });
                }
                return None;
            }
            let ident = item_str.parse::<Identifier>().ok()?;
            if let Some(item) = REGISTRY.items.by_key(&ident) {
                return Some(Self::Item { item });
            }
        }

        None
    }
}

impl HashComponent for Repairable {
    fn hash_component(&self, hasher: &mut ComponentHasher) {
        hasher.start_map();

        let mut key_hasher = ComponentHasher::new();
        key_hasher.put_string("items");

        let mut value_hasher = ComponentHasher::new();
        value_hasher.put_string(&match self {
            Repairable::Item { item } => item.key.to_string(),
            Repairable::Tag { tag } => format!("#{tag}"),
        });

        let entry = HashEntry::new(key_hasher, value_hasher);
        hasher.put_raw_bytes(&entry.key_bytes);
        hasher.put_raw_bytes(&entry.value_bytes);

        hasher.end_map();
    }
}

#[cfg(test)]
mod tests {
    use steel_utils::{Identifier, hash::HashComponent};

    use crate::{RegistryExt, data_components::Repairable, items::ItemRegistry, vanilla_items};

    fn create_test_registry() -> ItemRegistry {
        let mut registry = ItemRegistry::new();
        vanilla_items::register_items(&mut registry);
        registry.freeze();
        registry
    }

    #[test]
    fn test_repairable_item() {
        let repairable = Repairable::Item {
            item: create_test_registry()
                .by_key(&Identifier::vanilla_static("phantom_membrane"))
                .unwrap(),
        };
        let hash = repairable.compute_hash();
        assert_eq!(hash, 0x45fbfc46_u32 as i32, "should match vanilla client");
    }

    #[test]
    fn test_repairable_tag() {
        let repairable = Repairable::Tag {
            tag: Identifier::vanilla_static("diamond_tool_materials"),
        };
        let hash = repairable.compute_hash();
        assert_eq!(hash, 1788140035_u32 as i32, "should match vanilla client");
    }
}
