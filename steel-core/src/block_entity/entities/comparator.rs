//! Comparator block-entity output storage.

use std::sync::{Arc, Weak};

use simdnbt::borrow::{BaseNbtCompound as BorrowedNbtCompound, NbtCompound as NbtCompoundView};
use simdnbt::owned::NbtCompound;
use steel_registry::block_entity_type::BlockEntityTypeRef;
use steel_registry::vanilla_block_entity_types;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

use crate::block_entity::BlockEntity;
use crate::world::World;

/// Vanilla `ComparatorBlockEntity`.
pub struct ComparatorBlockEntity {
    world: Weak<World>,
    pos: BlockPos,
    state: BlockStateId,
    removed: bool,
    output_signal: i32,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ComparatorBlockEntity`.
unsafe impl DowncastType for ComparatorBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/comparator");
}

impl ComparatorBlockEntity {
    /// Creates comparator storage with vanilla's zero output.
    #[must_use]
    pub const fn new(world: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        Self {
            world,
            pos,
            state,
            removed: false,
            output_signal: 0,
        }
    }

    /// Returns the comparator's cached output signal.
    #[must_use]
    pub const fn output_signal(&self) -> i32 {
        self.output_signal
    }

    /// Replaces the comparator's cached output signal.
    pub const fn set_output_signal(&mut self, output_signal: i32) {
        self.output_signal = output_signal;
    }
}

impl BlockEntity for ComparatorBlockEntity {
    fn get_type(&self) -> BlockEntityTypeRef {
        &vanilla_block_entity_types::COMPARATOR
    }

    fn get_block_pos(&self) -> BlockPos {
        self.pos
    }

    fn get_block_state(&self) -> BlockStateId {
        self.state
    }

    fn set_block_state(&mut self, state: BlockStateId) {
        self.state = state;
    }

    fn is_removed(&self) -> bool {
        self.removed
    }

    fn set_removed(&mut self) {
        self.removed = true;
    }

    fn clear_removed(&mut self) {
        self.removed = false;
    }

    fn get_level(&self) -> Option<Arc<World>> {
        self.world.upgrade()
    }

    fn load_additional(&mut self, nbt: &BorrowedNbtCompound<'_>) {
        let nbt: NbtCompoundView<'_, '_> = nbt.into();
        self.output_signal = nbt.int("OutputSignal").unwrap_or(0);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        nbt.insert("OutputSignal", self.output_signal);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use simdnbt::borrow::read_compound as read_borrowed_compound;
    use steel_registry::{test_support::init_test_registry, vanilla_blocks};

    use super::*;

    fn comparator() -> ComparatorBlockEntity {
        init_test_registry();
        ComparatorBlockEntity::new(
            Weak::new(),
            BlockPos::new(4, 65, -9),
            vanilla_blocks::COMPARATOR.default_state(),
        )
    }

    #[test]
    fn output_signal_round_trips_with_vanilla_nbt_key() {
        let mut source = comparator();
        source.set_output_signal(11);
        let mut nbt = NbtCompound::new();
        source.save_additional(&mut nbt);
        assert_eq!(nbt.int("OutputSignal"), Some(11));

        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed = read_borrowed_compound(&mut Cursor::new(bytes.as_slice()))
            .expect("test NBT should reborrow");
        let mut loaded = comparator();
        loaded.load_additional(&borrowed);
        assert_eq!(loaded.output_signal(), 11);
    }

    #[test]
    fn missing_output_signal_loads_vanilla_default() {
        let nbt = NbtCompound::new();
        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed = read_borrowed_compound(&mut Cursor::new(bytes.as_slice()))
            .expect("test NBT should reborrow");
        let mut loaded = comparator();
        loaded.set_output_signal(15);
        loaded.load_additional(&borrowed);
        assert_eq!(loaded.output_signal(), 0);
    }
}
