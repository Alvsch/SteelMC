//! A declarative builder for assembling [`MenuBehavior`]s.
//!
//! ```rust
//! use steel_registry::{vanilla_items, vanilla_menu_types};
//! use steel_core::{inventory::menu::kinds::BasicKind, player::player_inventory::PlayerInventory};
//!
//! use steel_core::inventory::prelude::*;
//!
//! fn example(container_id: u8, inventory: Shared<PlayerInventory>) -> Menu {
//!     let mut builder = MenuBuilder::new(&vanilla_menu_types::GENERIC_9X1, container_id);
//!
//!     let items = vec![ItemStack::new(&vanilla_items::FLINT_AND_STEEL); 9];
//!     let container = SimpleContainer::from_items(items).into_shared();
//!
//!     let section = builder.section(container, 9);
//!
//!     let player = builder.player_inventory(&inventory);
//!     let level_cost = builder.data_slot(0);
//!
//!     builder.route(section, [player.all()], FillDirection::Backward);
//!     builder.route(player.all(), [section], FillDirection::Forward);
//!
//!     builder.build(MenuKindType::Basic(BasicKind {}))
//! }
//! ```

use std::array::IntoIter;
use std::fmt;
use std::iter;
use std::range::Range;
use std::slice;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::vec;

use steel_registry::{item_stack::ItemStack, menu_type::MenuTypeRef};
use steel_utils::locks::Shared;

use crate::inventory::menu::Menu;
use crate::inventory::menu::behavior::MenuBehavior;
use crate::inventory::menu::kind::MenuKindType;
use crate::inventory::menu::layout::MenuLayout;
use crate::inventory::{
    lock::{ContainerId, ContainerLockGuard, ContainerRef},
    slots::{
        MayPickupFn, MayPlaceFn, NormalSlot, RestrictedSlot, ResultHandler, ResultSlot, SlotType,
        add_standard_inventory_slots,
    },
};
use crate::player::Player;
use crate::player::player_inventory::PlayerInventory;

/// Identity of one built menu.
///
/// Given to every [`Section`] and [`DataSlot`] a [`MenuBuilder`] creates, so
/// a handle can never act on a [`Menu`] it wasn't made for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MenuInstanceId(u64);

impl MenuInstanceId {
    /// Creates a new unique `MenuInstanceId`
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// A handle to a contiguous range of slots added to a [`MenuBuilder`].
///
/// Sections contain the id of the [`Menu`] they were made for and can only be
/// created by a builder. Two Sections cannot cover the same range for the same
/// [`Menu`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    menu: MenuInstanceId,
    range: Range<usize>,
}

impl Section {
    pub(crate) fn new(menu: MenuInstanceId, range: impl Into<Range<usize>>) -> Self {
        Self {
            menu,
            range: range.into(),
        }
    }

    /// The start of the section.
    #[must_use]
    pub const fn start(self) -> usize {
        self.range.start
    }

    /// The end of the section.
    #[must_use]
    pub const fn end(self) -> usize {
        self.range.end
    }

    /// The length of the section.
    #[must_use]
    pub const fn len(self) -> usize {
        self.range.end - self.range.start
    }

    /// Whether the section is empty (start == end).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.range.start == self.range.end
    }

    /// Whether the section contains an index.
    #[must_use]
    pub const fn contains(self, slot_index: usize) -> bool {
        slot_index >= self.range.start && slot_index < self.range.end
    }

    /// A copy of the internal range.
    #[must_use]
    pub const fn range(self) -> Range<usize> {
        self.range
    }
}

/// Converts different types into an Iterator of sections so they can be passed into `MenuBuilder::route`
pub trait IntoSections {
    /// The Iterator over the Section(s).
    type Iter: Iterator<Item = Section>;

    /// Converts self into the Iterator.
    fn into_sections(self) -> Self::Iter;
}

impl IntoSections for Section {
    type Iter = iter::Once<Section>;

    fn into_sections(self) -> Self::Iter {
        iter::once(self)
    }
}

impl<const N: usize> IntoSections for [Section; N] {
    type Iter = IntoIter<Section, N>;

    fn into_sections(self) -> Self::Iter {
        self.into_iter()
    }
}

