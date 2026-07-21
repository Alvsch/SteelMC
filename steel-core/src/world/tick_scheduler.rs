//! Scheduled tick storage and selection for deterministic block and fluid updates.
//!
//! Scheduled ticks are stored in per-chunk priority queues against absolute world
//! game time. Within a chunk, the queue follows vanilla's
//! `ScheduledTick.DRAIN_ORDER`: trigger time, priority, then sub-tick order. Across
//! chunks, only each ready queue head participates in selection, following
//! vanilla's `LevelTicks` container-draining behavior.
//!
//! Saved and proto-chunk ticks remain pending until the chunk first reaches
//! confirmed block-ticking readiness. That transition anchors their saved delays
//! to the current game time, matching `LevelChunkTicks.unpack`. Later readiness
//! demotions do not pause or re-anchor those deadlines.
//!
//! ## Exact cross-chunk ties
//!
//! Each loaded chunk reconstructs saved ticks with its own negative sub-tick
//! order range, so two ready chunk heads can have the same priority and
//! sub-tick order. A `WorldGenRegion` also owns an independent counter, as in
//! Vanilla, and can retain that order when it schedules directly into an
//! already-Full dependency chunk. Vanilla's final order for these exact ties
//! follows iteration of fastutil's `Long2LongOpenHashMap` and then Java's
//! `PriorityQueue` heap behavior. Minecraft supplies no custom hash strategy
//! for that map. As an intentional performance tradeoff, Steel keeps the
//! optimized `scc` chunk traversal as the final tie order instead of reproducing
//! implementation-specific Java collection state. Ordinary live-world ticks
//! still use a world-global sub-tick counter.

use std::{
    cmp::Ordering,
    collections::BinaryHeap,
    ptr,
    sync::atomic::{AtomicI64, Ordering as AtomicOrdering},
};

use rustc_hash::{FxHashMap, FxHashSet};
use steel_registry::blocks::BlockRef;
use steel_registry::fluid::FluidRef;
use steel_utils::{BlockPos, ChunkPos, locks::SyncMutex};

use crate::chunk::level_chunk::LevelChunk;

/// Priority levels for scheduled ticks. Lower discriminant = higher priority.
///
/// Matches vanilla's `TickPriority` enum. `Ord` is derived so that
/// `ExtremelyHigh < Normal < ExtremelyLow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i8)]
pub enum TickPriority {
    /// Highest priority (-3). Fires before all others.
    ExtremelyHigh = -3,
    /// Very high priority (-2).
    VeryHigh = -2,
    /// High priority (-1).
    High = -1,
    /// Default priority (0).
    Normal = 0,
    /// Low priority (1).
    Low = 1,
    /// Very low priority (2).
    VeryLow = 2,
    /// Lowest priority (3). Fires after all others.
    ExtremelyLow = 3,
}

impl TickPriority {
    /// Converts from an `i8` value, returning `None` for out-of-range values.
    #[must_use]
    pub const fn from_i8(value: i8) -> Option<Self> {
        match value {
            -3 => Some(Self::ExtremelyHigh),
            -2 => Some(Self::VeryHigh),
            -1 => Some(Self::High),
            0 => Some(Self::Normal),
            1 => Some(Self::Low),
            2 => Some(Self::VeryLow),
            3 => Some(Self::ExtremelyLow),
            _ => None,
        }
    }
}

/// Trait for types that can be used as the tick target in `ScheduledTick`.
///
/// Provides a `usize` key for deduplication (one tick per `(BlockPos, key)` pair).
pub trait TickKey: Copy {
    /// Returns a key suitable for dedup hashing.
    fn key(self) -> usize;
}

impl TickKey for BlockRef {
    #[inline]
    fn key(self) -> usize {
        ptr::from_ref(self) as usize
    }
}

impl TickKey for FluidRef {
    #[inline]
    fn key(self) -> usize {
        ptr::from_ref(self) as usize
    }
}

/// A single scheduled tick targeting a block or fluid at a specific position.
#[derive(Debug, Clone, Copy)]
pub struct ScheduledTick<T: TickKey> {
    /// The block or fluid type this tick targets.
    pub tick_type: T,
    /// The block position to tick.
    pub pos: BlockPos,
    /// Absolute world game-time deadline.
    pub trigger_tick: i64,
    /// Execution priority (lower = fires first within the same active tick).
    pub priority: TickPriority,
    /// Monotonic counter for stable ordering within the same priority.
    /// Loaded ticks use negative values and therefore precede newly scheduled ticks.
    pub sub_tick_order: i64,
}

/// A scheduled tick in the chunk persistence representation.
///
/// Like vanilla's `SavedTick`, this stores relative delay but not sub-tick order.
/// Loaded ticks receive negative sub-tick orders in their saved list order.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SavedTick<T: TickKey> {
    /// The block or fluid type this tick targets.
    pub(crate) tick_type: T,
    /// The block position to tick.
    pub(crate) pos: BlockPos,
    /// Delay relative to the game time at which the chunk was saved.
    pub(crate) delay: i32,
    /// Execution priority.
    pub(crate) priority: TickPriority,
}

/// A scheduled tick targeting a block.
pub type BlockTick = ScheduledTick<BlockRef>;
/// A scheduled tick targeting a fluid.
pub type FluidTick = ScheduledTick<FluidRef>;
/// Deduplication key used by scheduled tick containers and execution snapshots.
pub type ScheduledTickKey = (BlockPos, usize);
/// Per-chunk storage for scheduled block ticks.
pub type BlockTickList = TickList<BlockRef>;
/// Per-chunk storage for scheduled fluid ticks.
pub type FluidTickList = TickList<FluidRef>;

/// Block and fluid scheduled-tick queues belonging to one Full chunk.
///
/// Full chunks transfer this pair into [`WorldTickScheduler`] before their Full
/// status is published. Keeping both queues under the same world scheduler lock
/// makes the block/fluid phase boundary atomic without locking every chunk.
#[derive(Debug, Default)]
pub(crate) struct ChunkTickLists {
    block: BlockTickList,
    fluid: FluidTickList,
}

impl ChunkTickLists {
    #[must_use]
    pub(crate) const fn new(block: BlockTickList, fluid: FluidTickList) -> Self {
        Self { block, fluid }
    }

    pub(crate) const fn block(&self) -> &BlockTickList {
        &self.block
    }

    pub(crate) const fn block_mut(&mut self) -> &mut BlockTickList {
        &mut self.block
    }

    pub(crate) const fn fluid(&self) -> &FluidTickList {
        &self.fluid
    }

    pub(crate) const fn fluid_mut(&mut self) -> &mut FluidTickList {
        &mut self.fluid
    }

