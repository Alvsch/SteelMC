//! A 2D layer over [`MenuBuilder`] for menus whose slot indices are row-major
//! over a visual grid — the generic chest screens (`GENERIC_9X1` ...
//! `GENERIC_9X6`).
//!
//! Instead of counting flat slot runs by hand, place containers as rectangles
//! and let the grid lower them into sections:
//!
//! ```rust
//! use steel_registry::{vanilla_items, vanilla_menu_types};
//! use steel_core::inventory::menu::kinds::BasicKind;
//! use steel_core::player::player_inventory::PlayerInventory;
//!
//! use steel_core::inventory::prelude::*;
//!
//! fn example(container_id: u8, inventory: Shared<PlayerInventory>) -> Menu {
//!     let mut b = MenuBuilder::new(&vanilla_menu_types::GENERIC_9X3, container_id);
//!
//!     let storage = SimpleContainer::new(3).into_shared();
//!
//!     let items = b.grid(3, |g| {
//!         let items = g.place(Rect::new(3, 1, 3, 1), storage);
//!         g.paint_all(ItemStack::new(&vanilla_items::GRAY_STAINED_GLASS_PANE));
//!         items
//!     });
//!
//!     let player = b.player_inventory(&inventory);
//!     for section in items.sections() {
//!         b.route(section, [player.all()], FillDirection::Backward);
//!     }
//!     b.route(player.all(), &items, FillDirection::Forward);
//!     b.build(MenuKindType::Basic(BasicKind {}))
//! }
//! ```
//!
//! # Rules
//!
//! - **Placements never overlap.** A cell can belong to at most one
//!   container-backed placement ([`GridPlacer::place`],
//!   [`GridPlacer::place_restricted`], [`GridPlacer::place_display`],
//!   [`GridPlacer::place_result`]); a second claim panics with the offending
//!   cell.
//! - **Paint is decoration.** [`GridPlacer::paint`] layers freely (the last
//!   paint on a cell wins) and is always masked by placements, regardless of
//!   call order. Painted cells become locked display slots of one
//!   auto-sized filler container.
//! - **Every cell must be accounted for.** When a grid scope closes, any cell
//!   that is neither placed nor painted panics — a hole is a miscounted rect,
//!   not an implicit empty slot.
//! - **Subgrids are self-contained.** [`GridPlacer::subgrid`] runs a closure
//!   against a rectangular sub-area with its own local coordinates and its own
//!   coverage check; parent paint does not bleed into it.
//! - **Carving is sugar.** [`GridPlacer::rows`], [`GridPlacer::cols`] and
//!   [`GridPlacer::rest`] are cursor-computed subgrids, so tiled layouts need
//!   no coordinates at all. One carve axis per scope; nest to switch axes.
//!
//! This layer only fits menu types whose slot order is row-major over the
//! screen. Vanilla work-station menus (crafting table, anvil, ...) have
//! protocol-defined, non-geometric slot orders — build those with the plain
//! [`MenuBuilder`] methods.

use std::iter::Copied;
use std::slice;
use std::sync::Arc;

use steel_registry::item_stack::ItemStack;
use steel_utils::locks::IntoShared;

use crate::inventory::container::SimpleContainer;
use crate::inventory::lock::{ContainerLockGuard, ContainerRef};
use crate::inventory::menu::builder::{MenuBuilder, MenuInstanceId, Section};
use crate::inventory::slots::{
    MayPickupFn, MayPlaceFn, NormalSlot, RestrictedSlot, ResultHandler, ResultSlot, SlotType,
};
use crate::player::Player;

/// The width of the generic chest screens.
const GRID_WIDTH: usize = 9;

/// A rectangle of grid cells. `x` is the column and `y` the row, both 0-based
/// from the top-left of the enclosing grid scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl Rect {
    /// Creates a rect at column `x`, row `y`, spanning `w` columns and `h` rows.
    ///
    /// # Panics
    /// If `w` or `h` is zero.
    #[must_use]
    pub const fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        assert!(w > 0 && h > 0, "Rect must span at least one cell");
        Self { x, y, w, h }
    }

    /// A single cell at column `x`, row `y`.
    #[must_use]
    pub const fn cell(x: usize, y: usize) -> Self {
        Self::new(x, y, 1, 1)
    }

    /// The number of cells covered.
    #[must_use]
    pub const fn area(self) -> usize {
        self.w * self.h
    }

    /// Whether the cell at `(x, y)` lies within this rect.
    const fn contains_cell(self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    /// The row-major index of the cell `(x, y)` within this rect.
    const fn local_index(self, x: usize, y: usize) -> usize {
        (y - self.y) * self.w + (x - self.x)
    }

    /// Iterates the covered cells in row-major order.
    fn cells(self) -> impl Iterator<Item = (usize, usize)> {
        (self.y..self.y + self.h).flat_map(move |y| (self.x..self.x + self.w).map(move |x| (x, y)))
    }
}