impl<'a> IntoSections for &'a [Section] {
    type Iter = iter::Copied<slice::Iter<'a, Section>>;

    fn into_sections(self) -> Self::Iter {
        self.iter().copied()
    }
}

impl IntoSections for Vec<Section> {
    type Iter = vec::IntoIter<Section>;

    fn into_sections(self) -> Self::Iter {
        self.into_iter()
    }
}

/// The sections that cover the player's inventory.
///
/// Exclusively produced by [`MenuBuilder::player_inventory`].
#[derive(Clone, Copy, Debug)]
pub struct PlayerInventorySections {
    /// All 36 player slots (main and hotbar).
    all: Section,
    /// The 27 main inventory slots.
    main: Section,
    /// The 9 hotbar slots.
    hotbar: Section,
}

impl PlayerInventorySections {
    /// All 36 player slots (main and hotbar).
    #[must_use]
    pub const fn all(&self) -> Section {
        self.all
    }

    /// The 27 main inventory slots.
    #[must_use]
    pub const fn main(&self) -> Section {
        self.main
    }

    /// The 9 hotbar slots.
    #[must_use]
    pub const fn hotbar(&self) -> Section {
        self.hotbar
    }
}

/// A data slot handle created by the [`MenuBuilder::data_slot`], to use for easy access
/// instead of a bare index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataSlot {
    menu: MenuInstanceId,
    index: usize,
}

impl DataSlot {
    /// Reads the current value of this data slot.
    ///
    /// # Panics
    /// Panics if `behavior` belongs to a different menu than the
    /// [`MenuBuilder`] that minted this handle.
    #[must_use]
    pub fn get(self, behavior: &MenuBehavior) -> i16 {
        assert_eq!(
            self.menu,
            behavior.instance(),
            "DataSlot used with a MenuBehavior it does not belong to"
        );
        behavior
            .get_data(self.index)
            .expect("DataSlot index is always valid for its own menu")
    }

    /// Writes a new value to this data slot.
    ///
    /// # Panics
    /// Panics if `behavior` belongs to a different menu than the
    /// [`MenuBuilder`] that minted this handle.
    pub fn set(self, behavior: &mut MenuBehavior, value: i16) {
        assert_eq!(
            self.menu,
            behavior.instance(),
            "DataSlot used with a MenuBehavior it does not belong to"
        );
        behavior.set_data(self.index, value);
    }

    /// The raw data slot index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }
}

/// The not-yet-carved slots of a container being split across multiple
/// sections.
///
/// Created only by [`MenuBuilder::split`]. Every section created from this handle
/// consumes the next `count` container slots.
pub struct ContainerSlots {
    /// The container being split.
    container: ContainerRef,
    /// The next container slot not yet covered by a section.
    next: usize,
    /// The container's size when [`MenuBuilder::split`] was called, used to
    /// catch sections that take more slots than the container has.
    size: usize,
}

impl fmt::Debug for ContainerSlots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContainerSlots")
            .field("next", &self.next)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// A supplier for ranges of slots in a [`ContainerRef`]. Allowing either making
/// the whole Container a [`Section`] or splitting one Container into multiple Sections.
pub trait SectionSource {
    /// Consumes the next `count` slots of the slot range.
    fn take(self, count: usize) -> (ContainerRef, Range<usize>);
}

impl<T: Into<ContainerRef>> SectionSource for T {
    fn take(self, count: usize) -> (ContainerRef, Range<usize>) {
        (self.into(), (0..count).into())
    }
}

impl SectionSource for &mut ContainerSlots {
    /// # Panics
    /// Panics if taking `count` slots overflows the actual size of the container.
    fn take(self, count: usize) -> (ContainerRef, Range<usize>) {
        let start = self.next;
        assert!(
            start + count <= self.size,
            "section takes container slots {}..{}, but the container only has {} slots",
            start,
            start + count,
            self.size
        );
        self.next = start + count;
        (self.container.clone(), (start..start + count).into())
    }
}

/// The direction in which a slot range is walked when distributing items.
///
/// Vanilla fills backwards when moving into the player inventory so existing
/// hotbar stacks top up first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillDirection {
    /// Walk from the first slot of the range to the last.
    Forward,
    /// Walk from the last slot of the range to the first.
    Backward,
}