    #[must_use]
    pub(crate) fn snapshot(&self, current_tick: i64) -> ScheduledTickSnapshot {
        ScheduledTickSnapshot {
            block: self.block.pack(current_tick),
            fluid: self.fluid.pack(current_tick),
        }
    }
}

/// Owned persistence snapshot of both scheduled-tick queues for a Full chunk.
pub(crate) struct ScheduledTickSnapshot {
    pub(crate) block: Vec<SavedTick<BlockRef>>,
    pub(crate) fluid: Vec<SavedTick<FluidRef>>,
}

/// A violated Full-chunk scheduled-tick ownership invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TickSchedulerError {
    /// A second Full chunk attempted to register the same position.
    AlreadyRegistered(ChunkPos),
    /// Neither the world scheduler nor the unpublished Full chunk owns queues.
    MissingContainer(ChunkPos),
}

/// Ready ticks and the active containers whose persisted delays changed.
pub(crate) struct ScheduledTickBatch<T: TickKey> {
    pub(crate) ticks: Vec<ScheduledTick<T>>,
    pub(crate) changed_containers: Vec<usize>,
}

/// World owner for all live Full-chunk scheduled block and fluid ticks.
///
/// The mutex is held only while registering, scheduling, querying, snapshotting,
/// or collecting a batch. It is never held while block/fluid callbacks run.
/// A scheduling call linearizes when it acquires this mutex to insert. Reserving
/// a sub-tick order alone does not make the tick visible to a concurrent
/// collection; if collection acquires first, the call can only participate in a
/// later collection.
pub(crate) struct WorldTickScheduler {
    next_sub_tick_order: AtomicI64,
    state: SyncMutex<WorldTickSchedulerState>,
}

#[derive(Debug, Default)]
struct WorldTickSchedulerState {
    chunks: FxHashMap<ChunkPos, ChunkTickLists>,
    block_next_tick: FxHashMap<ChunkPos, i64>,
    fluid_next_tick: FxHashMap<ChunkPos, i64>,
}

impl WorldTickScheduler {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            next_sub_tick_order: AtomicI64::new(0),
            state: SyncMutex::new(WorldTickSchedulerState::default()),
        }
    }

    /// Allocates the world-global order before container lookup or deduplication.
    ///
    /// Vanilla creates the `ScheduledTick` before asking `LevelTicks` to store
    /// it, so failed and duplicate scheduling attempts consume an order too.
    pub(crate) fn next_sub_tick_order(&self) -> i64 {
        self.next_sub_tick_order
            .fetch_add(1, AtomicOrdering::Relaxed)
    }

    /// Transfers an unpublished Full chunk's queues into the world owner.
    pub(crate) fn register_chunk(&self, chunk: &LevelChunk) -> Result<(), TickSchedulerError> {
        let mut state = self.state.lock();
        if state.chunks.contains_key(&chunk.pos) {
            return Err(TickSchedulerError::AlreadyRegistered(chunk.pos));
        }
        let Some(ticks) = chunk.take_unregistered_tick_lists() else {
            return Err(TickSchedulerError::MissingContainer(chunk.pos));
        };
        if let Some(tick) = ticks.block.peek() {
            state.block_next_tick.insert(chunk.pos, tick.trigger_tick);
        }
        if let Some(tick) = ticks.fluid.peek() {
            state.fluid_next_tick.insert(chunk.pos, tick.trigger_tick);
        }
        state.chunks.insert(chunk.pos, ticks);
        Ok(())
    }

    /// Anchors saved/proto delays when a Full chunk first becomes block-ticking.
    ///
    /// `TickList::unpack` is idempotent, so later readiness promotions preserve
    /// the original absolute deadlines.
    pub(crate) fn unpack_chunk(
        &self,
        pos: ChunkPos,
        current_tick: i64,
    ) -> Result<(), TickSchedulerError> {
        let mut state = self.state.lock();
        let (block_head, fluid_head) = {
            let Some(ticks) = state.chunks.get_mut(&pos) else {
                return Err(TickSchedulerError::MissingContainer(pos));
            };
            ticks.block.unpack(current_tick);
            ticks.fluid.unpack(current_tick);
            (
                ticks.block.peek().map(|tick| tick.trigger_tick),
                ticks.fluid.peek().map(|tick| tick.trigger_tick),
            )
        };
        Self::set_next_tick(&mut state.block_next_tick, pos, block_head);
        Self::set_next_tick(&mut state.fluid_next_tick, pos, fluid_head);
        Ok(())
    }

    /// Removes a finally-unloaded Full chunk after its last save completed.
    pub(crate) fn unregister_chunk(&self, pos: ChunkPos) {
        let mut state = self.state.lock();
        state.chunks.remove(&pos);
        state.block_next_tick.remove(&pos);
        state.fluid_next_tick.remove(&pos);
    }

    #[cfg(test)]
    pub(crate) fn has_registered_chunk(&self, pos: ChunkPos) -> bool {
        self.state.lock().chunks.contains_key(&pos)
    }

    #[cfg(test)]
    pub(crate) fn has_indexed_head(&self, pos: ChunkPos) -> bool {
        let state = self.state.lock();
        state.block_next_tick.contains_key(&pos) || state.fluid_next_tick.contains_key(&pos)
    }

    /// Checks the Full-publication invariant at a rare snapshot rebuild rather
    /// than scanning every active chunk during every scheduled-tick phase.
    pub(crate) fn verify_registered_chunks(
        &self,
        active_chunks: &FxHashMap<ChunkPos, usize>,
    ) -> Result<(), TickSchedulerError> {
        let state = self.state.lock();
        for pos in active_chunks.keys() {
            if !state.chunks.contains_key(pos) {
                return Err(TickSchedulerError::MissingContainer(*pos));
            }
        }
        Ok(())
    }

    pub(crate) fn schedule_block(
        &self,
        chunk: &LevelChunk,
        block: BlockRef,
        pos: BlockPos,
        trigger_tick: i64,
        priority: TickPriority,
        sub_tick_order: i64,
    ) -> Result<bool, TickSchedulerError> {
        let mut state = self.state.lock();
        if let Some(ticks) = state.chunks.get_mut(&chunk.pos) {
            let (added, head) = {
                let list = &mut ticks.block;
                let added = list.schedule(block, pos, trigger_tick, priority, sub_tick_order);
                (added, list.peek().map(|tick| tick.trigger_tick))
            };
            if added {
                Self::set_next_tick(&mut state.block_next_tick, chunk.pos, head);
            }
            return Ok(added);
        }
        chunk
            .schedule_unregistered_block_tick(block, pos, trigger_tick, priority, sub_tick_order)
            .ok_or(TickSchedulerError::MissingContainer(chunk.pos))
    }

    pub(crate) fn schedule_fluid(
        &self,
        chunk: &LevelChunk,
        fluid: FluidRef,
        pos: BlockPos,
        trigger_tick: i64,
        priority: TickPriority,
        sub_tick_order: i64,
    ) -> Result<bool, TickSchedulerError> {
        let mut state = self.state.lock();
        if let Some(ticks) = state.chunks.get_mut(&chunk.pos) {
            let (added, head) = {
                let list = &mut ticks.fluid;
                let added = list.schedule(fluid, pos, trigger_tick, priority, sub_tick_order);
                (added, list.peek().map(|tick| tick.trigger_tick))
            };
            if added {
                Self::set_next_tick(&mut state.fluid_next_tick, chunk.pos, head);
            }
            return Ok(added);
        }
        chunk
            .schedule_unregistered_fluid_tick(fluid, pos, trigger_tick, priority, sub_tick_order)
            .ok_or(TickSchedulerError::MissingContainer(chunk.pos))
    }

    pub(crate) fn has_block_tick(
        &self,
        chunk: &LevelChunk,
        pos: BlockPos,
        block: BlockRef,
    ) -> Result<bool, TickSchedulerError> {
        let state = self.state.lock();
        if let Some(ticks) = state.chunks.get(&chunk.pos) {
            return Ok(ticks.block.has_tick(pos, block));
        }
        chunk
            .has_unregistered_block_tick(pos, block)
            .ok_or(TickSchedulerError::MissingContainer(chunk.pos))
    }

    pub(crate) fn has_fluid_tick(
        &self,
        chunk: &LevelChunk,
        pos: BlockPos,
        fluid: FluidRef,
    ) -> Result<bool, TickSchedulerError> {
        let state = self.state.lock();
        if let Some(ticks) = state.chunks.get(&chunk.pos) {
            return Ok(ticks.fluid.has_tick(pos, fluid));
        }
        chunk
            .has_unregistered_fluid_tick(pos, fluid)
            .ok_or(TickSchedulerError::MissingContainer(chunk.pos))
    }

    pub(crate) fn snapshot(
        &self,
        chunk: &LevelChunk,
        current_tick: i64,
    ) -> Result<ScheduledTickSnapshot, TickSchedulerError> {
        let state = self.state.lock();
        if let Some(ticks) = state.chunks.get(&chunk.pos) {
            return Ok(ticks.snapshot(current_tick));
        }
        chunk
            .snapshot_unregistered_tick_lists(current_tick)
            .ok_or(TickSchedulerError::MissingContainer(chunk.pos))
    }

    /// Selects the ready block batch from sparse live-container heads.
    pub(crate) fn begin_tick(
        &self,
        current_tick: i64,
        active_chunks: &FxHashMap<ChunkPos, usize>,
        max_ticks: usize,
    ) -> ScheduledTickBatch<BlockRef> {
        let mut state = self.state.lock();
        let WorldTickSchedulerState {
            chunks,
            block_next_tick,
            ..
        } = &mut *state;
        collect_registered_ticks(
            chunks,
            block_next_tick,
            active_chunks,
            ChunkTickLists::block_mut,
            current_tick,
            max_ticks,
        )
    }

    /// Selects fluids after block callbacks using the same captured game time.
    pub(crate) fn collect_fluid_ticks(
        &self,
        current_tick: i64,
        active_chunks: &FxHashMap<ChunkPos, usize>,
        max_ticks: usize,
    ) -> ScheduledTickBatch<FluidRef> {
        let mut state = self.state.lock();
        let WorldTickSchedulerState {
            chunks,
            fluid_next_tick,
            ..
        } = &mut *state;
        collect_registered_ticks(
            chunks,
            fluid_next_tick,
            active_chunks,
            ChunkTickLists::fluid_mut,
            current_tick,
            max_ticks,
        )
    }

    fn set_next_tick(
        next_ticks: &mut FxHashMap<ChunkPos, i64>,
        pos: ChunkPos,
        trigger_tick: Option<i64>,
    ) {
        if let Some(trigger_tick) = trigger_tick {
            next_ticks.insert(pos, trigger_tick);
        } else {
            next_ticks.remove(&pos);
        }
    }
}

