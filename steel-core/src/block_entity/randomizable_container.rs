//! Shared storage and persistence for loot-backed block containers.

use simdnbt::borrow::NbtCompound as NbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_registry::item_stack::ItemStack;
use steel_utils::{DowncastType, DowncastTypeKey, Identifier};
use text_components::TextComponent;

use crate::block_entity::base_container::BaseContainer;
use crate::inventory::container::Container;

/// Inventory data shared by Vanilla randomizable block containers.
pub(crate) struct RandomizableContainer {
    base: BaseContainer,
    loot_table: Option<Identifier>,
    loot_table_seed: i64,
}

// SAFETY: This Steel-owned key uniquely identifies the concrete storage shared
// by randomizable block-container implementations.
unsafe impl DowncastType for RandomizableContainer {
    const TYPE_KEY: DowncastTypeKey =
        DowncastTypeKey::new("steel:container/randomizable_block_entity");
}

impl RandomizableContainer {
    #[must_use]
    pub(crate) fn new(size: usize) -> Self {
        Self {
            base: BaseContainer::new(size),
            loot_table: None,
            loot_table_seed: 0,
        }
    }

    pub(crate) fn load(&mut self, nbt: &NbtCompoundView<'_, '_>) {
        self.base.load_metadata(nbt);
        self.loot_table = nbt
            .string("LootTable")
            .and_then(|value| value.to_str().parse().ok());
        self.loot_table_seed = nbt.long("LootTableSeed").unwrap_or(0);

        if self.loot_table.is_some() {
            self.base.clear_items();
        } else {
            self.base.load_items(nbt);
        }
    }

    pub(crate) fn save(&self, nbt: &mut NbtCompound) {
        self.base.save_metadata(nbt);
        if let Some(loot_table) = &self.loot_table {
            nbt.insert("LootTable", loot_table.to_string());
            if self.loot_table_seed != 0 {
                nbt.insert("LootTableSeed", self.loot_table_seed);
            }
            return;
        }
        self.base.save_items(nbt);
    }

    /// Removes every realized item while retaining the fixed slot count.
    pub(crate) fn take_items(&mut self) -> Vec<ItemStack> {
        self.base.take_items()
    }

    #[must_use]
    pub(crate) fn display_name(&self, default: TextComponent) -> TextComponent {
        self.base.display_name(default)
    }

    #[must_use]
    pub(crate) const fn has_custom_name(&self) -> bool {
        self.base.has_custom_name()
    }

    #[must_use]
    pub(crate) fn has_lock(&self) -> bool {
        self.base.has_lock()
    }

    #[must_use]
    pub(crate) const fn has_pending_loot(&self) -> bool {
        self.loot_table.is_some()
    }
}

impl Container for RandomizableContainer {
    // TODO: Vanilla unpacks pending loot before every inventory access. Steel
    // preserves the loot reference and fails closed at current block callers
    // until deterministic `LootTable.fill` is available.
    fn items(&self) -> &[ItemStack] {
        self.base.items()
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        self.base.items_mut()
    }

    fn set_item(&mut self, slot: usize, stack: ItemStack) {
        self.base.set_item(slot, stack);
    }

    fn set_changed(&mut self) {}
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use simdnbt::borrow::read_compound as read_borrowed_compound;
    use steel_registry::{test_support::init_test_registry, vanilla_items};

    use super::*;

    #[test]
    fn realized_items_round_trip_with_vanilla_slot_indices() {
        init_test_registry();
        let mut source = RandomizableContainer::new(27);
        source.set_item(17, ItemStack::with_count(&vanilla_items::STONE, 23));
        let mut saved = NbtCompound::new();
        source.save(&mut saved);

        let mut bytes = Vec::new();
        saved.write(&mut bytes);
        let borrowed = read_borrowed_compound(&mut Cursor::new(bytes.as_slice()))
            .expect("test container NBT should reborrow");
        let view = NbtCompoundView::from(&borrowed);
        let mut loaded = RandomizableContainer::new(27);
        loaded.load(&view);

        assert!(loaded.get_item(17).is(&vanilla_items::STONE));
        assert_eq!(loaded.get_item(17).count(), 23);
        assert!(loaded.get_item(0).is_empty());
    }

    #[test]
    fn pending_loot_round_trip_suppresses_realized_items() {
        init_test_registry();
        let mut loot_nbt = NbtCompound::new();
        loot_nbt.insert("LootTable", "minecraft:chests/simple_dungeon");
        loot_nbt.insert("LootTableSeed", 42_i64);
        let mut bytes = Vec::new();
        loot_nbt.write(&mut bytes);
        let borrowed = read_borrowed_compound(&mut Cursor::new(bytes.as_slice()))
            .expect("test loot NBT should reborrow");
        let view = NbtCompoundView::from(&borrowed);

        let mut container = RandomizableContainer::new(27);
        container.set_item(0, ItemStack::new(&vanilla_items::STONE));
        container.load(&view);
        let mut saved = NbtCompound::new();
        container.save(&mut saved);

        assert!(container.has_pending_loot());
        assert_eq!(
            saved.string("LootTable").map(ToString::to_string),
            Some("minecraft:chests/simple_dungeon".to_owned())
        );
        assert_eq!(saved.long("LootTableSeed"), Some(42));
        assert!(saved.list("Items").is_none());
    }

    #[test]
    fn lock_predicate_round_trips_without_becoming_unlocked() {
        let mut predicate = NbtCompound::new();
        predicate.insert("count", 2_i32);
        let mut source = NbtCompound::new();
        source.insert("lock", predicate);
        let mut bytes = Vec::new();
        source.write(&mut bytes);
        let borrowed = read_borrowed_compound(&mut Cursor::new(bytes.as_slice()))
            .expect("test lock NBT should reborrow");
        let view = NbtCompoundView::from(&borrowed);

        let mut container = RandomizableContainer::new(27);
        container.load(&view);
        let mut saved = NbtCompound::new();
        container.save(&mut saved);

        assert!(container.has_lock());
        assert_eq!(
            saved.compound("lock").and_then(|lock| lock.int("count")),
            Some(2)
        );
    }

    #[test]
    fn explicit_empty_lock_round_trips_but_remains_unlocked() {
        let mut source = NbtCompound::new();
        source.insert("lock", NbtCompound::new());
        let mut bytes = Vec::new();
        source.write(&mut bytes);
        let borrowed = read_borrowed_compound(&mut Cursor::new(bytes.as_slice()))
            .expect("test empty lock NBT should reborrow");
        let view = NbtCompoundView::from(&borrowed);

        let mut container = RandomizableContainer::new(27);
        container.load(&view);
        let mut saved = NbtCompound::new();
        container.save(&mut saved);

        assert!(!container.has_lock());
        assert!(saved.compound("lock").is_some_and(NbtCompound::is_empty));
    }
}