/// A shift clicking Route that goes from a single Range to a Vec of Ranges.
pub(crate) struct Route {
    pub(crate) from: Range<usize>,
    pub(crate) targets: Vec<Range<usize>>,
    pub(crate) direction: FillDirection,
}

/// Builds a Menu.
///
/// See the [module documentation](self) for an overview.
pub struct MenuBuilder {
    instance: MenuInstanceId,
    menu_type: Option<MenuTypeRef>,
    container_id: u8,
    slots: Vec<SlotType>,
    container_refs: Vec<ContainerRef>,
    data_slots: Vec<i16>,
    routes: Vec<Route>,
    drain_sections: Vec<Range<usize>>,
    /// Container-local slot ranges already covered by a section, used to catch
    /// two sections mapping onto the same container slots.
    claimed: Vec<(ContainerId, Range<usize>)>,
}

impl fmt::Debug for MenuBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MenuBuilder")
            .field("instance", &self.instance)
            .field("container_id", &self.container_id)
            .field("slots", &self.slots.len())
            .field("routes", &self.routes.len())
            .finish_non_exhaustive()
    }
}

impl MenuBuilder {
    /// Creates a new builder for a menu of the given type and container id.
    ///
    /// Pass `None` for the player's own inventory menu, or a menu type
    /// (`&vanilla_menu_types::ANVIL`, ...).
    #[must_use]
    pub fn new(menu_type: impl Into<Option<MenuTypeRef>>, container_id: u8) -> Self {
        Self {
            instance: MenuInstanceId::next(),
            menu_type: menu_type.into(),
            container_id,
            slots: Vec::new(),
            container_refs: Vec::new(),
            data_slots: Vec::new(),
            routes: Vec::new(),
            drain_sections: Vec::new(),
            claimed: Vec::new(),
        }
    }

    /// Starts splitting a `Container` into multiple sections.
    ///
    /// Use this when you are locked to storing items in one `Container`
    /// and need to split them into different [Section]s.
    ///
    /// # Example
    /// ```rust
    /// use steel_core::inventory::prelude::*;
    /// use steel_core::inventory::menu::kinds::BasicKind;
    ///
    /// let mut b = MenuBuilder::new(None, 0);
    ///
    /// let mut stand = b.split(SimpleContainer::new(5).into_shared());
    /// let bottles = b.section(&mut stand, 3); // slots 0..3
    /// let ingredient = b.section(&mut stand, 1); // slot 3
    /// let fuel = b.section(&mut stand, 1); // slot 4
    ///
    /// b.build(MenuKindType::Basic(BasicKind {}));
    /// ```
    ///
    /// # Panics
    /// Panics if the sections carved from the returned handle take more slots
    /// than the container has.
    #[must_use]
    pub fn split(&mut self, container: impl Into<ContainerRef>) -> ContainerSlots {
        let container = container.into();
        let size = ContainerLockGuard::lock_all(slice::from_ref(&container))
            .get(container.container_id())
            .expect("container was just locked")
            .get_container_size();
        self.register_container(container.clone());
        ContainerSlots {
            container,
            next: 0,
            size,
        }
    }

    /// Adds `count` plain slots backed by `source`.
    ///
    /// Pass a container directly to cover its slots `0..count`, or a
    /// [`ContainerSlots`] handle from [`MenuBuilder::split`] to cover the next
    /// `count` slots of a container shared between several sections.
    ///
    /// Returns a [`Section`] handle over the slots that were added.
    ///
    /// # Panics
    /// Panics if the covered container slots overlap another section of this
    /// menu.
    pub fn section(&mut self, source: impl SectionSource, count: usize) -> Section {
        let (container, range) = source.take(count);
        self.claim(&container, range);
        let start = self.slots.len();
        for index in range {
            self.slots
                .push(SlotType::Normal(NormalSlot::new(container.clone(), index)));
        }
        self.register_container(container);
        self.section_from(start)
    }