impl Default for WorldTickScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: TickKey> ScheduledTick<T> {
    /// Returns the position/type identity used to deduplicate this tick.
    #[must_use]
    pub fn key(&self) -> ScheduledTickKey {
        (self.pos, self.tick_type.key())
    }

    fn drain_order(&self, other: &Self) -> Ordering {
        self.trigger_tick.cmp(&other.trigger_tick).then_with(|| {
            intra_tick_drain_order(
                self.priority,
                self.sub_tick_order,
                other.priority,
                other.sub_tick_order,
            )
        })
    }
}

fn intra_tick_drain_order(
    left_priority: TickPriority,
    left_sub_tick_order: i64,
    right_priority: TickPriority,
    right_sub_tick_order: i64,
) -> Ordering {
    left_priority
        .cmp(&right_priority)
        .then_with(|| left_sub_tick_order.cmp(&right_sub_tick_order))
}

#[derive(Debug)]
struct QueuedTick<T: TickKey> {
    tick: ScheduledTick<T>,
    insertion_order: u64,
}

impl<T: TickKey> PartialEq for QueuedTick<T> {
    fn eq(&self, other: &Self) -> bool {
        self.tick.drain_order(&other.tick) == Ordering::Equal
            && self.insertion_order == other.insertion_order
    }
}

impl<T: TickKey> Eq for QueuedTick<T> {}

impl<T: TickKey> PartialOrd for QueuedTick<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: TickKey> Ord for QueuedTick<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.tick
            .drain_order(&other.tick)
            .reverse()
            .then_with(|| other.insertion_order.cmp(&self.insertion_order))
    }
}

#[derive(Debug, Clone, Copy)]
struct ReadyContainer {
    pos: ChunkPos,
    rank: usize,
    priority: TickPriority,
    sub_tick_order: i64,
    dirty_reported: bool,
}

impl PartialEq for ReadyContainer {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority
            && self.sub_tick_order == other.sub_tick_order
            && self.rank == other.rank
    }
}

impl Eq for ReadyContainer {}

impl ReadyContainer {
    const fn new<T: TickKey>(
        pos: ChunkPos,
        rank: usize,
        tick: ScheduledTick<T>,
        dirty_reported: bool,
    ) -> Self {
        Self {
            pos,
            rank,
            priority: tick.priority,
            sub_tick_order: tick.sub_tick_order,
            dirty_reported,
        }
    }
}

