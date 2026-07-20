//! Block entity system for blocks that need additional data storage.
//!
//! Block entities provide additional data storage and functionality for blocks
//! that need more than what block state properties can offer (e.g., chests,
//! furnaces, signs, etc.).
//!
//! # Architecture
//!
//! Similar to the block/item behavior system, block entities use a registry
//! pattern:
//! - `BlockEntityRegistry` - maps `BlockEntityType` to factory functions
//! - `BlockEntityStorage` - stores block entities in a chunk
//!
//! # Usage
//!
//! ```ignore
//! use steel_core::block_entity::{init_block_entities, BLOCK_ENTITIES};
//!
//! // After registry is frozen, call once at startup:
//! init_block_entities();
//!
//! // Create a block entity:
//! let entity = BLOCK_ENTITIES.create(block_entity_type, pos, state);
//! ```

pub(crate) mod block_state_nbt;
pub mod entities;
mod registry;
mod storage;

use std::{
    ptr,
    sync::{Arc, Weak},
};

use simdnbt::borrow::BaseNbtCompound as BorrowedNbtCompound;
use simdnbt::owned::NbtCompound;
use steel_registry::block_entity_type::BlockEntityTypeRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_utils::{BlockPos, BlockStateId, ErasedType, locks::SyncMutex};

pub use registry::{BLOCK_ENTITIES, BlockEntityFactory, BlockEntityRegistry, init_block_entities};
pub use storage::BlockEntityStorage;

use crate::inventory::lock::ContainerRef;
use crate::player::Player;

use crate::world::World;

struct BlockEntityLifecycle {
    block_state: BlockStateId,
    removed: bool,
}

/// Immutable block-entity identity and its short-lived lifecycle state.
///
/// Concrete block entities keep gameplay data behind their own focused locks.
/// The lifecycle lock is never held while invoking world callbacks.
pub struct BlockEntityBase {
    block_entity_type: BlockEntityTypeRef,
    level: Weak<World>,
    pos: BlockPos,
    lifecycle: SyncMutex<BlockEntityLifecycle>,
}

impl BlockEntityBase {
    /// Creates common metadata for one block entity.
    #[must_use]
    pub const fn new(
        block_entity_type: BlockEntityTypeRef,
        level: Weak<World>,
        pos: BlockPos,
        block_state: BlockStateId,
    ) -> Self {
        Self {
            block_entity_type,
            level,
            pos,
            lifecycle: SyncMutex::new(BlockEntityLifecycle {
                block_state,
                removed: false,
            }),
        }
    }

    #[must_use]
    const fn block_entity_type(&self) -> BlockEntityTypeRef {
        self.block_entity_type
    }

    #[must_use]
    const fn pos(&self) -> BlockPos {
        self.pos
    }

    #[must_use]
    fn block_state(&self) -> BlockStateId {
        self.lifecycle.lock().block_state
    }

    fn set_block_state(&self, state: BlockStateId) {
        self.lifecycle.lock().block_state = state;
    }

    #[must_use]
    fn is_removed(&self) -> bool {
        self.lifecycle.lock().removed
    }

    fn set_removed(&self) {
        self.lifecycle.lock().removed = true;
    }

    fn clear_removed(&self) {
        self.lifecycle.lock().removed = false;
    }

    #[must_use]
    fn level(&self) -> Option<Arc<World>> {
        self.level.upgrade()
    }

    pub(crate) fn set_changed(&self) {
        let Some(world) = self.level() else {
            return;
        };
        let state = self.block_state();
        world.block_entity_changed(self.pos);
        if !state.is_air() {
            world.update_neighbor_for_output_signal(self.pos, state.get_block());
        }
    }

    pub(crate) fn is_valid_container_for(&self, player: &Player) -> bool {
        if self.is_removed() {
            return false;
        }
        let Some(world) = self.level() else {
            return false;
        };
        let Some(current) = world.get_block_entity(self.pos) else {
            return false;
        };
        ptr::eq(current.base(), self)
            && player.is_within_block_interaction_range_with_buffer(self.pos, 4.0)
    }
}

/// Trait for all block entities.
///
/// Block entities are attached to specific blocks in the world and provide
/// additional data storage beyond what block states can hold. Concrete
/// implementations must claim a unique [`steel_utils::DowncastTypeKey`] through
/// [`steel_utils::DowncastType`].
pub trait BlockEntity: ErasedType + Send + Sync {
    /// Returns the common metadata owned by this block entity.
    fn base(&self) -> &BlockEntityBase;

