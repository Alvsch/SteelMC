use rustc_hash::FxHashMap;
use steel_utils::Identifier;

use crate::blocks::BlockRef;

/// Represents a block entity type in Minecraft.
/// Block entities are used for blocks that need to store additional data
/// beyond their block state, such as chests, furnaces, signs, etc.
#[derive(Debug)]
pub struct BlockEntityType {
    pub key: Identifier,
    /// Blocks for which vanilla accepts this block entity type.
    pub valid_blocks: &'static [BlockRef],
}

impl BlockEntityType {
    #[must_use]
    pub fn is_valid(&self, block: BlockRef) -> bool {
        self.valid_blocks
            .iter()
            .any(|valid_block| std::ptr::eq(*valid_block, block))
    }
}

pub type BlockEntityTypeRef = &'static BlockEntityType;

pub struct BlockEntityTypeRegistry {
    block_entity_types_by_id: Vec<BlockEntityTypeRef>,
    block_entity_types_by_key: FxHashMap<Identifier, usize>,
    allows_registering: bool,
}

impl BlockEntityTypeRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            block_entity_types_by_id: Vec::new(),
            block_entity_types_by_key: FxHashMap::default(),
            allows_registering: true,
        }
    }
}

crate::impl_standard_methods!(
    BlockEntityTypeRegistry,
    BlockEntityTypeRef,
    block_entity_types_by_id,
    block_entity_types_by_key,
    allows_registering
);

crate::impl_registry!(
    BlockEntityTypeRegistry,
    BlockEntityType,
    block_entity_types_by_id,
    block_entity_types_by_key,
    block_entity_types
);