impl PartialOrd for ReadyContainer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ReadyContainer {
    fn cmp(&self, other: &Self) -> Ordering {
        intra_tick_drain_order(
            self.priority,
            self.sub_tick_order,
            other.priority,
            other.sub_tick_order,
        )
        .reverse()
        .then_with(|| other.rank.cmp(&self.rank))
    }
}

/// Per-chunk storage for scheduled ticks of one type (block or fluid).
///
/// Saved and proto-chunk entries remain in `pending_ticks` until the chunk first
/// reaches block-ticking readiness. Live entries use absolute game-time deadlines.
/// A priority queue keeps live work ordered without scanning every tick.
#[derive(Debug)]
pub struct TickList<T: TickKey> {
    pending_ticks: Option<Vec<SavedTick<T>>>,
    ticks: BinaryHeap<QueuedTick<T>>,
    scheduled: FxHashSet<ScheduledTickKey>,
    next_insertion_order: u64,
}

impl<T: TickKey> TickList<T> {
    /// Creates an empty tick list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending_ticks: None,
            ticks: BinaryHeap::new(),
            scheduled: FxHashSet::default(),
            next_insertion_order: 0,
        }
    }

    /// Creates an empty proto-chunk list whose entries remain relative until
    /// the promoted Full chunk first becomes block-ticking.
    #[must_use]
    pub(crate) fn new_pending() -> Self {
        Self {
            pending_ticks: Some(Vec::new()),
            ticks: BinaryHeap::new(),
            scheduled: FxHashSet::default(),
            next_insertion_order: 0,
        }
    }

    /// Creates a tick list from relative-delay ticks loaded from chunk storage.
    ///
    /// Vanilla assigns loaded entries the range `-len..-1` in saved list order,
    /// ensuring they execute before newly scheduled entries with equal timing
    /// once the list is unpacked.
    #[must_use]
    pub(crate) fn from_saved_ticks(saved_ticks: Vec<SavedTick<T>>) -> Self {
        let mut result = Self::new_pending();
        result.scheduled.reserve(saved_ticks.len());
        for saved_tick in &saved_ticks {
            result
                .scheduled
                .insert((saved_tick.pos, saved_tick.tick_type.key()));
        }
        result.pending_ticks = Some(saved_ticks);
        result
    }

    /// Creates a proto-chunk tick list from relative-delay storage entries.
    ///
    /// `ProtoChunkTicks.load` schedules saved entries individually, so duplicate
    /// `(pos, type)` keys are discarded while preserving the first entry. Full
    /// chunk loading intentionally uses [`Self::from_saved_ticks`] instead because
    /// `LevelChunkTicks` retains its saved list exactly as stored.
    #[must_use]
    pub(crate) fn from_proto_saved_ticks(saved_ticks: Vec<SavedTick<T>>) -> Self {
        let mut result = Self::new_pending();
        result.scheduled.reserve(saved_ticks.len());
        for saved_tick in saved_ticks {
            result.schedule_saved_pending(saved_tick);
        }
        result
    }

    /// Schedules a live tick with an absolute world game-time deadline.
    ///
    /// Returns `true` if the tick was added, or `false` when the same `(pos, type)`
    /// is already scheduled.
    pub(crate) fn schedule(
        &mut self,
        tick_type: T,
        pos: BlockPos,
        trigger_tick: i64,
        priority: TickPriority,
        sub_tick_order: i64,
    ) -> bool {
        let key = (pos, tick_type.key());
        if !self.scheduled.insert(key) {
            return false;
        }

        self.push_unchecked(ScheduledTick {
            tick_type,
            pos,
            trigger_tick,
            priority,
            sub_tick_order,
        });
        true
    }

    /// Stores a proto-chunk tick with Vanilla's fixed zero delay.
    pub(crate) fn schedule_pending(
        &mut self,
        tick_type: T,
        pos: BlockPos,
        priority: TickPriority,
    ) -> bool {
        self.schedule_saved_pending(SavedTick {
            tick_type,
            pos,
            delay: 0,
            priority,
        })
    }

    fn schedule_saved_pending(&mut self, saved_tick: SavedTick<T>) -> bool {
        let key = (saved_tick.pos, saved_tick.tick_type.key());
        if !self.scheduled.insert(key) {
            return false;
        }
        let pending_ticks = self.pending_ticks.get_or_insert_default();
        pending_ticks.push(saved_tick);
        true
    }

    /// Returns `true` if a tick is scheduled for the given `(pos, type)`.
    #[must_use]
    pub(crate) fn has_tick(&self, pos: BlockPos, tick_type: T) -> bool {
        self.scheduled.contains(&(pos, tick_type.key()))
    }

    /// Packs pending entries followed by live entries in Vanilla saved-list order.
    #[must_use]
    pub(crate) fn pack(&self, current_tick: i64) -> Vec<SavedTick<T>> {
        let mut saved = Vec::with_capacity(self.len());
        if let Some(pending_ticks) = &self.pending_ticks {
            saved.extend_from_slice(pending_ticks);
        }

        let mut ticks: Vec<_> = self.ticks.iter().collect();
        ticks.sort_by(|a, b| {
            a.tick
                .sub_tick_order
                .cmp(&b.tick.sub_tick_order)
                .then_with(|| a.insertion_order.cmp(&b.insertion_order))
        });

        saved.extend(ticks.into_iter().map(|queued| SavedTick {
            tick_type: queued.tick.tick_type,
            pos: queued.tick.pos,
            delay: queued.tick.trigger_tick.wrapping_sub(current_tick) as i32,
            priority: queued.tick.priority,
        }));
        saved
    }

    /// Converts pending saved/proto ticks into live absolute-time ordering.
    ///
    /// This mirrors `LevelChunkTicks.unpack`: delays are anchored to `current_tick`
    /// and entries receive negative sub-tick orders in saved-list order. Repeated
    /// calls are no-ops, so later readiness changes cannot re-anchor deadlines.
    pub(crate) fn unpack(&mut self, current_tick: i64) {
        let Some(pending_ticks) = self.pending_ticks.take() else {
            return;
        };
        let tick_count = pending_ticks.len() as i64;
        self.ticks.reserve(pending_ticks.len());
        for (index, saved_tick) in pending_ticks.into_iter().enumerate() {
            self.push_unchecked(ScheduledTick {
                tick_type: saved_tick.tick_type,
                pos: saved_tick.pos,
                trigger_tick: current_tick.wrapping_add(i64::from(saved_tick.delay)),
                priority: saved_tick.priority,
                sub_tick_order: -tick_count + index as i64,
            });
        }
    }

    /// Returns the number of scheduled ticks.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.ticks.len() + self.pending_ticks.as_ref().map_or(0, Vec::len)
    }

    fn push_unchecked(&mut self, tick: ScheduledTick<T>) {
        let insertion_order = self.next_insertion_order;
        self.next_insertion_order = self.next_insertion_order.wrapping_add(1);
        self.ticks.push(QueuedTick {
            tick,
            insertion_order,
        });
    }

    fn peek(&self) -> Option<ScheduledTick<T>> {
        Some(self.ticks.peek()?.tick)
    }

    fn peek_ready(&self, current_tick: i64) -> Option<ScheduledTick<T>> {
        let tick = self.ticks.peek()?.tick;
        (tick.trigger_tick <= current_tick).then_some(tick)
    }

    fn pop_ready(&mut self, current_tick: i64) -> Option<ScheduledTick<T>> {
        self.peek_ready(current_tick)?;
        let tick = self.ticks.pop()?.tick;
        self.scheduled.remove(&tick.key());
        Some(tick)
    }

    #[cfg(test)]
    fn drain_ready(&mut self, current_tick: i64) -> Vec<ScheduledTick<T>> {
        let mut ready = Vec::new();
        while let Some(tick) = self.pop_ready(current_tick) {
            ready.push(tick);
        }
        ready
    }
}

