//! A declarative builder for assembling [`MenuBehavior`]s.
//!
//! Concrete menus (chest, anvil, crafting, …) all repeat the same four chunks:
//! build a `Vec<SlotType>` by hand, maintain a parallel list of `ContainerRef`s
//! that must match exactly what the slots touch, hand-count slot index ranges in
//! a `mod slots`, and write `quick_move_stack` as range arithmetic over those
//! indices.
//!
//! [`MenuBuilder`] collapses all of that into one place:
//!
//! - Slots are added in **sections**; each call hands back a [`Section`]
//!   handle (a cheap `Copy` range) so you never hand-count indices again.
//! - The set of containers to lock is **derived** from the sections, so it can
//!   never drift out of sync with the slots.
//! - [`data_slot`](MenuBuilder::data_slot) returns a typed [`DataSlot`] handle,
//!   replacing "remember that data slot 0 is the level cost".
//! - Shift-click behavior is described declaratively with
//!   [`route`](MenuBuilder::route); the resulting route table drives a generic
//!   quick-move (`MenuLayout::quick_move`) that does what every hand-written
//!   `quick_move_stack` currently does.
//!
//! ```ignore
//! let mut builder = MenuBuilder::new(&vanilla_menu_types::ANVIL, container_id);
//! let inputs = builder.section(input_container, 2);
//! let result = builder.result_slot(anvil_handler, result_container);
//! let player = builder.player_inventory(&inventory);
//! let level_cost = builder.data_slot(0);
//!
//! builder.route(result, [player.all], FillDirection::Backward);
//! builder.route(inputs, [player.all], FillDirection::Forward);
//! builder.route(player.main, [inputs, player.hotbar], FillDirection::Forward);
//! builder.route(player.hotbar, [inputs, player.main], FillDirection::Forward);
//! builder.drain([inputs]);
//!
//! let menu = builder.build(AnvilKind { /* per-menu state */ });
//! ```

use std::range::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use steel_registry::{item_stack::ItemStack, menu_type::MenuTypeRef};
use steel_utils::locks::{Shared, SyncMutex};

use crate::inventory::container::SimpleContainer;
use crate::inventory::menu::Menu;
use crate::inventory::menu::behavior::MenuBehavior;
use crate::inventory::menu::kind::MenuKindType;
use crate::inventory::menu::layout::MenuLayout;
use crate::inventory::{
    lock::{ContainerLockGuard, ContainerRef},
    slots::{
        MayPickupFn, MayPlaceFn, NormalSlot, RestrictedSlot, ResultHandler, ResultSlot, SlotType,
        add_standard_inventory_slots,
    },
};
use crate::player::Player;
use crate::player::player_inventory::PlayerInventory;

/// Identity of one built menu.
///
/// Stamped onto every [`Section`] and [`DataSlot`] a [`MenuBuilder`] mints, so
/// a handle can never silently act on a different menu's slots: the consuming
/// sites ([`MenuBuilder::route`], [`MenuBuilder::drain`], [`DataSlot::get`] /
/// [`DataSlot::set`]) verify the stamp in debug builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MenuInstanceId(u64);

impl MenuInstanceId {
    /// Mints a process-unique id (one per menu built, not per click).
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// A handle to a contiguous range of slots added to a [`MenuBuilder`].
///
/// Sections are `Copy` and carry only a `start..end` range plus the identity
/// of the menu that minted them, so passing them around (e.g. into
/// [`MenuBuilder::route`]) is free. Two sections covering the same range in
/// different menus compare unequal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    menu: MenuInstanceId,
    start: usize,
    end: usize,
}

impl Section {
    /// Creates a section over an explicit slot range, stamped with the menu it
    /// belongs to.
    ///
    /// Crate-internal: sections are only meaningful as handles minted by a
    /// [`MenuBuilder`] over its own slots. Restricting construction keeps a
    /// fabricated out-of-range section from reaching [`MenuBuilder::route`] /
    /// [`MenuBuilder::drain`] and panicking during click handling.
    #[must_use]
    pub(crate) fn new(menu: MenuInstanceId, range: impl Into<Range<usize>>) -> Self {
        let range = range.into();
        Self {
            menu,
            start: range.start,
            end: range.end,
        }
    }

