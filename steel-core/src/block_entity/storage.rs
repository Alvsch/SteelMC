//! Block entity storage for chunks.

use std::{fmt, ptr, sync::Arc};

use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use steel_utils::{BlockPos, BlockStateId, locks::SyncRwLock};

use super::{BlockEntity, BlockEntityLifecycleExt as _, SharedBlockEntity};

/// Storage for block entities in a chunk.
///
/// Encapsulates both the main storage map and the ticking list to ensure
/// they stay in sync.
pub(crate) struct BlockEntityStorage {
    /// Related entity, marker, and ticker state shares one lock. This makes replacement,
    /// promotion, removal, and persistence snapshots linearizable.
    entries: SyncRwLock<BlockEntityEntries>,
}

#[derive(Default)]
struct BlockEntityEntries {
    entities: FxHashMap<BlockPos, SharedBlockEntity>,
    pending: FxHashSet<BlockPos>,
    /// Ticking entities, in registration order. Every entry is also present in `entities`.
    tickers: Vec<SharedBlockEntity>,
}

/// Atomic classification of one block-entity storage position.
pub(crate) enum BlockEntityLookup {
    /// A concrete entity currently owns the position.
    Concrete(SharedBlockEntity),
    /// A packed marker awaits lazy promotion.
    Pending,
    /// Neither a concrete entity nor a marker exists.
    Absent,
}

/// Result of conditionally inserting a concrete entity into an empty slot.
pub(crate) enum BlockEntityInsert {
    /// Another concrete entity already owns the position.
    Existing(SharedBlockEntity),
    /// The new entity was inserted and may have staged callbacks.
    Inserted(LifecycleDispatchers),
}

/// Entity ownership detached atomically with a replacing block-state write.
pub(crate) struct DetachedBlockEntity {
    /// The concrete entity that owned the old state, if any.
    pub entity: Option<SharedBlockEntity>,
    /// Whether this detachment owns dispatch of the queued removal callback.
    pub dispatch_removed: bool,
}

pub(crate) type LifecycleDispatchers = SmallVec<[SharedBlockEntity; 2]>;