impl<T: TickKey> Default for TickList<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Selects at most `max_ticks` ready entries from sparse live-container heads.
///
/// Only queue heads compete globally. Revealing the next head after each pop is
/// what preserves Vanilla's per-chunk deadline ordering when several ticks are
/// already overdue.
fn collect_registered_ticks<T: TickKey>(
    chunks: &mut FxHashMap<ChunkPos, ChunkTickLists>,
    next_ticks: &mut FxHashMap<ChunkPos, i64>,
    active_chunks: &FxHashMap<ChunkPos, usize>,
    select: fn(&mut ChunkTickLists) -> &mut TickList<T>,
    current_tick: i64,
    max_ticks: usize,
) -> ScheduledTickBatch<T> {
    if max_ticks == 0 || next_ticks.is_empty() || active_chunks.is_empty() {
        return ScheduledTickBatch {
            ticks: Vec::new(),
            changed_containers: Vec::new(),
        };
    }

    let mut ready_containers = BinaryHeap::new();
    next_ticks.retain(|pos, indexed_trigger| {
        if *indexed_trigger > current_tick {
            return true;
        }
        let Some(container) = chunks.get_mut(pos).map(select) else {
            return false;
        };
        let Some(tick) = container.peek() else {
            return false;
        };
        if tick.trigger_tick > current_tick {
            *indexed_trigger = tick.trigger_tick;
            return true;
        }
        let Some(&rank) = active_chunks.get(pos) else {
            return true;
        };
        ready_containers.push(ReadyContainer::new(*pos, rank, tick, false));
        false
    });

    if ready_containers.is_empty() {
        return ScheduledTickBatch {
            ticks: Vec::new(),
            changed_containers: Vec::new(),
        };
    }

    let mut ticks = Vec::with_capacity(max_ticks.min(ready_containers.len()));
    let mut changed_containers = Vec::with_capacity(ready_containers.len());
    while ticks.len() < max_ticks {
        let Some(mut ready_container) = ready_containers.pop() else {
            break;
        };
        let Some(container) = chunks.get_mut(&ready_container.pos).map(select) else {
            continue;
        };
        let Some(tick) = container.pop_ready(current_tick) else {
            if let Some(next_tick) = container.peek() {
                next_ticks.insert(ready_container.pos, next_tick.trigger_tick);
            }
            continue;
        };

        if !ready_container.dirty_reported {
            changed_containers.push(ready_container.rank);
            ready_container.dirty_reported = true;
        }
        ticks.push(tick);

        // Vanilla keeps draining the current container while its next head is
        // no later in intra-tick order than the best competing container. In
        // particular, an exact tie stays with the current container.
        let next_competing_container = ready_containers.peek().copied();
        while ticks.len() < max_ticks {
            let Some(next_tick) = container.peek_ready(current_tick) else {
                break;
            };
            if next_competing_container.is_some_and(|competitor| {
                intra_tick_drain_order(
                    next_tick.priority,
                    next_tick.sub_tick_order,
                    competitor.priority,
                    competitor.sub_tick_order,
                ) == Ordering::Greater
            }) {
                break;
            }
            let Some(next_tick) = container.pop_ready(current_tick) else {
                break;
            };
            ticks.push(next_tick);
        }

        if let Some(next_tick) = container.peek() {
            if ticks.len() < max_ticks && next_tick.trigger_tick <= current_tick {
                ready_containers.push(ReadyContainer::new(
                    ready_container.pos,
                    ready_container.rank,
                    next_tick,
                    ready_container.dirty_reported,
                ));
            } else {
                next_ticks.insert(ready_container.pos, next_tick.trigger_tick);
            }
        }
    }

    for ready_container in ready_containers {
        if let Some(next_tick) = chunks
            .get_mut(&ready_container.pos)
            .map(select)
            .and_then(|container| container.peek())
        {
            next_ticks.insert(ready_container.pos, next_tick.trigger_tick);
        }
    }

    ScheduledTickBatch {
        ticks,
        changed_containers,
    }
}

/// Remaining ticks in the currently collected execution snapshot.
///
/// Vanilla removes a tick from this set immediately before its callback. Earlier
/// callbacks can therefore detect a later tick selected for the same game tick.
#[derive(Debug, Default)]
pub(crate) struct ScheduledTickRunSet {
    remaining: FxHashSet<ScheduledTickKey>,
}

impl ScheduledTickRunSet {
    pub(crate) fn begin<T: TickKey>(&mut self, ticks: &[ScheduledTick<T>]) {
        self.remaining.clear();
        self.remaining.extend(ticks.iter().map(ScheduledTick::key));
    }

    pub(crate) fn start<T: TickKey>(&mut self, tick: &ScheduledTick<T>) {
        self.remaining.remove(&tick.key());
    }

    #[must_use]
    pub(crate) fn contains<T: TickKey>(&self, pos: BlockPos, tick_type: T) -> bool {
        self.remaining.contains(&(pos, tick_type.key()))
    }

