//! Scheduled tick storage and selection for deterministic block and fluid updates.
//!
//! Scheduled ticks are stored in per-chunk priority queues. Within a chunk, the
//! queue follows vanilla's `ScheduledTick.DRAIN_ORDER`: trigger time, priority,
//! then sub-tick order. Across chunks, only each ready queue head participates
//! in selection, following vanilla's `LevelTicks` container-draining behavior.
//!
//! ## Intentional difference from Vanilla
//!
//! Vanilla uses the world's absolute game time for in-memory trigger times. Game
//! time continues advancing while a loaded chunk is outside the block-ticking
//! range, so multiple repeater deadlines can become overdue and execute together
//! when the chunk starts ticking again. Steel intentionally advances a chunk's
//! scheduled-tick clock only while `ChunkMap` confirms that chunk is block-ticking.
//! This preserves the spacing and phase of repeater clocks across the loaded but
//! non-ticking zone. Remaining active-time delay is saved with the chunk and
//! re-anchored when loaded.
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
    /// Deadline on the owning container's active-time clock.
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
    /// Remaining active-time delay when the chunk was saved.
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
    pub(crate) fn snapshot(&self) -> ScheduledTickSnapshot {
        ScheduledTickSnapshot {
            block: self.block.pack(),
            fluid: self.fluid.pack(),
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
        state.chunks.insert(chunk.pos, ticks);
        Ok(())
    }

    /// Removes a finally-unloaded Full chunk after its last save completed.
    pub(crate) fn unregister_chunk(&self, pos: ChunkPos) {
        self.state.lock().chunks.remove(&pos);
    }

    #[cfg(test)]
    pub(crate) fn has_registered_chunk(&self, pos: ChunkPos) -> bool {
        self.state.lock().chunks.contains_key(&pos)
    }

    pub(crate) fn schedule_block(
        &self,
        chunk: &LevelChunk,
        block: BlockRef,
        pos: BlockPos,
        delay: i32,
        priority: TickPriority,
        sub_tick_order: i64,
    ) -> Result<bool, TickSchedulerError> {
        let mut state = self.state.lock();
        if let Some(ticks) = state.chunks.get_mut(&chunk.pos) {
            return Ok(ticks
                .block
                .schedule(block, pos, delay, priority, sub_tick_order));
        }
        chunk
            .schedule_unregistered_block_tick(block, pos, delay, priority, sub_tick_order)
            .ok_or(TickSchedulerError::MissingContainer(chunk.pos))
    }

    pub(crate) fn schedule_fluid(
        &self,
        chunk: &LevelChunk,
        fluid: FluidRef,
        pos: BlockPos,
        delay: i32,
        priority: TickPriority,
        sub_tick_order: i64,
    ) -> Result<bool, TickSchedulerError> {
        let mut state = self.state.lock();
        if let Some(ticks) = state.chunks.get_mut(&chunk.pos) {
            return Ok(ticks
                .fluid
                .schedule(fluid, pos, delay, priority, sub_tick_order));
        }
        chunk
            .schedule_unregistered_fluid_tick(fluid, pos, delay, priority, sub_tick_order)
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
    ) -> Result<ScheduledTickSnapshot, TickSchedulerError> {
        let state = self.state.lock();
        if let Some(ticks) = state.chunks.get(&chunk.pos) {
            return Ok(ticks.snapshot());
        }
        chunk
            .snapshot_unregistered_tick_lists()
            .ok_or(TickSchedulerError::MissingContainer(chunk.pos))
    }

    /// Advances both active clocks and selects the block batch.
    pub(crate) fn begin_tick(
        &self,
        active_chunks: &[ChunkPos],
        max_ticks: usize,
    ) -> Result<ScheduledTickBatch<BlockRef>, TickSchedulerError> {
        let mut state = self.state.lock();
        Self::verify_registered(&state, active_chunks)?;

        let mut changed_containers = Vec::new();
        for (index, pos) in active_chunks.iter().enumerate() {
            let Some(ticks) = state.chunks.get_mut(pos) else {
                continue;
            };
            if !ticks.block.is_empty() || !ticks.fluid.is_empty() {
                changed_containers.push(index);
            }
            ticks.block.advance_active_time();
            ticks.fluid.advance_active_time();
        }

        let ticks = collect_registered_ticks(
            &mut state.chunks,
            active_chunks,
            ChunkTickLists::block_mut,
            max_ticks,
        );
        Ok(ScheduledTickBatch {
            ticks,
            changed_containers,
        })
    }

    /// Selects fluids after block callbacks, without advancing clocks again.
    pub(crate) fn collect_fluid_ticks(
        &self,
        active_chunks: &[ChunkPos],
        max_ticks: usize,
    ) -> Result<Vec<FluidTick>, TickSchedulerError> {
        let mut state = self.state.lock();
        Self::verify_registered(&state, active_chunks)?;
        Ok(collect_registered_ticks(
            &mut state.chunks,
            active_chunks,
            ChunkTickLists::fluid_mut,
            max_ticks,
        ))
    }

    fn verify_registered(
        state: &WorldTickSchedulerState,
        active_chunks: &[ChunkPos],
    ) -> Result<(), TickSchedulerError> {
        for pos in active_chunks {
            if !state.chunks.contains_key(pos) {
                return Err(TickSchedulerError::MissingContainer(*pos));
            }
        }
        Ok(())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadyContainer {
    index: usize,
    priority: TickPriority,
    sub_tick_order: i64,
}

impl ReadyContainer {
    const fn new<T: TickKey>(index: usize, tick: ScheduledTick<T>) -> Self {
        Self {
            index,
            priority: tick.priority,
            sub_tick_order: tick.sub_tick_order,
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
        .then_with(|| other.index.cmp(&self.index))
    }
}

/// Per-chunk storage for scheduled ticks of one type (block or fluid).
///
/// The active-time clock advances only when this container's chunk is eligible
/// for block ticks. A priority queue keeps future work ordered without scanning
/// every pending tick each game tick.
#[derive(Debug)]
pub struct TickList<T: TickKey> {
    active_tick: i64,
    ticks: BinaryHeap<QueuedTick<T>>,
    scheduled: FxHashSet<ScheduledTickKey>,
    next_insertion_order: u64,
}

impl<T: TickKey> TickList<T> {
    /// Creates an empty tick list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            active_tick: 0,
            ticks: BinaryHeap::new(),
            scheduled: FxHashSet::default(),
            next_insertion_order: 0,
        }
    }

    /// Creates a tick list from relative-delay ticks loaded from chunk storage.
    ///
    /// Vanilla assigns loaded entries the range `-len..-1` in saved list order,
    /// ensuring they execute before newly scheduled entries with equal timing.
    #[must_use]
    pub(crate) fn from_saved_ticks(saved_ticks: Vec<SavedTick<T>>) -> Self {
        let tick_count = saved_ticks.len() as i64;
        let mut result = Self::new();
        result.ticks.reserve(saved_ticks.len());
        result.scheduled.reserve(saved_ticks.len());

        for (index, saved_tick) in saved_ticks.into_iter().enumerate() {
            let tick = ScheduledTick {
                tick_type: saved_tick.tick_type,
                pos: saved_tick.pos,
                trigger_tick: i64::from(saved_tick.delay),
                priority: saved_tick.priority,
                sub_tick_order: -tick_count + index as i64,
            };
            result.scheduled.insert(tick.key());
            result.push_unchecked(tick);
        }

        result
    }

    /// Schedules a tick relative to this container's current active time.
    ///
    /// Returns `true` if the tick was added, or `false` when the same `(pos, type)`
    /// is already scheduled.
    pub fn schedule(
        &mut self,
        tick_type: T,
        pos: BlockPos,
        delay: i32,
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
            trigger_tick: self.active_tick.wrapping_add(i64::from(delay)),
            priority,
            sub_tick_order,
        });
        true
    }

    /// Returns `true` if a tick is scheduled for the given `(pos, type)`.
    #[must_use]
    pub fn has_tick(&self, pos: BlockPos, tick_type: T) -> bool {
        self.scheduled.contains(&(pos, tick_type.key()))
    }

    /// Packs ticks as relative active-time delays in Vanilla saved-list order.
    #[must_use]
    pub(crate) fn pack(&self) -> Vec<SavedTick<T>> {
        let mut ticks: Vec<_> = self.ticks.iter().collect();
        ticks.sort_by(|a, b| {
            a.tick
                .sub_tick_order
                .cmp(&b.tick.sub_tick_order)
                .then_with(|| a.insertion_order.cmp(&b.insertion_order))
        });

        ticks
            .into_iter()
            .map(|queued| SavedTick {
                tick_type: queued.tick.tick_type,
                pos: queued.tick.pos,
                delay: queued.tick.trigger_tick.wrapping_sub(self.active_tick) as i32,
                priority: queued.tick.priority,
            })
            .collect()
    }

    /// Converts proto-chunk pending ticks into live loaded-tick ordering.
    ///
    /// This mirrors `LevelChunkTicks.unpack`: remaining delays are re-anchored and
    /// all entries receive negative sub-tick orders in their packed list order.
    pub(crate) fn unpack(&mut self) {
        let saved_ticks = self.pack();
        *self = Self::from_saved_ticks(saved_ticks);
    }

    /// Returns the number of scheduled ticks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ticks.len()
    }

    /// Returns `true` if no ticks are scheduled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ticks.is_empty()
    }

    fn push_unchecked(&mut self, tick: ScheduledTick<T>) {
        let insertion_order = self.next_insertion_order;
        self.next_insertion_order = self.next_insertion_order.wrapping_add(1);
        self.ticks.push(QueuedTick {
            tick,
            insertion_order,
        });
    }

    const fn advance_active_time(&mut self) {
        self.active_tick = self.active_tick.wrapping_add(1);
    }

    fn peek_ready(&self) -> Option<ScheduledTick<T>> {
        let tick = self.ticks.peek()?.tick;
        (tick.trigger_tick <= self.active_tick).then_some(tick)
    }

    fn pop_ready(&mut self) -> Option<ScheduledTick<T>> {
        self.peek_ready()?;
        let tick = self.ticks.pop()?.tick;
        self.scheduled.remove(&tick.key());
        Some(tick)
    }

    #[cfg(test)]
    fn drain_ready(&mut self) -> Vec<ScheduledTick<T>> {
        self.advance_active_time();
        let mut ready = Vec::new();
        while let Some(tick) = self.pop_ready() {
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

/// Selects at most `max_ticks` ready entries from already-advanced containers.
///
/// Only queue heads compete globally. Revealing the next head after each pop is
/// what preserves Vanilla's per-chunk deadline ordering when several ticks are
/// already overdue.
fn collect_registered_ticks<T: TickKey>(
    chunks: &mut FxHashMap<ChunkPos, ChunkTickLists>,
    active_chunks: &[ChunkPos],
    select: fn(&mut ChunkTickLists) -> &mut TickList<T>,
    max_ticks: usize,
) -> Vec<ScheduledTick<T>> {
    let mut ready_containers = BinaryHeap::with_capacity(active_chunks.len());
    for (index, pos) in active_chunks.iter().enumerate() {
        let Some(container) = chunks.get_mut(pos).map(select) else {
            continue;
        };
        if let Some(tick) = container.peek_ready() {
            ready_containers.push(ReadyContainer::new(index, tick));
        }
    }

    let mut ticks = Vec::with_capacity(max_ticks.min(ready_containers.len()));
    while ticks.len() < max_ticks {
        let Some(ready_container) = ready_containers.pop() else {
            break;
        };
        let Some(container) = chunks
            .get_mut(&active_chunks[ready_container.index])
            .map(select)
        else {
            continue;
        };
        let Some(tick) = container.pop_ready() else {
            continue;
        };

        ticks.push(tick);

        // Vanilla keeps draining the current container while its next head is
        // no later in intra-tick order than the best competing container. In
        // particular, an exact tie stays with the current container.
        let next_competing_container = ready_containers.peek().copied();
        while ticks.len() < max_ticks {
            let Some(next_tick) = container.peek_ready() else {
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
            let Some(next_tick) = container.pop_ready() else {
                break;
            };
            ticks.push(next_tick);
        }

        if ticks.len() < max_ticks
            && let Some(next_tick) = container.peek_ready()
        {
            ready_containers.push(ReadyContainer::new(ready_container.index, next_tick));
        }
    }

    ticks
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
        list.schedule(block, pos, delay, priority, sub_tick_order)
    }

    fn scheduler_with_block_lists(
        chunks: impl IntoIterator<Item = (ChunkPos, BlockTickList)>,
    ) -> WorldTickScheduler {
        let scheduler = WorldTickScheduler::new();
        {
            let mut state = scheduler.state.lock();
            for (pos, block) in chunks {
                state
                    .chunks
                    .insert(pos, ChunkTickLists::new(block, FluidTickList::new()));
            }
        }
        scheduler
    }

    fn begin_block_tick(
        scheduler: &WorldTickScheduler,
        active_chunks: &[ChunkPos],
        max_ticks: usize,
    ) -> ScheduledTickBatch<BlockRef> {
        match scheduler.begin_tick(active_chunks, max_ticks) {
            Ok(batch) => batch,
            Err(error) => panic!("test scheduler invariant failed: {error:?}"),
        }
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
    fn active_time_preserves_spacing_across_a_pause() {
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

        assert_eq!(list.drain_ready()[0].pos, first_pos);

        // No call while the chunk is outside the block-ticking range: its clock is paused.
        assert!(list.drain_ready().is_empty());
        assert!(list.drain_ready().is_empty());
        assert_eq!(list.drain_ready()[0].pos, fourth_pos);
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
    fn block_and_fluid_collection_share_one_clock_advance() {
        let chunk_pos = ChunkPos::new(0, 0);
        let block_pos = BlockPos::new(0, 0, 0);
        let fluid_pos = BlockPos::new(1, 0, 0);
        let scheduler = scheduler_with_block_lists([(chunk_pos, BlockTickList::new())]);

        assert!(
            begin_block_tick(&scheduler, &[chunk_pos], 2)
                .ticks
                .is_empty()
        );
        {
            let mut state = scheduler.state.lock();
            let Some(ticks) = state.chunks.get_mut(&chunk_pos) else {
                panic!("test chunk must remain registered");
            };
            assert!(
                ticks
                    .block
                    .schedule(test_block(), block_pos, 0, TickPriority::Normal, 0)
            );
            assert!(ticks.fluid.schedule(
                &vanilla_fluids::WATER,
                fluid_pos,
                0,
                TickPriority::Normal,
                1
            ));
        }

        let fluids = match scheduler.collect_fluid_ticks(&[chunk_pos], 2) {
            Ok(ticks) => ticks,
            Err(error) => panic!("test scheduler invariant failed: {error:?}"),
        };
        assert_eq!(
            fluids.iter().map(|tick| tick.pos).collect::<Vec<_>>(),
            [fluid_pos]
        );

        let blocks = begin_block_tick(&scheduler, &[chunk_pos], 2);
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

        let selected = begin_block_tick(&scheduler, &[first_pos, second_pos], 3);
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
    fn missing_active_container_does_not_partially_advance_clocks() {
        let registered_pos = ChunkPos::new(0, 0);
        let missing_pos = ChunkPos::new(1, 0);
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

        let Err(error) = scheduler.begin_tick(&[registered_pos, missing_pos], 1) else {
            panic!("missing active container must reject the whole collection");
        };
        assert_eq!(error, TickSchedulerError::MissingContainer(missing_pos));
        let state = scheduler.state.lock();
        let Some(ticks) = state.chunks.get(&registered_pos) else {
            panic!("test chunk must remain registered");
        };
        assert_eq!(ticks.block.pack()[0].delay, 3);
    }

    #[test]
    fn advancing_pending_ticks_reports_a_persistence_change() {
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
        assert_eq!(pending.pack()[0].delay, 3);

        let empty_pos = ChunkPos::new(0, 0);
        let pending_pos = ChunkPos::new(1, 0);
        let scheduler = scheduler_with_block_lists([(empty_pos, empty), (pending_pos, pending)]);
        let selected = begin_block_tick(&scheduler, &[empty_pos, pending_pos], 0);
        assert_eq!(selected.changed_containers, vec![1]);
        let state = scheduler.state.lock();
        let Some(ticks) = state.chunks.get(&pending_pos) else {
            panic!("test chunk must remain registered");
        };
        assert_eq!(ticks.block.pack()[0].delay, 2);
    }

    #[test]
    fn persistence_saves_remaining_active_delay_and_rebuilds_loaded_order() {
        let mut list = BlockTickList::new();
        let first_pos = BlockPos::new(0, 0, 0);
        let second_pos = BlockPos::new(1, 0, 0);
        assert!(schedule(
            &mut list,
            test_block(),
            first_pos,
            5,
            TickPriority::Normal,
            100
        ));
        assert!(schedule(
            &mut list,
            test_block(),
            second_pos,
            5,
            TickPriority::Normal,
            101
        ));
        assert!(list.drain_ready().is_empty());
        assert!(list.drain_ready().is_empty());

        let saved = list.pack();
        assert_eq!(
            saved.iter().map(|tick| tick.delay).collect::<Vec<_>>(),
            vec![3, 3]
        );

        let mut loaded = BlockTickList::from_saved_ticks(saved);
        assert!(schedule(
            &mut loaded,
            test_block(),
            BlockPos::new(2, 0, 0),
            3,
            TickPriority::Normal,
            0
        ));
        assert!(loaded.drain_ready().is_empty());
        assert!(loaded.drain_ready().is_empty());
        let ready = loaded.drain_ready();

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
    fn unpack_preserves_proto_tick_insertion_order() {
        let mut proto_ticks = BlockTickList::new();
        let first_pos = BlockPos::new(0, 0, 0);
        let second_pos = BlockPos::new(1, 0, 0);
        assert!(schedule(
            &mut proto_ticks,
            test_block(),
            first_pos,
            0,
            TickPriority::Normal,
            0
        ));
        assert!(schedule(
            &mut proto_ticks,
            test_block(),
            second_pos,
            0,
            TickPriority::Normal,
            0
        ));

        proto_ticks.unpack();
        let ready = proto_ticks.drain_ready();
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
        assert_eq!(list.drain_ready().len(), 1);
        assert!(schedule(&mut list, block, pos, 5, TickPriority::Normal, 1));
    }

    #[test]
    fn priority_ordering_matches_vanilla_discriminants() {
        assert!(TickPriority::ExtremelyHigh < TickPriority::Normal);
        assert!(TickPriority::Normal < TickPriority::ExtremelyLow);
        assert!(TickPriority::High < TickPriority::Low);
    }
}
