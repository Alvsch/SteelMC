//! Shared storage and persistence for loot-backed block containers.

use simdnbt::borrow::NbtCompound as NbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_registry::{
    REGISTRY, RegistryExt, item_stack::ItemStack, loot_table::LootContext, vanilla_attributes,
};
use steel_utils::{DowncastType, DowncastTypeKey, Identifier};
use text_components::TextComponent;

use crate::block_entity::base_container::BaseContainer;
use crate::entity::{LivingEntity as _, entity_loot_ref};
use crate::inventory::container::{Container, ContainerAccessContext};

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

    /// Realizes the pending loot table with Vanilla's chest loot parameters.
    pub(crate) fn unpack_loot_table(&mut self, context: &ContainerAccessContext<'_>) -> bool {
        let Some(loot_table_key) = self.loot_table.take() else {
            return false;
        };
        let Some(loot_table) = REGISTRY.loot_tables.by_key(&loot_table_key) else {
            // Vanilla resolves a missing table to LootTable.EMPTY while still
            // clearing the deferred table reference.
            return true;
        };

        let luck = context.player.map_or(0.0, |player| {
            player
                .attributes()
                .lock()
                .get_value(vanilla_attributes::LUCK)
                .unwrap_or(0.0) as f32
        });
        // TODO: Trigger `GENERATE_LOOT` once Steel has advancement criteria.
        context.world.with_loot_random(
            self.loot_table_seed,
            loot_table.random_sequence.as_ref(),
            |random| {
                let mut loot_context = LootContext::new(random).with_origin(
                    f64::from(context.pos.x()) + 0.5,
                    f64::from(context.pos.y()) + 0.5,
                    f64::from(context.pos.z()) + 0.5,
                );
                if let Some(player) = context.player {
                    loot_context = loot_context
                        .with_luck(luck)
                        .with_this_entity(entity_loot_ref(player));
                }
                loot_table.fill(self.base.items_mut(), &mut loot_context);
            },
        );
        true
    }
}

impl Container for RandomizableContainer {
    fn prepare_access(&mut self, context: &ContainerAccessContext<'_>) -> bool {
        self.unpack_loot_table(context)
    }

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
    use steel_registry::{
        test_support::init_test_registry, vanilla_blocks, vanilla_entities, vanilla_items,
    };
    use steel_utils::{BlockPos, ChunkPos, Downcast as _, WorldAabb, types::UpdateFlags};

    use crate::{
        behavior::init_behaviors,
        block_entity::init_block_entities,
        entity::entities::ItemEntity,
        inventory::lock::ContainerLockGuard,
        test_support::{fresh_test_world, insert_ready_full_chunk},
    };

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
    fn direct_inventory_access_unpacks_pending_loot() {
        init_test_registry();
        init_behaviors();
        init_block_entities();
        let world = fresh_test_world("pending_loot_inventory_access");
        let pos = BlockPos::new(3, 64, 3);
        insert_ready_full_chunk(&world, ChunkPos::from_block_pos(pos));
        assert!(world.set_block(
            pos,
            vanilla_blocks::CHEST.default_state(),
            UpdateFlags::UPDATE_NONE,
        ));
        let Some(block_entity) = world.get_block_entity(pos) else {
            panic!("chest placement should create its block entity");
        };

        let mut loot_nbt = NbtCompound::new();
        loot_nbt.insert("LootTable", "minecraft:chests/simple_dungeon");
        loot_nbt.insert("LootTableSeed", 42_i64);
        let mut bytes = Vec::new();
        loot_nbt.write(&mut bytes);
        let borrowed = read_borrowed_compound(&mut Cursor::new(bytes.as_slice()))
            .expect("test loot NBT should reborrow");
        block_entity.load_additional(&borrowed);

        let Some(container_ref) = block_entity.container_ref() else {
            panic!("chest should expose its inventory");
        };
        let container_id = container_ref.container_id();
        let guard = ContainerLockGuard::lock_all(&[&container_ref]);
        let Some(container) = guard.get_typed::<RandomizableContainer>(container_id) else {
            panic!("chest should use randomizable container storage");
        };

        assert!(!container.has_pending_loot());
        assert!(container.items().iter().any(|stack| !stack.is_empty()));
    }

    #[test]
    fn destroying_unopened_chest_and_barrel_drops_generated_loot() {
        init_test_registry();
        init_behaviors();
        init_block_entities();
        let world = fresh_test_world("unopened_container_drops");
        insert_ready_full_chunk(&world, ChunkPos::new(0, 0));

        for (pos, block, loot_seed) in [
            (BlockPos::new(3, 64, 3), &vanilla_blocks::CHEST, 42_i64),
            (BlockPos::new(8, 64, 3), &vanilla_blocks::BARREL, 0_i64),
        ] {
            assert!(world.set_block(pos, block.default_state(), UpdateFlags::UPDATE_NONE));
            let Some(block_entity) = world.get_block_entity(pos) else {
                panic!("container placement should create its block entity");
            };

            let mut loot_nbt = NbtCompound::new();
            loot_nbt.insert("LootTable", "minecraft:chests/simple_dungeon");
            loot_nbt.insert("LootTableSeed", loot_seed);
            let mut bytes = Vec::new();
            loot_nbt.write(&mut bytes);
            let borrowed = read_borrowed_compound(&mut Cursor::new(bytes.as_slice()))
                .expect("test loot NBT should reborrow");
            block_entity.load_additional(&borrowed);

            assert!(world.set_block(
                pos,
                vanilla_blocks::AIR.default_state(),
                UpdateFlags::UPDATE_ALL,
            ));
            assert!(world.get_block_entity(pos).is_none());

            let min_x = f64::from(pos.x()) - 2.0;
            let min_y = f64::from(pos.y()) - 2.0;
            let min_z = f64::from(pos.z()) - 2.0;
            let dropped = world.get_entities_in_aabb_matching(
                &WorldAabb::new(min_x, min_y, min_z, min_x + 5.0, min_y + 5.0, min_z + 5.0),
                |entity| entity.entity_type() == &vanilla_entities::ITEM,
            );
            let generated_count = dropped
                .iter()
                .filter_map(|entity| entity.as_ref().downcast_ref::<ItemEntity>())
                .map(|entity| entity.get_item().count())
                .sum::<i32>();
            assert!(generated_count > 0, "{block:?} should drop generated loot");
        }
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