    pub(crate) fn clear(&mut self) {
        self.remaining.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steel_registry::blocks::Block;
    use steel_registry::blocks::behavior::BlockConfig;
    use steel_registry::vanilla_fluids;
    use steel_utils::Identifier;

    fn test_block() -> BlockRef {
        static BLOCK: Block = Block::new(
            Identifier::vanilla_static("test_block"),
            BlockConfig::new(),
            &[],
        );
        &BLOCK
    }

    fn test_block_2() -> BlockRef {
        static BLOCK: Block = Block::new(
            Identifier::vanilla_static("test_block_2"),
            BlockConfig::new(),
            &[],
        );
        &BLOCK
    }

    fn schedule(
        list: &mut BlockTickList,
        block: BlockRef,
        pos: BlockPos,
        delay: i32,
        priority: TickPriority,
        sub_tick_order: i64,
    ) -> bool {
        list.schedule(block, pos, i64::from(delay), priority, sub_tick_order)
    }

    fn scheduler_with_block_lists(
        chunks: impl IntoIterator<Item = (ChunkPos, BlockTickList)>,
    ) -> WorldTickScheduler {
        let scheduler = WorldTickScheduler::new();
        {
            let mut state = scheduler.state.lock();
            for (pos, block) in chunks {
                if let Some(tick) = block.peek() {
                    state.block_next_tick.insert(pos, tick.trigger_tick);
                }
                state
                    .chunks
                    .insert(pos, ChunkTickLists::new(block, FluidTickList::new()));
            }
        }
        scheduler
    }

    fn active_ranks(active_chunks: &[ChunkPos]) -> FxHashMap<ChunkPos, usize> {
        active_chunks
            .iter()
            .copied()
            .enumerate()
            .map(|(rank, pos)| (pos, rank))
            .collect()
    }

    fn begin_block_tick_at(
        scheduler: &WorldTickScheduler,
        current_tick: i64,
        active_chunks: &[ChunkPos],
        max_ticks: usize,
    ) -> ScheduledTickBatch<BlockRef> {
        scheduler.begin_tick(current_tick, &active_ranks(active_chunks), max_ticks)
    }

    fn begin_block_tick(
        scheduler: &WorldTickScheduler,
        active_chunks: &[ChunkPos],
        max_ticks: usize,
    ) -> ScheduledTickBatch<BlockRef> {
        begin_block_tick_at(scheduler, 1, active_chunks, max_ticks)
    }

    #[test]
    fn schedule_deduplicates_by_position_and_type() {
        let mut list = BlockTickList::new();
        let block = test_block();
        let pos = BlockPos::new(1, 2, 3);

        assert!(schedule(&mut list, block, pos, 5, TickPriority::Normal, 0));
        assert!(!schedule(&mut list, block, pos, 10, TickPriority::High, 1));
        assert!(schedule(
            &mut list,
            test_block_2(),
            pos,
            5,
            TickPriority::Normal,
            2
        ));
        assert!(schedule(
            &mut list,
            block,
            BlockPos::new(4, 5, 6),
            5,
            TickPriority::Normal,
            3
        ));
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn pending_ticks_are_unindexed_until_idempotent_unpack() {
        let chunk_pos = ChunkPos::new(0, 0);
        let tick_pos = BlockPos::new(1, 2, 3);
        let pending = BlockTickList::from_saved_ticks(vec![SavedTick {
            tick_type: test_block(),
            pos: tick_pos,
            delay: 5,
            priority: TickPriority::Normal,
        }]);
        let scheduler = scheduler_with_block_lists([(chunk_pos, pending)]);

        assert!(
            !scheduler
                .state
                .lock()
                .block_next_tick
                .contains_key(&chunk_pos)
        );
        if let Err(error) = scheduler.unpack_chunk(chunk_pos, 100) {
            panic!("test scheduler invariant failed: {error:?}");
        }
        assert_eq!(
            scheduler.state.lock().block_next_tick.get(&chunk_pos),
            Some(&105)
        );

        // A later readiness promotion cannot re-anchor the existing deadline.
        if let Err(error) = scheduler.unpack_chunk(chunk_pos, 200) {
            panic!("test scheduler invariant failed: {error:?}");
        }
        assert_eq!(
            scheduler.state.lock().block_next_tick.get(&chunk_pos),
            Some(&105)
        );
        assert!(
            begin_block_tick_at(&scheduler, 104, &[chunk_pos], 1)
                .ticks
                .is_empty()
        );
        assert_eq!(
            begin_block_tick_at(&scheduler, 105, &[chunk_pos], 1).ticks[0].pos,
            tick_pos
        );
    }

    #[test]
    fn pending_and_live_ticks_share_dedup_before_unpack() {
        let pending_pos = BlockPos::new(1, 2, 3);
        let live_pos = BlockPos::new(2, 2, 3);
        let mut list = BlockTickList::from_saved_ticks(vec![SavedTick {
            tick_type: test_block(),
            pos: pending_pos,
            delay: 5,
            priority: TickPriority::Normal,
        }]);

        assert!(!list.schedule(test_block(), pending_pos, 101, TickPriority::High, 10));
        assert!(list.schedule(test_block(), live_pos, 101, TickPriority::Normal, 11));
        assert_eq!(list.peek().map(|tick| tick.pos), Some(live_pos));
        list.unpack(100);
        assert_eq!(list.drain_ready(101)[0].pos, live_pos);
        assert_eq!(list.drain_ready(105)[0].pos, pending_pos);
    }

    #[test]
    fn absolute_time_makes_ineligible_deadlines_overdue() {
        let mut list = BlockTickList::new();
        let first_pos = BlockPos::new(0, 0, 0);
        let fourth_pos = BlockPos::new(1, 0, 0);
        assert!(schedule(
            &mut list,
            test_block(),
            first_pos,
            1,
            TickPriority::Normal,
            0
        ));
        assert!(schedule(
            &mut list,
            test_block(),
            fourth_pos,
            4,
            TickPriority::Normal,
            1
        ));

        assert_eq!(list.drain_ready(1)[0].pos, first_pos);
        // No collection occurs while the chunk is ineligible, but world game
        // time continues. The later deadline is overdue upon re-entry.
        assert_eq!(list.drain_ready(100)[0].pos, fourth_pos);
    }

    #[test]
    fn global_cap_retains_ready_overflow() {
        let mut list = BlockTickList::new();
        let chunk_pos = ChunkPos::new(0, 0);
        let high_pos = BlockPos::new(0, 0, 0);
        let normal_pos = BlockPos::new(1, 0, 0);
        let overflow_pos = BlockPos::new(2, 0, 0);
        for (pos, priority, order) in [
            (overflow_pos, TickPriority::Normal, 10),
            (high_pos, TickPriority::High, 20),
            (normal_pos, TickPriority::Normal, 5),
        ] {
            assert!(schedule(&mut list, test_block(), pos, 1, priority, order));
        }

        let scheduler = scheduler_with_block_lists([(chunk_pos, list)]);
        let selected = begin_block_tick(&scheduler, &[chunk_pos], 2);
        assert_eq!(
            selected
                .ticks
                .iter()
                .map(|tick| tick.pos)
                .collect::<Vec<_>>(),
            vec![high_pos, normal_pos]
        );
        assert_eq!(selected.changed_containers, vec![0]);
        {
            let state = scheduler.state.lock();
            let Some(ticks) = state.chunks.get(&chunk_pos) else {
                panic!("test chunk must remain registered");
            };
            assert!(ticks.block.has_tick(overflow_pos, test_block()));
        }

        let selected = begin_block_tick(&scheduler, &[chunk_pos], 2);
        assert_eq!(selected.ticks.len(), 1);
        assert_eq!(selected.ticks[0].pos, overflow_pos);
    }

    #[test]
    fn block_and_fluid_collection_use_the_same_absolute_time() {
        let chunk_pos = ChunkPos::new(0, 0);
        let block_pos = BlockPos::new(0, 0, 0);
        let fluid_pos = BlockPos::new(1, 0, 0);
        let scheduler = scheduler_with_block_lists([(chunk_pos, BlockTickList::new())]);

        {
            let mut state = scheduler.state.lock();
            let (block_head, fluid_head) = {
                let Some(ticks) = state.chunks.get_mut(&chunk_pos) else {
                    panic!("test chunk must remain registered");
                };
                assert!(
                    ticks
                        .block
                        .schedule(test_block(), block_pos, 20, TickPriority::Normal, 0)
                );
                assert!(ticks.fluid.schedule(
                    &vanilla_fluids::WATER,
                    fluid_pos,
                    20,
                    TickPriority::Normal,
                    1
                ));
                (
                    ticks.block.peek().map(|tick| tick.trigger_tick),
                    ticks.fluid.peek().map(|tick| tick.trigger_tick),
                )
            };
            WorldTickScheduler::set_next_tick(&mut state.block_next_tick, chunk_pos, block_head);
            WorldTickScheduler::set_next_tick(&mut state.fluid_next_tick, chunk_pos, fluid_head);
        }

        let active = active_ranks(&[chunk_pos]);
        let blocks = scheduler.begin_tick(20, &active, 2);
        let fluids = scheduler.collect_fluid_ticks(20, &active, 2);
        assert_eq!(
            fluids.ticks.iter().map(|tick| tick.pos).collect::<Vec<_>>(),
            [fluid_pos]
        );
        assert_eq!(
            blocks.ticks.iter().map(|tick| tick.pos).collect::<Vec<_>>(),
            [block_pos]
        );
    }

    #[test]
    fn selection_respects_each_chunks_deadline_head() {
        let mut first_chunk = BlockTickList::new();
        let mut second_chunk = BlockTickList::new();
        let old_low_pos = BlockPos::new(0, 0, 0);
        let later_high_pos = BlockPos::new(1, 0, 0);
        let other_normal_pos = BlockPos::new(16, 0, 0);

        assert!(schedule(
            &mut first_chunk,
            test_block(),
            old_low_pos,
            1,
            TickPriority::Low,
            0
        ));
        assert!(schedule(
            &mut first_chunk,
            test_block(),
            later_high_pos,
            2,
            TickPriority::ExtremelyHigh,
            1
        ));
        assert!(schedule(
            &mut second_chunk,
            test_block(),
            other_normal_pos,
            1,
            TickPriority::Normal,
            2
        ));

        let first_pos = ChunkPos::new(0, 0);
        let second_pos = ChunkPos::new(1, 0);
        let scheduler =
            scheduler_with_block_lists([(first_pos, first_chunk), (second_pos, second_chunk)]);
        // Leave the first due heads queued so that all three are overdue next active tick.
        assert!(
            begin_block_tick(&scheduler, &[first_pos, second_pos], 0)
                .ticks
                .is_empty()
        );

        let selected = begin_block_tick_at(&scheduler, 2, &[first_pos, second_pos], 3);
        assert_eq!(
            selected
                .ticks
                .iter()
                .map(|tick| tick.pos)
                .collect::<Vec<_>>(),
            vec![other_normal_pos, old_low_pos, later_high_pos]
        );
    }

    #[test]
    fn exact_intra_tick_ties_keep_draining_the_current_chunk() {
        let current_high_pos = BlockPos::new(16, 0, 0);
        let current_normal_pos = BlockPos::new(17, 0, 0);
        let competing_normal_pos = BlockPos::new(0, 0, 0);
        let current_chunk = BlockTickList::from_saved_ticks(vec![
            SavedTick {
                tick_type: test_block(),
                pos: current_high_pos,
                delay: 1,
                priority: TickPriority::High,
            },
            SavedTick {
                tick_type: test_block(),
                pos: current_normal_pos,
                delay: 1,
                priority: TickPriority::Normal,
            },
        ]);
        let competing_chunk = BlockTickList::from_saved_ticks(vec![SavedTick {
            tick_type: test_block(),
            pos: competing_normal_pos,
            delay: 1,
            priority: TickPriority::Normal,
        }]);

        // Put the competitor first so its container-index tie-break would win if
        // the current container were reinserted after every pop.
        let competing_chunk_pos = ChunkPos::new(0, 0);
        let current_chunk_pos = ChunkPos::new(1, 0);
        let scheduler = scheduler_with_block_lists([
            (competing_chunk_pos, competing_chunk),
            (current_chunk_pos, current_chunk),
        ]);
        for pos in [competing_chunk_pos, current_chunk_pos] {
            if let Err(error) = scheduler.unpack_chunk(pos, 0) {
                panic!("test scheduler invariant failed: {error:?}");
            }
        }
        let selected = begin_block_tick(&scheduler, &[competing_chunk_pos, current_chunk_pos], 3);

        assert_eq!(
            selected
                .ticks
                .iter()
                .map(|tick| tick.pos)
                .collect::<Vec<_>>(),
            vec![current_high_pos, current_normal_pos, competing_normal_pos]
        );
    }

    #[test]
    fn exact_loaded_head_ties_follow_the_active_scc_order() {
        let first_tick_pos = BlockPos::new(0, 0, 0);
        let second_tick_pos = BlockPos::new(16, 0, 0);
        let first_chunk_pos = ChunkPos::new(0, 0);
        let second_chunk_pos = ChunkPos::new(1, 0);
        let first_chunk = BlockTickList::from_saved_ticks(vec![SavedTick {
            tick_type: test_block(),
            pos: first_tick_pos,
            delay: 1,
            priority: TickPriority::Normal,
        }]);
        let second_chunk = BlockTickList::from_saved_ticks(vec![SavedTick {
            tick_type: test_block(),
            pos: second_tick_pos,
            delay: 1,
            priority: TickPriority::Normal,
        }]);
        let scheduler = scheduler_with_block_lists([
            (first_chunk_pos, first_chunk),
            (second_chunk_pos, second_chunk),
        ]);
        for pos in [first_chunk_pos, second_chunk_pos] {
            if let Err(error) = scheduler.unpack_chunk(pos, 0) {
                panic!("test scheduler invariant failed: {error:?}");
            }
        }

        let selected = begin_block_tick(&scheduler, &[second_chunk_pos, first_chunk_pos], 2);
        assert_eq!(
            selected
                .ticks
                .iter()
                .map(|tick| tick.pos)
                .collect::<Vec<_>>(),
            [second_tick_pos, first_tick_pos]
        );
    }

    #[test]
    fn ineligible_live_head_stays_indexed_until_reentry() {
        let registered_pos = ChunkPos::new(0, 0);
        let mut pending = BlockTickList::new();
        assert!(schedule(
            &mut pending,
            test_block(),
            BlockPos::new(0, 0, 0),
            3,
            TickPriority::Normal,
            0
        ));
        let scheduler = scheduler_with_block_lists([(registered_pos, pending)]);

        let inactive = active_ranks(&[]);
        let inactive_batch = scheduler.begin_tick(100, &inactive, 1);
        assert!(inactive_batch.ticks.is_empty());
        assert_eq!(
            scheduler.state.lock().block_next_tick.get(&registered_pos),
            Some(&3)
        );

        let selected = begin_block_tick_at(&scheduler, 100, &[registered_pos], 1);
        assert_eq!(selected.ticks.len(), 1);
        assert_eq!(selected.changed_containers, [0]);
    }

    #[test]
    fn only_popped_containers_report_a_persistence_change() {
        let empty = BlockTickList::new();
        let mut pending = BlockTickList::new();
        assert!(schedule(
            &mut pending,
            test_block(),
            BlockPos::new(0, 0, 0),
            3,
            TickPriority::Normal,
            0
        ));
        assert_eq!(pending.pack(0)[0].delay, 3);

        let empty_pos = ChunkPos::new(0, 0);
        let pending_pos = ChunkPos::new(1, 0);
        let scheduler = scheduler_with_block_lists([(empty_pos, empty), (pending_pos, pending)]);
        let before_deadline = begin_block_tick_at(&scheduler, 1, &[empty_pos, pending_pos], 1);
        assert!(before_deadline.changed_containers.is_empty());
        let selected = begin_block_tick_at(&scheduler, 3, &[empty_pos, pending_pos], 1);
        assert_eq!(selected.changed_containers, vec![1]);
    }

    #[test]
    fn persistence_uses_absolute_time_and_rebuilds_loaded_order() {
        let mut list = BlockTickList::new();
        let first_pos = BlockPos::new(0, 0, 0);
        let second_pos = BlockPos::new(1, 0, 0);
        assert!(list.schedule(test_block(), first_pos, 105, TickPriority::Normal, 100));
        assert!(list.schedule(test_block(), second_pos, 105, TickPriority::Normal, 101));

        let saved = list.pack(102);
        assert_eq!(
            saved.iter().map(|tick| tick.delay).collect::<Vec<_>>(),
            vec![3, 3]
        );

        let mut loaded = BlockTickList::from_saved_ticks(saved);
        assert!(loaded.schedule(
            test_block(),
            BlockPos::new(2, 0, 0),
            203,
            TickPriority::Normal,
            0
        ));
        loaded.unpack(200);
        assert!(loaded.drain_ready(202).is_empty());
        let ready = loaded.drain_ready(203);

        assert_eq!(
            ready
                .iter()
                .map(|tick| tick.sub_tick_order)
                .collect::<Vec<_>>(),
            vec![-2, -1, 0]
        );
        assert_eq!(ready[0].pos, first_pos);
        assert_eq!(ready[1].pos, second_pos);
    }

    #[test]
    fn proto_saved_ticks_deduplicate_in_first_occurrence_order() {
        let pos = BlockPos::new(1, 2, 3);
        let proto = BlockTickList::from_proto_saved_ticks(vec![
            SavedTick {
                tick_type: test_block(),
                pos,
                delay: 7,
                priority: TickPriority::High,
            },
            SavedTick {
                tick_type: test_block(),
                pos,
                delay: 2,
                priority: TickPriority::Low,
            },
        ]);

        let saved = proto.pack(0);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].delay, 7);
        assert_eq!(saved[0].priority, TickPriority::High);
    }