    /// The index of the first slot in this section.
    #[must_use]
    pub const fn start(self) -> usize {
        self.start
    }

    /// The index one past the last slot in this section.
    #[must_use]
    pub const fn end(self) -> usize {
        self.end
    }

    /// The number of slots in this section.
    #[must_use]
    pub const fn len(self) -> usize {
        self.end - self.start
    }

    /// Returns `true` if this section contains no slots.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Returns `true` if `slot_index` falls within this section.
    #[must_use]
    pub const fn contains(self, slot_index: usize) -> bool {
        slot_index >= self.start && slot_index < self.end
    }

    /// The section as a `Range`, suitable for indexing and iteration.
    #[must_use]
    pub fn range(self) -> Range<usize> {
        Range::from(self.start..self.end)
    }
}

/// The sections produced by [`MenuBuilder::player_inventory`].
///
/// Vanilla shift-click routing treats the main inventory and the hotbar as
/// separate targets (filling one before falling back to the other), so both are
/// exposed alongside the combined [`all`](PlayerInventorySections::all) range.
#[derive(Clone, Copy, Debug)]
pub struct PlayerInventorySections {
    /// All 36 player slots (main inventory followed by hotbar).
    all: Section,
    /// The 27 main inventory slots.
    main: Section,
    /// The 9 hotbar slots.
    hotbar: Section,
}

impl PlayerInventorySections {
    /// All 36 player slots (main inventory followed by hotbar).
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

/// A typed handle to a data slot (furnace progress, anvil level cost, …).
///
/// Obtained from [`MenuBuilder::data_slot`]; read and write it through the menu
/// behavior instead of remembering a bare index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataSlot {
    menu: MenuInstanceId,
    index: usize,
}