    /// Adds a section whose slots only accept items that pass `may_place`.
    /// The closure is shared using an Arc across all slots in the section and
    /// receives the container-local slot index.
    ///
    /// Items can always be taken back out; use
    /// [`guarded_section`](Self::guarded_section) to also guard pickup.
    ///
    /// # Example
    /// ```rust
    /// use steel_registry::vanilla_items;
    /// use steel_core::inventory::prelude::*;
    /// use steel_core::inventory::menu::kinds::BasicKind;
    ///
    /// let mut b = MenuBuilder::new(None, 0);
    ///
    /// let container = SimpleContainer::new(9).into_shared();
    /// let fuel = b.restricted_section(container, 9, |_slot, stack| {
    ///     stack.is(&vanilla_items::COAL)
    /// });
    ///
    /// b.build(MenuKindType::Basic(BasicKind {}));
    /// ```
    pub fn restricted_section(
        &mut self,
        source: impl SectionSource,
        count: usize,
        may_place: impl Fn(usize, &ItemStack) -> bool + Send + Sync + 'static,
    ) -> Section {
        self.guarded_section_fns(source, count, Arc::new(may_place), None)
    }

    /// Like [`restricted_section`](Self::restricted_section), but also guards
    /// taking items out: pickup is only allowed while `may_pickup` returns
    /// true. Both closures are shared across all slots in the section.
    pub fn guarded_section(
        &mut self,
        source: impl SectionSource,
        count: usize,
        may_place: impl Fn(usize, &ItemStack) -> bool + Send + Sync + 'static,
        may_pickup: impl Fn(usize, &ContainerLockGuard, &Player, &ItemStack) -> bool
        + Send
        + Sync
        + 'static,
    ) -> Section {
        self.guarded_section_fns(
            source,
            count,
            Arc::new(may_place),
            Some(Arc::new(may_pickup)),
        )
    }

    /// Shared lowering of restricted, guarded and display sections.
    fn guarded_section_fns(
        &mut self,
        source: impl SectionSource,
        count: usize,
        may_place: MayPlaceFn,
        may_pickup: Option<MayPickupFn>,
    ) -> Section {
        let (container, range) = source.take(count);
        self.claim(&container, range);
        let start = self.slots.len();
        for index in range {
            self.slots.push(SlotType::Restricted(RestrictedSlot::new(
                container.clone(),
                index,
                may_place.clone(),
                may_pickup.clone(),
                64,
            )));
        }
        self.register_container(container);
        self.section_from(start)
    }

    /// Adds a display section containing the specified items. No items can be placed or taken out of these slots,
    /// making it ideal for click menus. Clicks on these slots are always rejected, and can then properly be handled
    /// in the `MenuKind::on_slot_clicked`.
    ///
    /// This is equivalent to a guarded section with both closures always returning false.
    ///
    /// # Example
    /// ```rust
    /// use steel_registry::vanilla_items;
    /// use steel_registry::item_stack::ItemStack;
    /// use steel_core::inventory::prelude::*;
    /// use steel_core::inventory::menu::kinds::BasicKind;
    ///
    /// let container_id = 0;
    ///
    /// let mut b = MenuBuilder::new(None, container_id);
    /// let items = vec![ItemStack::new(&vanilla_items::GRAY_STAINED_GLASS_PANE); 9];
    /// let container = SimpleContainer::from_items(items).into_shared();
    /// let display_section = b.display_section(container, 9);
    ///
    /// b.build(MenuKindType::Basic(BasicKind {}));
    /// ```
    pub fn display_section(&mut self, source: impl SectionSource, count: usize) -> Section {
        self.guarded_section_fns(
            source,
            count,
            Arc::new(|_, _| false),
            Some(Arc::new(|_, _, _, _| false)),
        )
    }

    /// Adds the player's 36 inventory slots (main inventory then hotbar).
    pub fn player_inventory(
        &mut self,
        inventory: &Shared<PlayerInventory>,
    ) -> PlayerInventorySections {
        let start = self.slots.len();
        add_standard_inventory_slots(&mut self.slots, inventory);
        self.register_container(ContainerRef::from(inventory.clone()));

        let main = Section::new(self.instance, start..start + 27);
        let hotbar = Section::new(self.instance, start + 27..self.slots.len());
        let all = Section::new(self.instance, start..self.slots.len());
        PlayerInventorySections { all, main, hotbar }
    }