impl BlockEntityStorage {
    /// Creates a new empty block entity storage.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            entries: SyncRwLock::new(BlockEntityEntries::default()),
        }
    }

    /// Gets a block entity at the given position.
    #[must_use]
    pub(crate) fn get(&self, pos: BlockPos) -> Option<SharedBlockEntity> {
        self.entries.read().entities.get(&pos).cloned()
    }

    /// Classifies a position with one lock acquisition and one entity-map probe.
    #[must_use]
    pub(crate) fn lookup(&self, pos: BlockPos) -> BlockEntityLookup {
        let entries = self.entries.read();
        if let Some(block_entity) = entries.entities.get(&pos) {
            BlockEntityLookup::Concrete(Arc::clone(block_entity))
        } else if entries.pending.contains(&pos) {
            BlockEntityLookup::Pending
        } else {
            BlockEntityLookup::Absent
        }
    }

    /// Returns all block entities in this storage.
    #[must_use]
    pub(crate) fn get_all(&self) -> Vec<SharedBlockEntity> {
        self.entries
            .read()
            .entities
            .values()
            .filter(|entity| !entity.base().is_removed())
            .cloned()
            .collect()
    }

    /// Returns every concrete entry without applying `LevelChunk` lifecycle filtering.
    #[must_use]
    pub(crate) fn get_all_without_lifecycle_filter(&self) -> Vec<SharedBlockEntity> {
        self.entries.read().entities.values().cloned().collect()
    }

    /// Atomically snapshots concrete entities and packed markers for persistence.
    #[must_use]
    pub(crate) fn save_snapshot(&self) -> (Vec<SharedBlockEntity>, Vec<BlockPos>) {
        let entries = self.entries.read();
        (
            entries
                .entities
                .values()
                .filter(|entity| !entity.base().is_removed())
                .cloned()
                .collect(),
            entries.pending.iter().copied().collect(),
        )
    }

    /// Atomically snapshots `ProtoChunk` entries without applying Full lifecycle filtering.
    ///
    /// Vanilla `ProtoChunk` storage is a raw map: removed flags are neither changed nor consulted
    /// until transfer into a `LevelChunk`.
    #[must_use]
    pub(crate) fn save_snapshot_without_lifecycle_filter(
        &self,
    ) -> (Vec<SharedBlockEntity>, Vec<BlockPos>) {
        let entries = self.entries.read();
        (
            entries.entities.values().cloned().collect(),
            entries.pending.iter().copied().collect(),
        )
    }

    /// Consumes this proto storage and transfers its contents without removal transitions.
    ///
    /// Promotion is ownership transfer, not unload. Rejected entities are simply not adopted,
    /// matching Vanilla's Proto-to-LevelChunk transfer.
    #[must_use]
    pub(crate) fn into_transfer_snapshot(self) -> (Vec<SharedBlockEntity>, Vec<BlockPos>) {
        let entries = self.entries.into_inner();
        (
            entries.entities.into_values().collect(),
            entries.pending.into_iter().collect(),
        )
    }

    /// Returns packed block-entity positions without changing them.
    #[must_use]
    pub(crate) fn pending_positions(&self) -> Vec<BlockPos> {
        self.entries.read().pending.iter().copied().collect()
    }

    /// Removes one invalid packed marker without constructing an entity.
    pub(crate) fn remove_pending(&self, pos: BlockPos) {
        self.entries.write().pending.remove(&pos);
    }

    /// Adds a packed marker only when no concrete entity owns the position.
    pub(crate) fn set_pending(&self, pos: BlockPos) -> bool {
        let mut entries = self.entries.write();
        if entries.entities.contains_key(&pos) {
            return false;
        }
        entries.pending.insert(pos)
    }

    /// Returns the number of block entities in this storage.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.entries.read().entities.len()
    }

    /// Sets a `ProtoChunk` block entity without invoking `LevelChunk` lifecycle callbacks.
    ///
    /// Vanilla `ProtoChunk` map replacement neither clears nor sets the removed flag.
    #[must_use]
    pub(crate) fn set_without_lifecycle(&self, block_entity: &SharedBlockEntity) -> bool {
        let pos = block_entity.get_block_pos();
        let mut entries = self.entries.write();
        entries.pending.remove(&pos);
        if entries
            .entities
            .get(&pos)
            .is_some_and(|existing| Arc::ptr_eq(existing, block_entity))
        {
            return false;
        }
        entries.tickers.retain(|entity| entity.base().pos() != pos);
        entries.entities.insert(pos, Arc::clone(block_entity));
        true
    }

    /// Removes a block entity at the given position.
    ///
    /// Marks the entity as removed and removes it from the ticking list.
    pub(crate) fn remove(&self, pos: BlockPos) -> bool {
        let (removed, lifecycle_dispatchers) = self.remove_staged(pos);
        for entity in lifecycle_dispatchers {
            entity.dispatch_lifecycle_events();
        }
        removed
    }

    /// Removes an entity or marker while staging lifecycle callbacks for an outer lock boundary.
    #[must_use]
    pub(crate) fn remove_staged(&self, pos: BlockPos) -> (bool, LifecycleDispatchers) {
        let (removed, removed_pending, dispatch_removed) = {
            let mut entries = self.entries.write();
            let removed = entries.entities.remove(&pos);
            let removed_pending = entries.pending.remove(&pos);
            let dispatch_removed = removed
                .as_ref()
                .is_some_and(|entity| entity.base().queue_set_removed());
            entries.tickers.retain(|entity| entity.base().pos() != pos);
            (removed, removed_pending, dispatch_removed)
        };
        let mut lifecycle_dispatchers = LifecycleDispatchers::new();
        if dispatch_removed && let Some(entity) = &removed {
            lifecycle_dispatchers.push(Arc::clone(entity));
        }
        (removed.is_some() || removed_pending, lifecycle_dispatchers)
    }

    /// Detaches the old owner without invoking callbacks.
    ///
    /// The removal flag/event is queued while storage still owns the entity. This lets a
    /// reentrant same-Arc insertion order its clear transition after removal even though the
    /// caller delays callback dispatch until pre-removal side effects have run.
    #[must_use]
    pub(crate) fn detach_and_queue_removal(&self, pos: BlockPos) -> DetachedBlockEntity {
        let mut entries = self.entries.write();
        let entity = entries.entities.remove(&pos);
        entries.pending.remove(&pos);
        entries.tickers.retain(|ticker| ticker.base().pos() != pos);
        let dispatch_removed = entity
            .as_ref()
            .is_some_and(|entity| entity.base().queue_set_removed());
        DetachedBlockEntity {
            entity,
            dispatch_removed,
        }
    }

    /// Removes `ProtoChunk` entity data without invoking `LevelChunk` lifecycle callbacks.
    pub(crate) fn remove_without_lifecycle(&self, pos: BlockPos) -> bool {
        let mut entries = self.entries.write();
        let removed = entries.entities.remove(&pos).is_some();
        let removed_pending = entries.pending.remove(&pos);
        entries.tickers.retain(|entity| entity.base().pos() != pos);
        removed || removed_pending
    }

    /// Removes the entity only if `expected` still owns `pos`.
    ///
    /// This prevents a stale reader from deleting a concurrent replacement.
    pub(crate) fn remove_if_same_and_removed(
        &self,
        pos: BlockPos,
        expected: &SharedBlockEntity,
    ) -> bool {
        let mut entries = self.entries.write();
        if !entries
            .entities
            .get(&pos)
            .is_some_and(|current| Arc::ptr_eq(current, expected) && current.base().is_removed())
        {
            return false;
        }
        entries.entities.remove(&pos);
        entries.pending.remove(&pos);
        entries.tickers.retain(|entity| entity.base().pos() != pos);
        true
    }

    #[cfg(test)]
    pub(crate) fn add_and_register(&self, block_entity: SharedBlockEntity) {
        let block_state = block_entity.get_block_state();
        let should_tick = block_entity.is_ticking();
        let (_, lifecycle_dispatchers) =
            self.add_and_register_staged(&block_entity, block_state, should_tick);
        for entity in lifecycle_dispatchers {
            entity.dispatch_lifecycle_events();
        }
    }

    /// Adds an entity while staging callbacks for dispatch after outer chunk locks are dropped.
    #[must_use]
    pub(crate) fn add_and_register_staged(
        &self,
        block_entity: &SharedBlockEntity,
        block_state: BlockStateId,
        should_tick: bool,
    ) -> (bool, LifecycleDispatchers) {
        self.set_inner(block_entity, block_state, should_tick)
    }

    fn set_inner(
        &self,
        block_entity: &SharedBlockEntity,
        block_state: BlockStateId,
        should_tick: bool,
    ) -> (bool, LifecycleDispatchers) {
        let pos = block_entity.get_block_pos();
        let (inserted, dispatch_new, removed) = {
            let mut entries = self.entries.write();
            entries.pending.remove(&pos);

            if entries
                .entities
                .get(&pos)
                .is_some_and(|existing| Arc::ptr_eq(existing, block_entity))
            {
                let dispatch_state = block_entity.base().queue_block_state_change(block_state);
                let dispatch_clear = block_entity.base().queue_clear_removed();
                Self::replace_ticker(&mut entries, pos, block_entity, should_tick);
                (false, dispatch_state || dispatch_clear, None)
            } else {
                let dispatch_state = block_entity.base().queue_block_state_change(block_state);
                let dispatch_clear = block_entity.base().queue_clear_removed();
                let removed = entries
                    .entities
                    .insert(pos, Arc::clone(block_entity))
                    .map(|old| {
                        let dispatch = old.base().queue_set_removed();
                        (old, dispatch)
                    });
                entries.tickers.retain(|entity| entity.base().pos() != pos);
                if should_tick {
                    entries.tickers.push(Arc::clone(block_entity));
                }
                (true, dispatch_state || dispatch_clear, removed)
            }
        };
        let mut lifecycle_dispatchers = LifecycleDispatchers::new();
        if dispatch_new {
            lifecycle_dispatchers.push(Arc::clone(block_entity));
        }
        if let Some((old, true)) = removed {
            lifecycle_dispatchers.push(old);
        }
        (inserted, lifecycle_dispatchers)
    }

    /// Inserts without replacing a concurrent concrete owner.
    #[must_use]
    pub(crate) fn insert_if_absent_and_register_staged(
        &self,
        block_entity: &SharedBlockEntity,
        block_state: BlockStateId,
        should_tick: bool,
    ) -> BlockEntityInsert {
        let pos = block_entity.get_block_pos();
        let dispatch_new = {
            let mut entries = self.entries.write();
            if let Some(existing) = entries.entities.get(&pos) {
                return BlockEntityInsert::Existing(Arc::clone(existing));
            }
            entries.pending.remove(&pos);
            let dispatch_state = block_entity.base().queue_block_state_change(block_state);
            let dispatch_clear = block_entity.base().queue_clear_removed();
            entries.entities.insert(pos, Arc::clone(block_entity));
            if should_tick {
                entries.tickers.push(Arc::clone(block_entity));
            }
            dispatch_state || dispatch_clear
        };
        let mut lifecycle_dispatchers = LifecycleDispatchers::new();
        if dispatch_new {
            lifecycle_dispatchers.push(Arc::clone(block_entity));
        }
        BlockEntityInsert::Inserted(lifecycle_dispatchers)
    }

    fn promote_entry(
        &self,
        expected_pos: BlockPos,
        block_entity: SharedBlockEntity,
        block_state: Option<BlockStateId>,
        should_tick: bool,
        update_lifecycle: bool,
    ) -> (Option<SharedBlockEntity>, LifecycleDispatchers) {
        let pos = block_entity.get_block_pos();
        if pos != expected_pos {
            return (None, LifecycleDispatchers::new());
        }
        let dispatch_new = {
            let mut entries = self.entries.write();
            if let Some(existing) = entries.entities.get(&pos) {
                return (Some(Arc::clone(existing)), LifecycleDispatchers::new());
            }
            if !entries.pending.remove(&pos) {
                return (None, LifecycleDispatchers::new());
            }
            let dispatch_state = block_state
                .is_some_and(|state| block_entity.base().queue_block_state_change(state));
            let dispatch_clear = update_lifecycle && block_entity.base().queue_clear_removed();
            entries.entities.insert(pos, Arc::clone(&block_entity));
            if should_tick {
                entries.tickers.push(Arc::clone(&block_entity));
            }
            dispatch_state || dispatch_clear
        };
        let mut lifecycle_dispatchers = LifecycleDispatchers::new();
        if dispatch_new {
            lifecycle_dispatchers.push(Arc::clone(&block_entity));
        }
        (Some(block_entity), lifecycle_dispatchers)
    }

    /// Atomically replaces a packed marker with a concrete proto entity without a ticker.
    pub(crate) fn promote_without_ticker(
        &self,
        expected_pos: BlockPos,
        block_entity: SharedBlockEntity,
    ) -> Option<SharedBlockEntity> {
        self.promote_entry(expected_pos, block_entity, None, false, false)
            .0
    }

    /// Promotes a marker while staging callbacks for dispatch after outer locks are dropped.
    #[must_use]
    pub(crate) fn promote_and_register_staged(
        &self,
        expected_pos: BlockPos,
        block_state: BlockStateId,
        block_entity: SharedBlockEntity,
        should_tick: bool,
    ) -> (Option<SharedBlockEntity>, LifecycleDispatchers) {
        self.promote_entry(
            expected_pos,
            block_entity,
            Some(block_state),
            should_tick,
            true,
        )
    }

    /// Updates the ticking status of a block entity.
    ///
    /// Call this when a block entity's ticking status may have changed.
    pub(crate) fn update_ticker(&self, block_entity: &SharedBlockEntity) {
        let should_tick = block_entity.is_ticking();
        self.update_ticker_with_decision(block_entity, should_tick);
    }

    /// Applies a previously computed ticking decision if `block_entity` still owns its position.
    pub(crate) fn update_ticker_with_decision(
        &self,
        block_entity: &SharedBlockEntity,
        should_tick: bool,
    ) {
        let pos = block_entity.get_block_pos();
        let mut entries = self.entries.write();
        if !entries
            .entities
            .get(&pos)
            .is_some_and(|current| Arc::ptr_eq(current, block_entity))
        {
            return;
        }
        Self::replace_ticker(&mut entries, pos, block_entity, should_tick);
    }

    /// Updates cached state and ticker membership only while `block_entity` still owns `pos`.
    #[must_use]
    pub(crate) fn update_if_same_staged(
        &self,
        pos: BlockPos,
        block_entity: &SharedBlockEntity,
        block_state: BlockStateId,
        should_tick: bool,
    ) -> (bool, LifecycleDispatchers) {
        let dispatch_state = {
            let mut entries = self.entries.write();
            if !entries
                .entities
                .get(&pos)
                .is_some_and(|current| Arc::ptr_eq(current, block_entity))
            {
                return (false, LifecycleDispatchers::new());
            }
            let dispatch_state = block_entity.base().queue_block_state_change(block_state);
            Self::replace_ticker(&mut entries, pos, block_entity, should_tick);
            dispatch_state
        };
        let mut lifecycle_dispatchers = LifecycleDispatchers::new();
        if dispatch_state {
            lifecycle_dispatchers.push(Arc::clone(block_entity));
        }
        (true, lifecycle_dispatchers)
    }

    /// Removes `expected` only while it still owns `pos`, staging its callback.
    #[must_use]
    pub(crate) fn remove_if_same_staged(
        &self,
        pos: BlockPos,
        expected: &dyn BlockEntity,
    ) -> (bool, LifecycleDispatchers) {
        let (removed, dispatch_removed) = {
            let mut entries = self.entries.write();
            if !entries
                .entities
                .get(&pos)
                .is_some_and(|current| ptr::addr_eq(current.as_ref(), expected))
            {
                return (false, LifecycleDispatchers::new());
            }
            let Some(removed) = entries.entities.remove(&pos) else {
                return (false, LifecycleDispatchers::new());
            };
            entries.pending.remove(&pos);
            entries.tickers.retain(|entity| entity.base().pos() != pos);
            let dispatch_removed = removed.base().queue_set_removed();
            (removed, dispatch_removed)
        };
        let mut lifecycle_dispatchers = LifecycleDispatchers::new();
        if dispatch_removed {
            lifecycle_dispatchers.push(removed);
        }
        (true, lifecycle_dispatchers)
    }

    /// Returns a copy of the ticking block entities for iteration.
    ///
    /// Filters out removed entities.
    #[must_use]
    pub(crate) fn get_tickers(&self) -> Vec<SharedBlockEntity> {
        self.entries
            .read()
            .tickers
            .iter()
            .filter(|entity| !entity.base().is_removed())
            .cloned()
            .collect()
    }

    /// Cleans up removed entities from the ticking list.
    pub(crate) fn cleanup_tickers(&self) {
        self.entries
            .write()
            .tickers
            .retain(|entity| !entity.base().is_removed());
    }

    /// Clears storage and returns entities whose lifecycle callback dispatcher this call owns.
    ///
    /// Callers that hold outer chunk-map or holder guards can drop them before dispatching.
    #[must_use]
    pub(crate) fn clear_and_stage_lifecycle_callbacks(&self) -> Vec<SharedBlockEntity> {
        {
            let mut entries = self.entries.write();
            let mut removed = Vec::with_capacity(entries.entities.len());
            for entity in entries.entities.values() {
                if entity.base().queue_set_removed() {
                    removed.push(Arc::clone(entity));
                }
            }
            entries.entities.clear();
            entries.pending.clear();
            entries.tickers.clear();
            removed
        }
    }

    /// Clears `ProtoChunk` entity data without invoking `LevelChunk` lifecycle callbacks.
    pub(crate) fn clear_without_lifecycle(&self) {
        let mut entries = self.entries.write();
        entries.entities.clear();
        entries.pending.clear();
        entries.tickers.clear();
    }

    fn replace_ticker(
        entries: &mut BlockEntityEntries,
        pos: BlockPos,
        block_entity: &SharedBlockEntity,
        should_tick: bool,
    ) {
        let existing_index = entries
            .tickers
            .iter()
            .position(|entity| entity.base().pos() == pos);
        match (existing_index, should_tick) {
            (Some(index), true) => entries.tickers[index] = Arc::clone(block_entity),
            (Some(index), false) => {
                entries.tickers.remove(index);
            }
            (None, true) => entries.tickers.push(Arc::clone(block_entity)),
            (None, false) => {}
        }
    }
}

