use simdnbt::{
    FromNbtTag, ToNbtTag,
    borrow::NbtTag as BorrowedNbtTag,
    owned::{NbtCompound, NbtTag as OwnedNbtTag},
};
use steel_utils::{
    Identifier,
    hash::{ComponentHasher, HashComponent},
};

use crate::{
    REGISTRY, RegistryExt, TaggedRegistryExt,
    data_components::{Component, ComponentData},
    items::ItemRef,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Repairable {
    Items { items: Vec<ItemRef> },
    Tag { tag: String },
}

impl Repairable {
    pub fn is_valid_repair_item(&self, item: ItemRef) -> bool {
        match self {
            Repairable::Items { items } => items.contains(&item),
            Repairable::Tag { tag } => REGISTRY
                .items
                .is_in_tag(item, &tag.as_str().parse::<Identifier>().expect("this conversion should work, otherwise an invalid tag was put in and we should panic")),
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
            Repairable::Items { items } => {
                compound.insert(
                    "items",
                    items
                        .iter()
                        .map(|it| it.key.to_string())
                        .collect::<Vec<String>>(),
                );
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
        {
            if let Some(list) = items_tag.list()
                && let Some(strings) = list.strings()
                && !strings.is_empty()
            {
                return Some(Self::Items {
                    items: strings
                        .iter()
                        .filter_map(|key| {
                            REGISTRY.items.by_key(&Identifier::vanilla(key.to_string()))
                        })
                        .collect(),
                });
            } else if let Some(tag) = items_tag.string()
                && !tag.is_empty()
            {
                return Some(Self::Tag {
                    tag: tag.to_string(),
                });
            }
        }

        None
    }
}

impl HashComponent for Repairable {
    fn hash_component(&self, hasher: &mut ComponentHasher) {
        match self {
            Repairable::Items { items } => {
                hasher.start_list();

                // TODO

                hasher.end_list();
            }
            Repairable::Tag { tag } => {
                hasher.put_string(tag);
            }
        }
    }
}