    /// Returns the type of this block entity.
    fn get_type(&self) -> BlockEntityTypeRef {
        self.base().block_entity_type()
    }

    /// Returns the position of this block entity in the world.
    fn get_block_pos(&self) -> BlockPos {
        self.base().pos()
    }

    /// Returns the current block state associated with this entity.
    fn get_block_state(&self) -> BlockStateId {
        self.base().block_state()
    }

    /// Updates the cached block state.
    ///
    /// Called when the block state changes but the block entity is kept.
    fn set_block_state(&self, state: BlockStateId) {
        self.base().set_block_state(state);
    }

    /// Returns whether this block entity has been marked for removal.
    fn is_removed(&self) -> bool {
        self.base().is_removed()
    }

    /// Marks this block entity as removed.
    ///
    /// Removed block entities will be cleaned up and should not be ticked.
    fn set_removed(&self) {
        self.base().set_removed();
    }

    /// Clears the removed flag.
    ///
    /// Used when re-adding a block entity that was previously removed.
    fn clear_removed(&self) {
        self.base().clear_removed();
    }

    /// Called when the block entity's data changes.
    ///
    /// Marks the containing chunk as dirty so changes are persisted to disk.
    fn set_changed(&self) {
        self.base().set_changed();
    }

    /// Gets the world reference if still valid.
    ///
    /// Block entities receive a `Weak<World>` at construction time.
    fn get_level(&self) -> Option<Arc<World>> {
        self.base().level()
    }

    /// Handles a block event delegated by the owning block behavior.
    ///
    /// Mirrors Vanilla `BlockEntity.triggerEvent`.
    fn trigger_event(&self, _param_a: i32, _param_b: i32) -> bool {
        false
    }

    /// Called before the block entity is removed to handle side effects.
    ///
    /// For example, containers should drop their contents here.
    ///
    /// # Arguments
    /// * `pos` - The position of the block entity
    /// * `state` - The block state being removed
    #[expect(
        unused_variables,
        reason = "default trait impl; parameters used by overrides"
    )]
    fn pre_remove_side_effects(&self, pos: BlockPos, state: BlockStateId) {
        // Default: no side effects
    }

    /// Loads additional data from NBT.
    ///
    /// Called when loading the block entity from disk or receiving initial
    /// chunk data from the server.
    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>);

    /// Saves additional data to NBT.
    ///
    /// Called when saving the block entity to disk.
    fn save_additional(&self, nbt: &mut NbtCompound);

    /// Saves only entity-specific data, excluding vanilla type and position metadata.
    fn save_custom_only(&self) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        self.save_additional(&mut nbt);
        for key in ["id", "x", "y", "z"] {
            while nbt.remove(key).is_some() {}
        }
        nbt
    }

    /// Saves command-visible data together with vanilla block-entity metadata.
    fn save_with_full_metadata(&self) -> NbtCompound {
        let mut nbt = self.save_custom_only();
        let pos = self.get_block_pos();
        nbt.insert("id", self.get_type().key.to_string());
        nbt.insert("x", pos.x());
        nbt.insert("y", pos.y());
        nbt.insert("z", pos.z());
        nbt
    }

    /// Returns the NBT data to send to clients for initial sync.
    ///
    /// This is included in the chunk data packet when the chunk is first sent.
    /// Return `None` if no client sync is needed.
    fn get_update_tag(&self) -> Option<NbtCompound> {
        None
    }

    /// Returns whether this block entity should be ticked every game tick.
    ///
    /// Block entities that return `true` will have their `tick()` method called
    /// each game tick.
    fn is_ticking(&self) -> bool {
        false
    }

    /// Called every game tick for ticking block entities.
    ///
    /// Only called if `is_ticking()` returns `true`.
    #[expect(
        unused_variables,
        reason = "default trait impl; parameter used by overrides"
    )]
    fn tick(&self, world: &Arc<World>) {}

    /// Returns the independently lockable container capability owned by this entity.
    fn container_ref(&self) -> Option<ContainerRef> {
        None
    }
}

/// A stable shared block entity without a whole-object mutex.
pub type SharedBlockEntity = Arc<dyn BlockEntity>;