impl Default for BlockEntityStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for BlockEntityStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlockEntityStorage")
            .field("len", &self.len())
            .field("ticking_len", &self.entries.read().tickers.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    };

    use simdnbt::{borrow::BaseNbtCompound as BorrowedNbtCompound, owned::NbtCompound};
    use steel_registry::{
        test_support::init_test_registry, vanilla_block_entity_types, vanilla_blocks,
    };
    use steel_utils::{DowncastType, DowncastTypeKey, locks::SyncMutex};

    use super::*;
    use crate::block_entity::{BlockEntity, BlockEntityBase, entities::SignBlockEntity};

    struct ReentrantLifecycleBlockEntity {
        base: BlockEntityBase,
        reenter_on_remove: AtomicBool,
        events: SyncMutex<Vec<&'static str>>,
    }

    // SAFETY: This test-only key uniquely identifies this concrete test implementation.
    unsafe impl DowncastType for ReentrantLifecycleBlockEntity {
        const TYPE_KEY: DowncastTypeKey =
            DowncastTypeKey::new("steel:test/block_entity/reentrant_lifecycle");
    }

    impl BlockEntity for ReentrantLifecycleBlockEntity {
        fn base(&self) -> &BlockEntityBase {
            &self.base
        }

        fn on_set_removed(&self) {
            self.events.lock().push("removed");
            if self.reenter_on_remove.swap(false, Ordering::AcqRel) {
                self.clear_removed();
            }
        }

        fn on_clear_removed(&self) {
            self.events.lock().push("cleared");
        }

        fn on_block_state_changed(&self, _state: BlockStateId) {
            self.events.lock().push("state");
        }

        fn load_additional(&self, _nbt: &BorrowedNbtCompound<'_>) {}

        fn save_additional(&self, _nbt: &mut NbtCompound) {}
    }

    #[test]
    fn readding_the_same_entity_does_not_remove_or_duplicate_its_ticker() {
        init_test_registry();
        let storage = BlockEntityStorage::new();
        let entity: SharedBlockEntity = Arc::new(SignBlockEntity::new(
            Weak::new(),
            BlockPos::new(1, 2, 3),
            vanilla_blocks::OAK_SIGN.default_state(),
        ));

        assert!(storage.set_pending(entity.get_block_pos()));
        let (concrete, pending) = storage.save_snapshot();
        assert!(concrete.is_empty());
        assert_eq!(pending, [entity.get_block_pos()]);

        storage.add_and_register(Arc::clone(&entity));
        storage.add_and_register(Arc::clone(&entity));

        assert_eq!(storage.len(), 1);
        assert_eq!(storage.get_tickers().len(), 1);
        assert!(!entity.is_removed());
        let (concrete, pending) = storage.save_snapshot();
        assert_eq!(concrete.len(), 1);
        assert!(pending.is_empty());
    }

    #[test]
    fn stale_removed_cleanup_cannot_delete_a_same_arc_revival() {
        init_test_registry();
        let storage = BlockEntityStorage::new();
        let entity: SharedBlockEntity = Arc::new(SignBlockEntity::new(
            Weak::new(),
            BlockPos::new(1, 2, 3),
            vanilla_blocks::OAK_SIGN.default_state(),
        ));
        storage.add_and_register(Arc::clone(&entity));
        entity.set_removed();
        storage.add_and_register(Arc::clone(&entity));

        assert!(!storage.remove_if_same_and_removed(entity.get_block_pos(), &entity));
        let Some(current) = storage.get(entity.get_block_pos()) else {
            panic!("revived entity should remain stored");
        };
        assert!(Arc::ptr_eq(&entity, &current));
        assert!(!entity.is_removed());
    }

    #[test]
    fn insert_if_absent_preserves_the_concurrent_owner() {
        init_test_registry();
        let storage = BlockEntityStorage::new();
        let pos = BlockPos::new(1, 2, 3);
        let state = vanilla_blocks::OAK_SIGN.default_state();
        let owner: SharedBlockEntity = Arc::new(SignBlockEntity::new(Weak::new(), pos, state));
        let challenger: SharedBlockEntity = Arc::new(SignBlockEntity::new(Weak::new(), pos, state));
        storage.add_and_register(Arc::clone(&owner));

        let result = storage.insert_if_absent_and_register_staged(
            &challenger,
            state,
            challenger.is_ticking(),
        );
        let BlockEntityInsert::Existing(existing) = result else {
            panic!("the existing owner should win insertion");
        };
        assert!(Arc::ptr_eq(&owner, &existing));
        let Some(stored) = storage.get(pos) else {
            panic!("the existing owner should remain stored");
        };
        assert!(Arc::ptr_eq(&owner, &stored));
    }

    #[test]
    fn lifecycle_callbacks_are_reentrant_and_keep_transition_order() {
        init_test_registry();
        let concrete = Arc::new(ReentrantLifecycleBlockEntity {
            base: BlockEntityBase::new(
                &vanilla_block_entity_types::BARREL,
                Weak::new(),
                BlockPos::new(1, 2, 3),
                vanilla_blocks::BARREL.default_state(),
            ),
            reenter_on_remove: AtomicBool::new(true),
            events: SyncMutex::new(Vec::new()),
        });
        let entity: SharedBlockEntity = concrete.clone();
        let storage = BlockEntityStorage::new();
        storage.add_and_register(entity);

        assert!(storage.remove(concrete.get_block_pos()));
        assert_eq!(*concrete.events.lock(), ["removed", "cleared"]);
        assert!(!concrete.is_removed());
    }

    #[test]
    fn repeated_set_removed_calls_remain_observable() {
        init_test_registry();
        let concrete = Arc::new(ReentrantLifecycleBlockEntity {
            base: BlockEntityBase::new(
                &vanilla_block_entity_types::BARREL,
                Weak::new(),
                BlockPos::new(1, 2, 3),
                vanilla_blocks::BARREL.default_state(),
            ),
            reenter_on_remove: AtomicBool::new(false),
            events: SyncMutex::new(Vec::new()),
        });

        concrete.set_removed();
        concrete.set_removed();

        assert_eq!(*concrete.events.lock(), ["removed", "removed"]);
        assert!(concrete.is_removed());
    }

    #[test]
    fn detached_dispatcher_preserves_same_arc_revival_order() {
        init_test_registry();
        let concrete = Arc::new(ReentrantLifecycleBlockEntity {
            base: BlockEntityBase::new(
                &vanilla_block_entity_types::BARREL,
                Weak::new(),
                BlockPos::new(1, 2, 3),
                vanilla_blocks::BARREL.default_state(),
            ),
            reenter_on_remove: AtomicBool::new(false),
            events: SyncMutex::new(Vec::new()),
        });
        let entity: SharedBlockEntity = concrete.clone();
        let storage = BlockEntityStorage::new();
        storage.add_and_register(Arc::clone(&entity));

        let detached = storage.detach_and_queue_removal(entity.get_block_pos());
        let Some(detached_entity) = detached.entity else {
            panic!("the stored entity should be detached");
        };
        assert!(Arc::ptr_eq(&entity, &detached_entity));
        assert!(entity.is_removed());

        let (_, revival_dispatchers) =
            storage.add_and_register_staged(&entity, vanilla_blocks::BARREL.default_state(), false);
        assert!(revival_dispatchers.is_empty());
        assert!(!entity.is_removed());
        assert!(concrete.events.lock().is_empty());

        if detached.dispatch_removed {
            detached_entity.dispatch_lifecycle_events();
        }
        assert_eq!(*concrete.events.lock(), ["removed", "cleared"]);
        let Some(current) = storage.get(entity.get_block_pos()) else {
            panic!("the revived entity should remain stored");
        };
        assert!(Arc::ptr_eq(&entity, &current));
    }

    #[test]
    fn cached_state_callback_is_staged_after_storage_commit() {
        init_test_registry();
        let copper = vanilla_blocks::COPPER_CHEST.default_state();
        let exposed = vanilla_blocks::EXPOSED_COPPER_CHEST.default_state();
        let concrete = Arc::new(ReentrantLifecycleBlockEntity {
            base: BlockEntityBase::new(
                &vanilla_block_entity_types::CHEST,
                Weak::new(),
                BlockPos::new(1, 2, 3),
                copper,
            ),
            reenter_on_remove: AtomicBool::new(false),
            events: SyncMutex::new(Vec::new()),
        });
        let entity: SharedBlockEntity = concrete.clone();
        let storage = BlockEntityStorage::new();
        storage.add_and_register(Arc::clone(&entity));

        let (_, lifecycle_dispatchers) = storage.add_and_register_staged(&entity, exposed, false);
        assert_eq!(entity.get_block_state(), exposed);
        assert!(concrete.events.lock().is_empty());
        for dispatcher in lifecycle_dispatchers {
            dispatcher.dispatch_lifecycle_events();
        }
        assert_eq!(*concrete.events.lock(), ["state"]);
    }
}