    /// Adds a single fake result slot driven by `handler`.
    ///
    /// See [`crate::inventory::container::ResultContainer`] and [`crate::inventory::slots::ResultHandler`].
    pub fn result_slot(
        &mut self,
        handler: impl ResultHandler + 'static,
        container: impl Into<ContainerRef>,
    ) -> Section {
        let container = container.into();
        let start = self.slots.len();
        self.slots.push(SlotType::Result(ResultSlot::new(
            handler,
            container.clone(),
        )));
        self.register_container(container);
        self.section_from(start)
    }

    /// Adds arbitrary or custom slots.
    pub fn custom_section(
        &mut self,
        slots: impl IntoIterator<Item = SlotType>,
        containers: impl IntoIterator<Item = impl Into<ContainerRef>>,
    ) -> Section {
        let start = self.slots.len();
        self.slots.extend(slots);
        for container in containers {
            self.register_container(container);
        }
        self.section_from(start)
    }

    /// Adds a data slot with an initial value and returns a typed handle to it.
    pub fn data_slot(&mut self, initial: i16) -> DataSlot {
        let index = self.data_slots.len();
        self.data_slots.push(initial);
        DataSlot {
            menu: self.instance,
            index,
        }
    }

    /// Declares a shift-click route from each section of `from` into
    /// `targets`.
    ///
    /// Both arguments accept anything [`IntoSections`]; a multi-section
    /// `from` declares one route per source section.
    ///
    /// Most commonly:
    /// `player_inventory` -> `container` is [`FillDirection::Forward`]
    /// `container` -> `player_inventory` is [`FillDirection::Backward`]
    ///
    /// # Panics
    /// Panics if any section was created by a different [`MenuBuilder`].
    pub fn route(
        &mut self,
        from: impl IntoSections,
        targets: impl IntoSections,
        direction: FillDirection,
    ) -> &mut Self {
        let targets: Vec<Range<usize>> = targets.into_sections().map(|s| self.owned(s)).collect();
        for from in from.into_sections() {
            let from = self.owned(from);
            assert!(
                !self
                    .routes
                    .iter()
                    .any(|route| route.from.start < from.end && from.start < route.from.end),
                "shift-click route source {from:?} overlaps an existing route source",
            );
            assert!(
                !targets
                    .iter()
                    .any(|t| t.start < from.end && from.start < t.end),
                "shift-click route target {targets:?} overlaps its own source {from:?}",
            );
            self.routes.push(Route {
                from,
                targets: targets.clone(),
                direction,
            });
        }
        self
    }

    /// Marks `sections` to be emptied back into the player or dropped on the floor on close.
    ///
    /// # Panics
    /// Panics if any section was created by a different [`MenuBuilder`].
    ///
    /// # Example
    /// ```rust
    /// use std::sync::Arc;
    ///
    /// use steel_registry::{item_stack::ItemStack, vanilla_items};
    /// use steel_utils::locks::SyncMutex;
    ///
    /// use steel_core::inventory::prelude::*;
    /// use steel_core::inventory::menu::kinds::BasicKind;
    /// use steel_core::inventory::container::SimpleContainer;
    ///
    /// let container_id = 0;
    ///
    /// let mut b = MenuBuilder::new(None, container_id);
    ///
    /// let items = vec![ItemStack::empty(); 9];
    /// let upper_container = SimpleContainer::from_items(items).into_shared();
    ///
    /// let items = vec![ItemStack::new(&vanilla_items::BARRIER); 9];
    /// let lower_container = SimpleContainer::from_items(items).into_shared();
    ///
    /// let restricted_section = b.display_section(lower_container, 9);
    ///
    /// let section = b.section(upper_container, 9);
    /// b.drain([section]); // only 'section' gets drained when the menu is closed
    /// b.build(MenuKindType::Basic(BasicKind {}));
    /// ```
    pub fn drain(&mut self, sections: impl IntoSections) -> &mut Self {
        let ranges: Vec<_> = sections.into_sections().map(|s| self.owned(s)).collect();
        self.drain_sections.extend(ranges);
        self
    }