impl DataSlot {
    /// Reads the current value of this data slot.
    ///
    /// # Panics
    /// In debug builds, panics if `behavior` belongs to a different menu than
    /// the [`MenuBuilder`] that minted this handle.
    #[must_use]
    pub fn get(self, behavior: &MenuBehavior) -> i16 {
        debug_assert_eq!(
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
    /// In debug builds, panics if `behavior` belongs to a different menu than
    /// the [`MenuBuilder`] that minted this handle.
    pub fn set(self, behavior: &mut MenuBehavior, value: i16) {
        debug_assert_eq!(
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

/// The direction in which a slot range is walked when distributing items.
///
/// Vanilla fills backwards when moving into the player inventory so existing
/// hotbar stacks top up first; the same enum steers double-click pickup-all
/// collection order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillDirection {
    /// Walk from the first slot of the range to the last.
    Forward,
    /// Walk from the last slot of the range to the first.
    Backward,
}

/// A declarative shift-click route: take from `from`, then try each target
/// range in order, stopping at the first that accepts anything.
pub(crate) struct Route {
    pub(crate) from: Range<usize>,
    pub(crate) targets: Vec<Range<usize>>,
    pub(crate) direction: FillDirection,
}

/// Builds the slots, containers, data slots and routing for a menu.
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
}

impl MenuBuilder {
    /// Creates a new builder for a menu of the given type and container id.
    ///
    /// Pass `None` for the player's own inventory menu, or a menu type
    /// (e.g. `&vanilla_menu_types::ANVIL`) for any other window.
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
        }
    }

    /// Adds `count` plain slots backed by `container` at indices `0..count`.
    ///
    /// Returns a [`Section`] handle over the slots that were added.
    pub fn section(&mut self, container: impl Into<ContainerRef>, count: usize) -> Section {
        let container = container.into();
        let start = self.slots.len();
        for index in 0..count {
            self.slots
                .push(SlotType::Normal(NormalSlot::new(container.clone(), index)));
        }
        self.register_container(container);
        self.section_from(start)
    }

    /// Adds a restricted section to the `Menu`. The closures `may_place` and `may_pickup`
    /// are shared using an Arc across all slots in the section.
    ///
    /// # Example
    /// ```rust
    /// use steel_registry::vanilla_items;
    /// use steel_registry::item_stack::ItemStack;
    /// use steel_registry::vanilla_menu_types;
    /// use steel_core::inventory::prelude::*;
    /// use steel_core::inventory::container::SimpleContainer;
    /// use steel_core::inventory::menu::kinds::BasicKind;
    /// use steel_core::player::Player;
    /// use steel_utils::locks::SyncMutex;
    /// use std::sync::Arc;
    ///
    /// let container_id = 0;
    ///
    /// let mut b = MenuBuilder::new(&vanilla_menu_types::GENERIC_9X1, container_id);
    ///
    /// let items = vec![ItemStack::new(&vanilla_items::ITEMS.gray_stained_glass_pane); 9];
    ///
    /// let container = SimpleContainer::from_items(items).into_shared();
    /// let display_section = b.restricted_section(container.clone(), 9, |_item_stack| true, Some(|_: &ContainerLockGuard, _: &Player, _: &ItemStack| false));
    ///
    /// b.build(MenuKindType::Basic(BasicKind {}));
    /// ```
    pub fn restricted_section(
        &mut self,
        container: impl Into<ContainerRef>,
        count: usize,
        may_place: impl Fn(&ItemStack) -> bool + Send + Sync + 'static,
        may_pickup: Option<
            impl Fn(&ContainerLockGuard, &Player, &ItemStack) -> bool + Send + Sync + 'static,
        >,
    ) -> Section {
        let container = container.into();
        let start = self.slots.len();
        let may_place: MayPlaceFn = Arc::new(may_place);
        let may_pickup = may_pickup.map(|it| -> MayPickupFn { Arc::new(it) });
        for index in 0..count {
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
    /// This is equivalent to a restricted section with both closures always returning false.
    ///
    /// # Example
    /// ```rust
    /// use steel_registry::vanilla_items;
    /// use steel_registry::item_stack::ItemStack;
    /// use steel_registry::vanilla_menu_types;
    /// use steel_core::inventory::prelude::*;
    /// use steel_core::inventory::menu::kinds::BasicKind;
    ///
    /// let container_id = 0;
    ///
    /// let mut b = MenuBuilder::new(&vanilla_menu_types::GENERIC_9X1, container_id);;
    /// let items = vec![ItemStack::new(&vanilla_items::ITEMS.gray_stained_glass_pane); 9];
    /// let display_section = b.display_section(items);
    ///
    /// b.build(MenuKindType::Basic(BasicKind {}));
    /// ```
    pub fn display_section(&mut self, items: Vec<ItemStack>) -> (Section, Shared<SimpleContainer>) {
        let count = items.len();
        let container = Arc::new(SyncMutex::new(SimpleContainer::from_items(items)));

        let may_place: MayPlaceFn = Arc::new(|_| false);
        let may_pickup: Option<MayPickupFn> = Some(Arc::new(
            |_: &ContainerLockGuard, _: &Player, _: &ItemStack| false,
        ));

        let start = self.slots.len();
        for index in 0..count {
            self.slots.push(SlotType::Restricted(RestrictedSlot::new(
                container.clone(),
                index,
                may_place.clone(),
                may_pickup.clone(),
                64,
            )));
        }

        self.register_container(container.clone());
        (self.section_from(start), container)
    }

    /// Adds the player's 36 inventory slots (main inventory then hotbar).
    ///
    /// Returns handles to the combined range as well as the main inventory and
    /// hotbar sub-ranges, which routing usually needs separately.
    pub fn player_inventory(
        &mut self,
        inventory: &Shared<PlayerInventory>,
    ) -> PlayerInventorySections {
        let start = self.slots.len();
        add_standard_inventory_slots(&mut self.slots, inventory);
        self.register_container(ContainerRef::PlayerInventory(inventory.clone()));

        let main = Section::new(self.instance, start..start + 27);
        let hotbar = Section::new(self.instance, start + 27..self.slots.len());
        let all = Section::new(self.instance, start..self.slots.len());
        PlayerInventorySections { all, main, hotbar }
    }

    /// Adds a single fake result slot driven by `handler`.
    ///
    /// The result is read from / written to `container` (typically a
    /// [`ResultContainer`](crate::inventory::crafting::ResultContainer)).
    pub fn result_slot(
        &mut self,
        handler: Arc<dyn ResultHandler + Send + Sync>,
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

    /// Adds arbitrary pre-built slots as a named section.
    ///
    /// Use this for slot kinds the convenience methods don't cover (armor,
    /// restricted, plugin-defined custom slots). `containers` must list every
    /// container the slots touch so it can be locked.
    pub fn custom_section(
        &mut self,
        slots: impl IntoIterator<Item = SlotType>,
        containers: impl IntoIterator<Item = ContainerRef>,
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

    /// Declares a shift-click route from one section into others.
    ///
    /// When a slot in `from` is shift-clicked, the generic quick-move walks
    /// `targets` in order and stops at the first that accepts any items. Pass
    /// [`FillDirection::Backward`] to fill the targets from the end (vanilla
    /// does this when moving into the player inventory so existing stacks top
    /// up first).
    ///
    /// # Panics
    /// In debug builds, panics if any section was minted by a different
    /// [`MenuBuilder`].
    pub fn route(
        &mut self,
        from: Section,
        targets: impl IntoIterator<Item = Section>,
        direction: FillDirection,
    ) -> &mut Self {
        let from = self.owned(from);
        let targets = targets.into_iter().map(|s| self.owned(s)).collect();
        self.routes.push(Route {
            from,
            targets,
            direction,
        });
        self
    }

    /// Marks `sections` to be emptied back into the player on close.
    ///
    /// Input grids (crafting, anvil, …) must not swallow items when the window
    /// closes; on close, `MenuLayout::return_drained_items` returns the listed slots'
    /// contents to the player's inventory (dropping overflow). Only list real
    /// input sections — fake/result slots are computed and would dupe items.
    ///
    /// # Panics
    /// In debug builds, panics if any section was minted by a different
    /// [`MenuBuilder`].
    ///
    /// # Example
    /// ```rust
    /// use std::sync::Arc;
    ///
    /// use steel_registry::{item_stack::ItemStack, vanilla_items, vanilla_menu_types};
    /// use steel_utils::locks::SyncMutex;
    ///
    /// use steel_core::inventory::prelude::*;
    /// use steel_core::inventory::menu::kinds::BasicKind;
    /// use steel_core::inventory::container::SimpleContainer;
    ///
    /// let container_id = 0;
    ///
    /// let mut b = MenuBuilder::new(&vanilla_menu_types::GENERIC_9X2, container_id);
    ///
    /// let items = vec![ItemStack::empty(); 9];
    /// let upper_container = SimpleContainer::from_items(items).into_shared();
    /// let upper_container_ref = ContainerRef::SimpleContainer(upper_container);
    ///
    /// let items = vec![ItemStack::new(&vanilla_items::ITEMS.barrier); 9];
    ///
    /// let restricted_section = b.display_section(items);
    ///
    /// let section = b.section(upper_container_ref, 9);
    /// b.drain([section]); // only section gets drained when the menu is closed
    /// b.build(MenuKindType::Basic(BasicKind {}));
    /// ```
    pub fn drain(&mut self, sections: impl IntoIterator<Item = Section>) -> &mut Self {
        let ranges: Vec<_> = sections.into_iter().map(|s| self.owned(s)).collect();
        self.drain_sections.extend(ranges);
        self
    }

    /// Consumes the builder, assembling the finished [`Menu`] around its
    /// per-menu [`MenuKind`](crate::inventory::MenuKind).
    #[must_use]
    pub fn build(self, kind: impl Into<MenuKindType>) -> Menu {
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

    /// Records a container to lock, ignoring it if an equal one is already present.
    fn register_container(&mut self, container: impl Into<ContainerRef>) {
        let container_ref = container.into();
        let id = container_ref.container_id();
        if !self.container_refs.iter().any(|c| c.container_id() == id) {
            self.container_refs.push(container_ref);
        }
    }

    /// Verifies (in debug builds) that `section` was minted by this builder,
    /// returning its raw slot range.
    fn owned(&self, section: Section) -> Range<usize> {
        debug_assert_eq!(
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