/// The sections created by one grid placement.
///
/// A placement narrower than the grid is not contiguous in flat slot space, so
/// it lowers to one [`Section`] per covered row; flat-adjacent rows (full-width
/// placements) are merged. `&Region` iterates the sections, so it can be passed
/// directly to [`MenuBuilder::drain`] and as [`MenuBuilder::route`] targets.
#[derive(Clone, Debug)]
pub struct Region {
    sections: Vec<Section>,
}

impl Region {
    /// Iterates the sections of this region, in flat slot order.
    pub fn iter(&self) -> Copied<slice::Iter<'_, Section>> {
        self.sections.iter().copied()
    }

    /// The sections of this region, in flat slot order. Alias for
    /// [`iter`](Self::iter).
    pub fn sections(&self) -> Copied<slice::Iter<'_, Section>> {
        self.iter()
    }

    /// Whether any section of this region contains the slot index.
    #[must_use]
    pub fn contains(&self, slot_index: usize) -> bool {
        self.sections.iter().any(|s| s.contains(slot_index))
    }

    /// The region's only section.
    ///
    /// # Panics
    /// If the region is not one contiguous slot range (a placement narrower
    /// than the grid spanning several rows).
    #[must_use]
    pub fn single(&self) -> Section {
        assert!(
            self.sections.len() == 1,
            "region covers {} non-contiguous slot ranges; iterate sections() instead",
            self.sections.len()
        );
        self.sections[0]
    }
}

impl<'a> IntoIterator for &'a Region {
    type Item = Section;
    type IntoIter = Copied<slice::Iter<'a, Section>>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// What one grid cell resolved to.
enum Cell {
    /// Not yet claimed by anything. A leftover `Empty` when a scope closes is
    /// a coverage error.
    Empty,
    /// Decoration, backed by the synthesized filler container.
    Painted(ItemStack),
    /// Claimed by the placement at this index into [`GridState::placements`].
    Functional(usize),
}

/// One container-backed placement, in absolute grid coordinates.
struct Placement {
    rect: Rect,
    kind: PlacementKind,
}

enum PlacementKind {
    /// Plain item slots; cell `(x, y)` maps to container slot
    /// `offset + rect.local_index(x, y)`.
    Normal {
        container: ContainerRef,
        offset: usize,
    },
    /// Slots guarded by `may_place`/`may_pickup` closures, shared across the
    /// placement. Display placements are the always-false special case.
    Restricted {
        container: ContainerRef,
        offset: usize,
        may_place: MayPlaceFn,
        may_pickup: Option<MayPickupFn>,
    },
    /// A single fake result slot driven by a handler.
    Result {
        handler: Arc<dyn ResultHandler + Send + Sync>,
        container: ContainerRef,
    },
}

impl PlacementKind {
    const fn container(&self) -> &ContainerRef {
        match self {
            Self::Normal { container, .. }
            | Self::Restricted { container, .. }
            | Self::Result { container, .. } => container,
        }
    }
}

/// The carve axis of [`GridPlacer::rows`] / [`GridPlacer::cols`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    Rows,
    Cols,
}

/// Grid-wide state shared by all nested [`GridPlacer`] scopes.
struct GridState {
    instance: MenuInstanceId,
    /// Flat slot index of the grid's top-left cell in the menu.
    base: usize,
    width: usize,
    cells: Vec<Cell>,
    placements: Vec<Placement>,
}

impl GridState {
    const fn cell_index(&self, x: usize, y: usize) -> usize {
        y * self.width + x
    }
}

/// One grid scope: the whole grid inside [`MenuBuilder::grid`], or a sub-area
/// inside [`GridPlacer::subgrid`] / the carving methods.
struct Frame {
    /// This scope's area, in absolute grid coordinates.
    rect: Rect,
    /// The axis locked in by the first `rows`/`cols` call in this scope.
    axis: Option<Axis>,
    /// Rows or columns (per `axis`) already carved off.
    cursor: usize,
    /// Closed subgrids of this scope, in absolute grid coordinates.
    sealed: Vec<Rect>,
}