    #[test]
    fn unpack_preserves_proto_tick_insertion_order() {
        let mut proto_ticks = BlockTickList::new_pending();
        let first_pos = BlockPos::new(0, 0, 0);
        let second_pos = BlockPos::new(1, 0, 0);
        assert!(proto_ticks.schedule_pending(test_block(), first_pos, TickPriority::Normal));
        assert!(proto_ticks.schedule_pending(test_block(), second_pos, TickPriority::Normal));

        proto_ticks.unpack(50);
        let ready = proto_ticks.drain_ready(50);
        assert_eq!(
            ready.iter().map(|tick| tick.pos).collect::<Vec<_>>(),
            vec![first_pos, second_pos]
        );
        assert_eq!(
            ready
                .iter()
                .map(|tick| tick.sub_tick_order)
                .collect::<Vec<_>>(),
            vec![-2, -1]
        );
    }

    #[test]
    fn execution_snapshot_contains_only_ticks_that_have_not_started() {
        let first = BlockTick {
            tick_type: test_block(),
            pos: BlockPos::new(0, 0, 0),
            trigger_tick: 1,
            priority: TickPriority::Normal,
            sub_tick_order: 0,
        };
        let second = BlockTick {
            tick_type: test_block(),
            pos: BlockPos::new(1, 0, 0),
            trigger_tick: 1,
            priority: TickPriority::Normal,
            sub_tick_order: 1,
        };
        let mut run_set = ScheduledTickRunSet::default();
        run_set.begin(&[first, second]);

        assert!(run_set.contains(first.pos, first.tick_type));
        assert!(run_set.contains(second.pos, second.tick_type));
        run_set.start(&first);
        assert!(!run_set.contains(first.pos, first.tick_type));
        assert!(run_set.contains(second.pos, second.tick_type));
        run_set.clear();
        assert!(!run_set.contains(second.pos, second.tick_type));
    }

    #[test]
    fn can_reschedule_after_ready_tick_is_removed() {
        let mut list = BlockTickList::new();
        let block = test_block();
        let pos = BlockPos::new(0, 0, 0);
        assert!(schedule(&mut list, block, pos, 1, TickPriority::Normal, 0));
        assert_eq!(list.drain_ready(1).len(), 1);
        assert!(schedule(&mut list, block, pos, 5, TickPriority::Normal, 1));
    }

    #[test]
    fn priority_ordering_matches_vanilla_discriminants() {
        assert!(TickPriority::ExtremelyHigh < TickPriority::Normal);
        assert!(TickPriority::Normal < TickPriority::ExtremelyLow);
        assert!(TickPriority::High < TickPriority::Low);
    }
}