    /// Consumes the builder, creating the finished [`Menu`].
    ///
    /// # Panics
    /// Panics if the number of slots does not match the client layout declared
    /// by the menu type.
    #[must_use]
    pub fn build(self, kind: impl Into<MenuKindType>) -> Menu {
        if let Some(menu_type) = self.menu_type {
            assert_eq!(
                self.slots.len(),
                menu_type.slot_count,
                "menu type {} expects {} slots, but the builder has {}",
                menu_type.key,
                menu_type.slot_count,
                self.slots.len(),
            );
        }

        let mut behavior = MenuBehavior::new(
            self.instance,
            self.slots,
            self.container_id,
            self.menu_type,
            self.container_refs,
        );
        for initial in self.data_slots {
            behavior.add_data_slot(initial);
        }

        let layout = MenuLayout {
            routes: self.routes,
            drain_sections: self.drain_sections,
        };
        Menu::from_parts(behavior, layout, kind.into())
    }

    /// The identity of the menu being built.
    pub(crate) const fn instance(&self) -> MenuInstanceId {
        self.instance
    }

    /// The number of slots added so far.
    pub(crate) const fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Appends a single slot without creating a section.
    pub(crate) fn push_slot(&mut self, slot: SlotType) {
        self.slots.push(slot);
    }

    /// Records that a section covers the container-local `range` of `container`.
    ///
    /// # Panics
    /// Panics if the range exceeds the container or was already covered by another range.
    pub(crate) fn claim(&mut self, container: &ContainerRef, range: Range<usize>) {
        let id = container.container_id();
        let size = {
            let guard = ContainerLockGuard::lock_all(slice::from_ref(container));
            let Some(container) = guard.get(id) else {
                panic!("container was not locked while validating a menu section");
            };
            container.get_container_size()
        };
        assert!(
            range.end <= size,
            "section takes container slots {}..{}, but the container only has {size} slots",
            range.start,
            range.end,
        );
        for (other_id, other) in &self.claimed {
            assert!(
                *other_id != id || range.start >= other.end || other.start >= range.end,
                "two sections cover overlapping slots ({other:?} and {range:?}) of the same \
                 container; carve shared containers with MenuBuilder::split"
            );
        }
        self.claimed.push((id, range));
    }

    /// Records a container to lock.
    pub(crate) fn register_container(&mut self, container: impl Into<ContainerRef>) {
        let container_ref = container.into();
        let id = container_ref.container_id();
        if !self.container_refs.iter().any(|c| c.container_id() == id) {
            self.container_refs.push(container_ref);
        }
    }

    /// Verifies that `section` was created by this builder.
    fn owned(&self, section: Section) -> Range<usize> {
        assert_eq!(
            section.menu, self.instance,
            "Section was minted by a different MenuBuilder"
        );
        section.range()
    }

    /// Returns a section spanning `start..self.slots.len()`.
    fn section_from(&self, start: usize) -> Section {
        Section::new(self.instance, start..self.slots.len())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Weak;

    use steel_registry::vanilla_menu_types;
    use steel_utils::locks::IntoShared;

    use super::*;
    use crate::inventory::container::SimpleContainer;
    use crate::inventory::menu::kinds::BasicKind;

    #[test]
    #[should_panic(
        expected = "menu type minecraft:generic_9x6 expects 90 slots, but the builder has 0"
    )]
    fn build_rejects_a_slot_count_that_disagrees_with_the_menu_type() {
        let _ = MenuBuilder::new(&vanilla_menu_types::GENERIC_9X6, 1)
            .build(MenuKindType::Basic(BasicKind {}));
    }

    #[test]
    #[should_panic(
        expected = "section takes container slots 0..2, but the container only has 1 slots"
    )]
    fn direct_section_rejects_a_range_past_container_capacity() {
        let mut builder = MenuBuilder::new(None, 0);
        builder.section(SimpleContainer::new(1).into_shared(), 2);
    }

    #[test]
    #[should_panic(expected = "shift-click route source 0..27 overlaps an existing route source")]
    fn route_rejects_overlapping_source_sections() {
        let inventory = PlayerInventory::new(Weak::new()).into_shared();
        let mut builder = MenuBuilder::new(None, 0);
        let player = builder.player_inventory(&inventory);
        let target = builder.section(SimpleContainer::new(1).into_shared(), 1);

        builder.route(player.all(), [target], FillDirection::Forward);
        builder.route(player.main(), [target], FillDirection::Forward);
    }
}