impl Frame {
    const fn new(rect: Rect) -> Self {
        Self {
            rect,
            axis: None,
            cursor: 0,
            sealed: Vec::new(),
        }
    }
}

/// Places rectangles on a grid scope. Created by [`MenuBuilder::grid`]; see
/// the [module documentation](self) for the rules.
///
/// All coordinates are local to this scope: `(0, 0)` is its top-left cell,
/// [`width`](Self::width) x [`height`](Self::height) its extent.
pub struct GridPlacer<'a> {
    state: &'a mut GridState,
    frame: Frame,
}

impl GridPlacer<'_> {
    /// The number of columns in this scope.
    #[must_use]
    pub const fn width(&self) -> usize {
        self.frame.rect.w
    }

    /// The number of rows in this scope.
    #[must_use]
    pub const fn height(&self) -> usize {
        self.frame.rect.h
    }

    /// The rect covering this whole scope.
    #[must_use]
    pub const fn full(&self) -> Rect {
        Rect::new(0, 0, self.frame.rect.w, self.frame.rect.h)
    }

    /// Adds plain slots for the container over `rect`, covering its slots
    /// `0..rect.area()` in row-major order.
    ///
    /// # Panics
    /// If the rect exceeds this scope or overlaps another placement or a
    /// subgrid. In debug builds, if the container has fewer slots than the
    /// rect has cells.
    pub fn place(&mut self, rect: Rect, container: impl Into<ContainerRef>) -> Region {
        self.place_at_offset(rect, container, 0)
    }

    /// Like [`place`](Self::place), but covering the container slots starting
    /// at `offset` — for carving one container into several placements.
    ///
    /// # Panics
    /// See [`place`](Self::place).
    pub fn place_at_offset(
        &mut self,
        rect: Rect,
        container: impl Into<ContainerRef>,
        offset: usize,
    ) -> Region {
        let container = container.into();
        #[cfg(debug_assertions)]
        Self::assert_container_size(&container, rect, offset);
        self.claim_functional(rect, PlacementKind::Normal { container, offset })
    }

    /// Adds slots for the container over `rect` whose interactions are guarded
    /// by `may_place` and `may_pickup` (the grid analogue of
    /// [`MenuBuilder::restricted_section`]). The closures are shared across
    /// all slots of the placement.
    ///
    /// # Panics
    /// See [`place`](Self::place).
    pub fn place_restricted(
        &mut self,
        rect: Rect,
        container: impl Into<ContainerRef>,
        may_place: impl Fn(usize, &ItemStack) -> bool + Send + Sync + 'static,
        may_pickup: Option<
            impl Fn(usize, &ContainerLockGuard, &Player, &ItemStack) -> bool + Send + Sync + 'static,
        >,
    ) -> Region {
        self.place_restricted_at_offset(rect, container, 0, may_place, may_pickup)
    }

    /// Like [`place_restricted`](Self::place_restricted), but covering the
    /// container slots starting at `offset`.
    ///
    /// # Panics
    /// See [`place`](Self::place).
    pub fn place_restricted_at_offset(
        &mut self,
        rect: Rect,
        container: impl Into<ContainerRef>,
        offset: usize,
        may_place: impl Fn(usize, &ItemStack) -> bool + Send + Sync + 'static,
        may_pickup: Option<
            impl Fn(usize, &ContainerLockGuard, &Player, &ItemStack) -> bool + Send + Sync + 'static,
        >,
    ) -> Region {
        let may_place: MayPlaceFn = Arc::new(may_place);
        let may_pickup = may_pickup.map(|it| -> MayPickupFn { Arc::new(it) });
        self.place_restricted_fns(rect, container, offset, may_place, may_pickup)
    }

    /// Adds locked display slots for the container over `rect` (the grid
    /// analogue of [`MenuBuilder::display_section`]). Clicks are rejected and
    /// can be handled in `MenuKind::on_slot_clicked`.
    ///
    /// Use this over [`paint`](Self::paint) when the shown items change at
    /// runtime or slot identity matters (click menus); use `paint` for static
    /// decoration.
    ///
    /// # Panics
    /// See [`place`](Self::place).
    pub fn place_display(&mut self, rect: Rect, container: impl Into<ContainerRef>) -> Region {
        self.place_display_at_offset(rect, container, 0)
    }

    /// Like [`place_display`](Self::place_display), but covering the container
    /// slots starting at `offset`.
    ///
    /// # Panics
    /// See [`place`](Self::place).
    pub fn place_display_at_offset(
        &mut self,
        rect: Rect,
        container: impl Into<ContainerRef>,
        offset: usize,
    ) -> Region {
        self.place_restricted_fns(
            rect,
            container,
            offset,
            Arc::new(|_, _| false),
            Some(Arc::new(|_, _, _, _| false)),
        )
    }

    /// Shared lowering of restricted and display placements.
    fn place_restricted_fns(
        &mut self,
        rect: Rect,
        container: impl Into<ContainerRef>,
        offset: usize,
        may_place: MayPlaceFn,
        may_pickup: Option<MayPickupFn>,
    ) -> Region {
        let container = container.into();
        #[cfg(debug_assertions)]
        Self::assert_container_size(&container, rect, offset);
        self.claim_functional(
            rect,
            PlacementKind::Restricted {
                container,
                offset,
                may_place,
                may_pickup,
            },
        )
    }

    /// Adds a single fake result slot driven by `handler` at `at` (the grid
    /// analogue of [`MenuBuilder::result_slot`]).
    ///
    /// # Panics
    /// If `at` is not a single cell, or on the overlap/bounds conditions of
    /// [`place`](Self::place).
    pub fn place_result(
        &mut self,
        at: Rect,
        handler: Arc<dyn ResultHandler + Send + Sync>,
        container: impl Into<ContainerRef>,
    ) -> Section {
        assert!(
            at.area() == 1,
            "place_result requires a single cell, got a {}x{} rect",
            at.w,
            at.h
        );
        let region = self.claim_functional(
            at,
            PlacementKind::Result {
                handler,
                container: container.into(),
            },
        );
        region.single()
    }

    /// Paints decoration over `rect`. Painted cells become locked display
    /// slots of one auto-sized filler container.
    ///
    /// Paint is the bottom layer: placements and subgrids mask it regardless
    /// of call order, and among paints the last one on a cell wins.
    ///
    /// # Panics
    /// If the rect exceeds this scope.
    pub fn paint(&mut self, rect: Rect, stack: ItemStack) {
        let abs = self.to_abs(rect);
        for (x, y) in abs.cells() {
            if self.in_sealed(x, y) {
                continue;
            }
            let index = self.state.cell_index(x, y);
            if !matches!(self.state.cells[index], Cell::Functional(_)) {
                self.state.cells[index] = Cell::Painted(stack.clone());
            }
        }
    }

    /// Paints the whole scope; shorthand for `paint(full(), stack)`.
    pub fn paint_all(&mut self, stack: ItemStack) {
        self.paint(self.full(), stack);
    }

    /// Runs `f` against the sub-area `rect` with its own local coordinates.
    ///
    /// The subgrid is self-contained: it must fully cover its own area (its
    /// coverage is checked when `f` returns), parent paint does not reach into
    /// it, and nothing may be placed over it afterwards.
    ///
    /// # Panics
    /// If the rect exceeds this scope or overlaps a placement or another
    /// subgrid; when `f` returns, if it left cells of the sub-area uncovered.
    pub fn subgrid<R>(&mut self, rect: Rect, f: impl FnOnce(&mut GridPlacer<'_>) -> R) -> R {
        let abs = self.to_abs(rect);
        for (x, y) in abs.cells() {
            assert!(
                !self.in_sealed(x, y),
                "subgrid {rect:?} overlaps another subgrid at local cell ({}, {})",
                x - self.frame.rect.x,
                y - self.frame.rect.y
            );
            let index = self.state.cell_index(x, y);
            match self.state.cells[index] {
                Cell::Functional(_) => panic!(
                    "subgrid {rect:?} overlaps a placement at local cell ({}, {})",
                    x - self.frame.rect.x,
                    y - self.frame.rect.y
                ),
                // The subgrid owns its area and must cover it itself.
                Cell::Painted(_) => self.state.cells[index] = Cell::Empty,
                Cell::Empty => {}
            }
        }

        let mut child = GridPlacer {
            state: &mut *self.state,
            frame: Frame::new(abs),
        };
        let result = f(&mut child);
        child.check_coverage();
        self.frame.sealed.push(abs);
        result
    }

    /// Carves the next `count` rows off this scope and runs `f` against them.
    ///
    /// # Panics
    /// If `cols` was already used in this scope, if fewer than `count` rows
    /// remain, or on the subgrid conditions of [`subgrid`](Self::subgrid).
    pub fn rows<R>(&mut self, count: usize, f: impl FnOnce(&mut GridPlacer<'_>) -> R) -> R {
        self.carve(Axis::Rows, count, f)
    }

    /// Carves the next `count` columns off this scope and runs `f` against
    /// them.
    ///
    /// # Panics
    /// If `rows` was already used in this scope, if fewer than `count` columns
    /// remain, or on the subgrid conditions of [`subgrid`](Self::subgrid).
    pub fn cols<R>(&mut self, count: usize, f: impl FnOnce(&mut GridPlacer<'_>) -> R) -> R {
        self.carve(Axis::Cols, count, f)
    }

    /// Carves everything remaining on the current axis (the whole scope if
    /// nothing was carved yet) and runs `f` against it.
    ///
    /// # Panics
    /// If nothing remains to carve, or on the subgrid conditions of
    /// [`subgrid`](Self::subgrid).
    pub fn rest<R>(&mut self, f: impl FnOnce(&mut GridPlacer<'_>) -> R) -> R {
        let axis = self.frame.axis.unwrap_or(Axis::Rows);
        let remaining = match axis {
            Axis::Rows => self.height() - self.frame.cursor,
            Axis::Cols => self.width() - self.frame.cursor,
        };
        assert!(
            remaining > 0,
            "rest() called with nothing remaining to carve"
        );
        self.carve(axis, remaining, f)
    }

    fn carve<R>(
        &mut self,
        axis: Axis,
        count: usize,
        f: impl FnOnce(&mut GridPlacer<'_>) -> R,
    ) -> R {
        assert!(count > 0, "cannot carve zero rows/columns");
        assert!(
            self.frame.axis.is_none_or(|a| a == axis),
            "cannot mix rows() and cols() in one grid scope; open a subgrid to switch axes"
        );
        let (remaining, local) = match axis {
            Axis::Rows => (
                self.height() - self.frame.cursor,
                Rect::new(0, self.frame.cursor, self.width(), count),
            ),
            Axis::Cols => (
                self.width() - self.frame.cursor,
                Rect::new(self.frame.cursor, 0, count, self.height()),
            ),
        };
        assert!(
            count <= remaining,
            "carving {count} {} exceeds the {remaining} remaining",
            match axis {
                Axis::Rows => "rows",
                Axis::Cols => "columns",
            }
        );
        self.frame.axis = Some(axis);
        self.frame.cursor += count;
        self.subgrid(local, f)
    }

    /// Translates a scope-local rect to absolute grid coordinates.
    ///
    /// # Panics
    /// If the rect does not fit this scope.
    fn to_abs(&self, rect: Rect) -> Rect {
        assert!(
            rect.x + rect.w <= self.frame.rect.w && rect.y + rect.h <= self.frame.rect.h,
            "rect {rect:?} exceeds the {}x{} grid area",
            self.frame.rect.w,
            self.frame.rect.h
        );
        Rect::new(
            self.frame.rect.x + rect.x,
            self.frame.rect.y + rect.y,
            rect.w,
            rect.h,
        )
    }

    /// Whether the absolute cell `(x, y)` lies in a closed subgrid of this scope.
    fn in_sealed(&self, x: usize, y: usize) -> bool {
        self.frame.sealed.iter().any(|r| r.contains_cell(x, y))
    }

    /// Claims `rect` for a container-backed placement and mints its region.
    fn claim_functional(&mut self, rect: Rect, kind: PlacementKind) -> Region {
        let abs = self.to_abs(rect);
        for (x, y) in abs.cells() {
            assert!(
                !self.in_sealed(x, y),
                "rect {rect:?} overlaps a subgrid at local cell ({}, {})",
                x - self.frame.rect.x,
                y - self.frame.rect.y
            );
            assert!(
                !matches!(
                    self.state.cells[self.state.cell_index(x, y)],
                    Cell::Functional(_)
                ),
                "rect {rect:?} overlaps another placement at local cell ({}, {})",
                x - self.frame.rect.x,
                y - self.frame.rect.y
            );
        }

        let placement = self.state.placements.len();
        for (x, y) in abs.cells() {
            let index = self.state.cell_index(x, y);
            self.state.cells[index] = Cell::Functional(placement);
        }
        self.state.placements.push(Placement { rect: abs, kind });
        self.region_for(abs)
    }

    /// Mints the sections covering `abs`: one per row, with flat-adjacent rows
    /// (full-width placements) merged.
    fn region_for(&self, abs: Rect) -> Region {
        let mut sections: Vec<(usize, usize)> = Vec::new();
        for y in abs.y..abs.y + abs.h {
            let start = self.state.base + y * self.state.width + abs.x;
            match sections.last_mut() {
                Some(last) if last.1 == start => last.1 = start + abs.w,
                _ => sections.push((start, start + abs.w)),
            }
        }
        Region {
            sections: sections
                .into_iter()
                .map(|(start, end)| Section::new(self.state.instance, start..end))
                .collect(),
        }
    }

    /// Panics if any cell of this scope is still [`Cell::Empty`].
    fn check_coverage(&self) {
        let holes: Vec<(usize, usize)> = self
            .frame
            .rect
            .cells()
            .filter(|&(x, y)| matches!(self.state.cells[self.state.cell_index(x, y)], Cell::Empty))
            .map(|(x, y)| (x - self.frame.rect.x, y - self.frame.rect.y))
            .collect();
        assert!(
            holes.is_empty(),
            "grid area not fully covered; place or paint the local cells (column, row): {holes:?}"
        );
    }

    /// Asserts (in debug builds) that the container has enough slots for the
    /// placement.
    #[cfg(debug_assertions)]
    fn assert_container_size(container: &ContainerRef, rect: Rect, offset: usize) {
        use crate::inventory::lock::ContainerLockGuard;

        let size = ContainerLockGuard::lock_all(slice::from_ref(container))
            .get(container.container_id())
            .expect("container was just locked")
            .get_container_size();
        debug_assert!(
            offset + rect.area() <= size,
            "placement needs container slots {}..{}, but the container only has {size} slots",
            offset,
            offset + rect.area()
        );
    }
}

impl MenuBuilder {
    /// Runs `f` against a fresh 9-wide, `rows`-tall grid and appends the
    /// resulting slots in flat row-major order. Whatever `f` returns is
    /// passed through, so section handles flow out naturally.
    ///
    /// Grids compose: each call covers the *next* `rows` rows of the menu, so
    /// two `grid(3, ...)` calls tile a `GENERIC_9X6` top area. See the
    /// [module documentation](self) for placement rules and an example.
    ///
    /// # Panics
    /// If `rows` is zero, if the slots added so far do not fill complete rows,
    /// or when `f` returns with cells of the grid neither placed nor painted.
    pub fn grid<R>(&mut self, rows: usize, f: impl FnOnce(&mut GridPlacer<'_>) -> R) -> R {
        assert!(rows > 0, "grid needs at least one row");
        assert!(
            self.slot_count().is_multiple_of(GRID_WIDTH),
            "grid starts mid-row (slot {}); previous sections must fill complete rows of {GRID_WIDTH}",
            self.slot_count()
        );

        let mut state = GridState {
            instance: self.instance(),
            base: self.slot_count(),
            width: GRID_WIDTH,
            cells: (0..GRID_WIDTH * rows).map(|_| Cell::Empty).collect(),
            placements: Vec::new(),
        };
        let mut placer = GridPlacer {
            state: &mut state,
            frame: Frame::new(Rect::new(0, 0, GRID_WIDTH, rows)),
        };
        let result = f(&mut placer);
        placer.check_coverage();
        self.flush_grid(state);
        result
    }

    /// Emits the resolved grid cells as menu slots, in flat row-major order.
    fn flush_grid(&mut self, state: GridState) {
        #[cfg(debug_assertions)]
        for placement in &state.placements {
            if let PlacementKind::Normal { container, offset }
            | PlacementKind::Restricted {
                container, offset, ..
            } = &placement.kind
            {
                self.claim(container, (*offset..offset + placement.rect.area()).into());
            }
        }

        let painted: Vec<ItemStack> = state
            .cells
            .iter()
            .filter_map(|cell| match cell {
                Cell::Painted(stack) => Some(stack.clone()),
                _ => None,
            })
            .collect();
        let filler = (!painted.is_empty())
            .then(|| ContainerRef::from(SimpleContainer::from_items(painted).into_shared()));

        let deny_place: MayPlaceFn = Arc::new(|_, _| false);
        let deny_pickup: Option<MayPickupFn> = Some(Arc::new(|_, _, _, _| false));

        let mut filler_next = 0;
        for (index, cell) in state.cells.iter().enumerate() {
            let (x, y) = (index % state.width, index / state.width);
            match cell {
                Cell::Empty => unreachable!("coverage was checked before flushing"),
                Cell::Painted(_) => {
                    let container = filler
                        .clone()
                        .expect("filler exists when cells are painted");
                    self.push_slot(SlotType::Restricted(RestrictedSlot::new(
                        container,
                        filler_next,
                        deny_place.clone(),
                        deny_pickup.clone(),
                        64,
                    )));
                    filler_next += 1;
                }
                Cell::Functional(placement) => {
                    let Placement { rect, kind } = &state.placements[*placement];
                    match kind {
                        PlacementKind::Normal { container, offset } => {
                            self.push_slot(SlotType::Normal(NormalSlot::new(
                                container.clone(),
                                offset + rect.local_index(x, y),
                            )));
                        }
                        PlacementKind::Restricted {
                            container,
                            offset,
                            may_place,
                            may_pickup,
                        } => {
                            self.push_slot(SlotType::Restricted(RestrictedSlot::new(
                                container.clone(),
                                offset + rect.local_index(x, y),
                                may_place.clone(),
                                may_pickup.clone(),
                                64,
                            )));
                        }
                        PlacementKind::Result { handler, container } => {
                            self.push_slot(SlotType::Result(ResultSlot::new(
                                handler.clone(),
                                container.clone(),
                            )));
                        }
                    }
                }
            }
        }

        for placement in state.placements {
            self.register_container(placement.kind.container().clone());
        }
        if let Some(filler) = filler {
            self.register_container(filler);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steel_utils::locks::IntoShared;

    fn container(size: usize) -> ContainerRef {
        ContainerRef::from(SimpleContainer::new(size).into_shared())
    }

    fn ranges(region: &Region) -> Vec<(usize, usize)> {
        region.sections().map(|s| (s.start(), s.end())).collect()
    }

    #[test]
    fn full_width_placement_merges_into_one_section() {
        let mut b = MenuBuilder::new(None, 0);
        let region = b.grid(2, |g| g.place(g.full(), container(18)));
        assert_eq!(ranges(&region), vec![(0, 18)]);
        assert_eq!(b.slot_count(), 18);
    }

    #[test]
    fn narrow_placement_yields_one_section_per_row() {
        let mut b = MenuBuilder::new(None, 0);
        let region = b.grid(3, |g| {
            let region = g.place(Rect::new(1, 0, 3, 3), container(9));
            g.paint_all(ItemStack::empty());
            region
        });
        assert_eq!(ranges(&region), vec![(1, 4), (10, 13), (19, 22)]);
        assert_eq!(b.slot_count(), 27);
    }

    #[test]
    fn sibling_grids_stack_vertically() {
        let mut b = MenuBuilder::new(None, 0);
        let top = b.grid(1, |g| g.place(g.full(), container(9)));
        let bottom = b.grid(1, |g| g.place(g.full(), container(9)));
        assert_eq!(ranges(&top), vec![(0, 9)]);
        assert_eq!(ranges(&bottom), vec![(9, 18)]);
    }

    #[test]
    fn cols_carve_side_by_side() {
        let mut b = MenuBuilder::new(None, 0);
        let (left, mid, right) = b.grid(2, |g| {
            let left = g.cols(4, |g| g.place(g.full(), container(8)));
            let mid = g.cols(1, |g| g.place(g.full(), container(2)));
            let right = g.rest(|g| g.place(g.full(), container(8)));
            (left, mid, right)
        });
        assert_eq!(ranges(&left), vec![(0, 4), (9, 13)]);
        assert_eq!(ranges(&mid), vec![(4, 5), (13, 14)]);
        assert_eq!(ranges(&right), vec![(5, 9), (14, 18)]);
    }

    #[test]
    fn rows_and_offset_carve_one_container() {
        let mut b = MenuBuilder::new(None, 0);
        let shared = container(54);
        let (top, body) = b.grid(6, |g| {
            let top = g.rows(1, |g| g.place(g.full(), shared.clone()));
            let body = g.rest(|g| g.place_at_offset(g.full(), shared.clone(), 9));
            (top.single(), body.single())
        });
        assert_eq!((top.start(), top.end()), (0, 9));
        assert_eq!((body.start(), body.end()), (9, 54));
    }

    #[test]
    fn restricted_placement_covers_like_place() {
        let mut b = MenuBuilder::new(None, 0);
        let region = b.grid(2, |g| {
            let region = g.place_restricted(
                Rect::new(2, 0, 3, 2),
                container(6),
                |_slot, _stack| true,
                Some(|_: usize, _: &ContainerLockGuard, _: &Player, _: &ItemStack| false),
            );
            g.paint_all(ItemStack::empty());
            region
        });
        assert_eq!(ranges(&region), vec![(2, 5), (11, 14)]);
        assert_eq!(b.slot_count(), 18);
    }

    #[test]
    fn placements_mask_paint_in_any_order() {
        let mut b = MenuBuilder::new(None, 0);
        b.grid(2, |g| {
            g.paint_all(ItemStack::empty());
            g.place(Rect::new(0, 0, 2, 1), container(2));
            g.place(Rect::new(2, 0, 2, 1), container(2));
        });
        assert_eq!(b.slot_count(), 18);
    }

    #[test]
    fn result_slot_lands_on_its_cell() {
        use crate::inventory::container::ResultContainer;
        use crate::inventory::lock::ContainerLockGuard;
        use crate::player::Player;

        struct NoopHandler;
        impl ResultHandler for NoopHandler {
            fn update_result(&self, _guard: &mut ContainerLockGuard) {}
            fn on_result_taken(
                &self,
                _guard: &mut ContainerLockGuard,
                _player: &Player,
            ) -> Option<ItemStack> {
                None
            }
            fn is_result_valid(&self, _guard: &ContainerLockGuard, _player: &Player) -> bool {
                true
            }
        }

        let mut b = MenuBuilder::new(None, 0);
        let result = b.grid(3, |g| {
            let result = g.place_result(
                Rect::cell(6, 2),
                Arc::new(NoopHandler),
                ResultContainer::new().into_shared(),
            );
            g.paint_all(ItemStack::empty());
            result
        });
        assert_eq!((result.start(), result.end()), (24, 25));
    }

    #[test]
    #[should_panic(expected = "overlaps another placement")]
    fn overlapping_placements_panic() {
        let mut b = MenuBuilder::new(None, 0);
        b.grid(1, |g| {
            g.place(Rect::new(0, 0, 5, 1), container(5));
            g.place(Rect::new(4, 0, 5, 1), container(5));
        });
    }

    #[test]
    #[should_panic(expected = "exceeds the 9x1 grid area")]
    fn out_of_bounds_placement_panics() {
        let mut b = MenuBuilder::new(None, 0);
        b.grid(1, |g| {
            g.place(Rect::new(5, 0, 5, 1), container(5));
        });
    }

    #[test]
    #[should_panic(expected = "not fully covered")]
    fn uncovered_cells_panic() {
        let mut b = MenuBuilder::new(None, 0);
        b.grid(1, |g| {
            g.place(Rect::new(0, 0, 4, 1), container(4));
        });
    }

    #[test]
    #[should_panic(expected = "not fully covered")]
    fn subgrid_must_cover_itself_despite_parent_paint() {
        let mut b = MenuBuilder::new(None, 0);
        b.grid(2, |g| {
            g.paint_all(ItemStack::empty());
            g.subgrid(Rect::new(0, 0, 4, 1), |g| {
                g.place(Rect::new(0, 0, 2, 1), container(2));
            });
        });
    }

    #[test]
    #[should_panic(expected = "cannot mix rows() and cols()")]
    fn mixing_carve_axes_panics() {
        let mut b = MenuBuilder::new(None, 0);
        b.grid(2, |g| {
            g.rows(1, |g| g.place(g.full(), container(9)));
            g.cols(4, |g| g.place(g.full(), container(4)));
        });
    }

    #[test]
    #[should_panic(expected = "grid starts mid-row")]
    fn grid_after_partial_row_panics() {
        let mut b = MenuBuilder::new(None, 0);
        b.section(container(5), 5);
        b.grid(1, |g| {
            g.paint_all(ItemStack::empty());
        });
    }

    #[test]
    #[should_panic(expected = "non-contiguous")]
    fn single_panics_on_multi_row_narrow_region() {
        let mut b = MenuBuilder::new(None, 0);
        b.grid(2, |g| {
            let region = g.place(Rect::new(0, 0, 4, 2), container(8));
            g.paint_all(ItemStack::empty());
            let _ = region.single();
        });
    }
}
