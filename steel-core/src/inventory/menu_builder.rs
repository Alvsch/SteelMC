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
//!   [`route`](MenuBuilder::route); the resulting [`MenuLayout`] can drive a
//!   generic [`quick_move`](MenuLayout::quick_move) that does what every
//!   hand-written `quick_move_stack` currently does.
//!
//! ```ignore
//! let mut builder = MenuBuilder::new(&vanilla_menu_types::ANVIL, container_id);
//! let inputs = builder.section(input_container, 2);
//! let result = builder.result_slot(anvil_handler, result_container);
//! let player = builder.player_inventory(&inventory);
//! let level_cost = builder.data_slot(0);
//!
//! builder.route(result, [player.all], true);
//! builder.route(inputs, [player.all], false);
//! builder.route(player.main, [inputs, player.hotbar], false);
//! builder.route(player.hotbar, [inputs, player.main], false);
//! builder.drain([inputs]);
//!
//! let BuiltMenu { behavior, layout } = builder.build();
//! ```

use std::mem;
use std::ops::Range;
use std::sync::Arc;

use steel_registry::{item_stack::ItemStack, menu_type::MenuTypeRef};

use crate::inventory::{
    SyncPlayerInv,
    lock::{ContainerLockGuard, ContainerRef},
    menu::MenuBehavior,
    slots::{
        MayPickupFn, MayPlaceFn, NormalSlot, RestrictedSlot, ResultHandler, ResultSlot, Slot,
        SlotType, add_standard_inventory_slots,
    },
};
use crate::player::Player;

/// A handle to a contiguous range of slots added to a [`MenuBuilder`].
///
/// Sections are `Copy` and carry only a `start..end` range, so passing them
/// around (e.g. into [`MenuBuilder::route`]) is free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    start: usize,
    end: usize,
}

impl Section {
    /// Creates a section over an explicit slot range.
    #[must_use]
    pub const fn new(range: Range<usize>) -> Self {
        Self {
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
    pub const fn range(self) -> Range<usize> {
        self.start..self.end
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
    pub all: Section,
    /// The 27 main inventory slots.
    pub main: Section,
    /// The 9 hotbar slots.
    pub hotbar: Section,
}

/// A typed handle to a data slot (furnace progress, anvil level cost, …).
///
/// Obtained from [`MenuBuilder::data_slot`]; read and write it through the menu
/// behavior instead of remembering a bare index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataSlot {
    index: usize,
}

impl DataSlot {
    /// Reads the current value of this data slot (0 if it no longer exists).
    #[must_use]
    pub fn get(self, behavior: &MenuBehavior) -> i16 {
        behavior.get_data(self.index).unwrap_or(0)
    }

    /// Writes a new value to this data slot.
    pub fn set(self, behavior: &mut MenuBehavior, value: i16) {
        behavior.set_data(self.index, value);
    }

    /// The raw data slot index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }
}

/// A declarative shift-click route: take from `from`, then try each target
/// range in order, stopping at the first that accepts anything.
struct Route {
    from: Range<usize>,
    targets: Vec<Range<usize>>,
    backwards: bool,
}

/// Builds the slots, containers, data slots and routing for a menu.
///
/// See the [module documentation](self) for an overview.
#[derive(Default)]
pub struct MenuBuilder {
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
            menu_type: menu_type.into(),
            container_id,
            ..Self::default()
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

    /// Adds `count` predicate-restricted slots backed by `container` at indices
    /// `0..count`.
    ///
    /// `may_place` decides which items the slots accept (e.g. only damageable
    /// items for a repair input). `may_pickup` optionally gates whether the
    /// player can take the current item out (pass `None` to always allow it);
    /// it receives the lock guard, the player, and the item being removed.
    ///
    /// Both predicates are shared across every slot in the section via [`Arc`],
    /// so the closures are stored once regardless of `count`. Pass bare
    /// closures — they are boxed internally. The slots use the default max
    /// stack size of 64.
    ///
    /// Returns a [`Section`] handle over the slots that were added.
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

    /// Adds the player's 36 inventory slots (main inventory then hotbar).
    ///
    /// Returns handles to the combined range as well as the main inventory and
    /// hotbar sub-ranges, which routing usually needs separately.
    pub fn player_inventory(&mut self, inventory: &SyncPlayerInv) -> PlayerInventorySections {
        let start = self.slots.len();
        add_standard_inventory_slots(&mut self.slots, inventory);
        self.register_container(ContainerRef::PlayerInventory(inventory.clone()));

        let main = Section::new(start..start + 27);
        let hotbar = Section::new(start + 27..self.slots.len());
        let all = Section::new(start..self.slots.len());
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
        DataSlot { index }
    }

    /// Declares a shift-click route from one section into others.
    ///
    /// When a slot in `from` is shift-clicked, [`MenuLayout::quick_move`] walks
    /// `targets` in order and stops at the first that accepts any items. Set
    /// `backwards` to fill the targets from the end (vanilla does this when
    /// moving into the player inventory so existing stacks top up first).
    pub fn route(
        &mut self,
        from: Section,
        targets: impl IntoIterator<Item = Section>,
        backwards: bool,
    ) -> &mut Self {
        self.routes.push(Route {
            from: from.range(),
            targets: targets.into_iter().map(Section::range).collect(),
            backwards,
        });
        self
    }

    /// Marks `sections` to be emptied back into the player on close.
    ///
    /// Input grids (crafting, anvil, …) must not swallow items when the window
    /// closes; [`MenuLayout::return_drained_items`] returns the listed slots'
    /// contents to the player's inventory (dropping overflow). Only list real
    /// input sections — fake/result slots are computed and would dupe items.
    pub fn drain(&mut self, sections: impl IntoIterator<Item = Section>) -> &mut Self {
        self.drain_sections
            .extend(sections.into_iter().map(Section::range));
        self
    }

    /// Consumes the builder, producing the menu behavior and its layout.
    #[must_use]
    pub fn build(self) -> BuiltMenu {
        let mut behavior = MenuBehavior::new(
            self.slots,
            self.container_id,
            self.menu_type,
            self.container_refs,
        );
        for initial in self.data_slots {
            behavior.add_data_slot(initial);
        }

        BuiltMenu {
            behavior,
            layout: MenuLayout {
                routes: self.routes,
                drain_sections: self.drain_sections,
            },
        }
    }

    /// Records a container to lock, ignoring it if an equal one is already present.
    fn register_container(&mut self, container: ContainerRef) {
        let id = container.container_id();
        if !self.container_refs.iter().any(|c| c.container_id() == id) {
            self.container_refs.push(container);
        }
    }

    /// Returns a section spanning `start..self.slots.len()`.
    const fn section_from(&self, start: usize) -> Section {
        Section::new(start..self.slots.len())
    }
}

/// The result of [`MenuBuilder::build`]: a ready [`MenuBehavior`] plus the
/// [`MenuLayout`] describing its sections and routing.
pub struct BuiltMenu {
    /// The assembled shared menu state.
    pub behavior: MenuBehavior,
    /// Section ranges and shift-click routes derived from the builder.
    pub layout: MenuLayout,
}

/// The static layout of a built menu: named section ranges and the shift-click
/// route table. Held alongside [`MenuBehavior`] so a menu can drive a generic
/// [`quick_move`](MenuLayout::quick_move) instead of writing one by hand.
pub struct MenuLayout {
    routes: Vec<Route>,
    drain_sections: Vec<Range<usize>>,
}

impl MenuLayout {
    /// Returns every item in the [`drain`](MenuBuilder::drain) sections to the
    /// player, emptying those slots. Call from `removed` so input grids hand
    /// their contents back on close instead of deleting them.
    pub fn return_drained_items(&self, behavior: &MenuBehavior, player: &Player) {
        if self.drain_sections.is_empty() {
            return;
        }

        let mut guard = behavior.lock_all_containers();
        for range in &self.drain_sections {
            for slot_index in range.clone() {
                let item = mem::take(behavior.slots[slot_index].get_item_mut(&mut guard));
                if !item.is_empty() {
                    player.add_item_or_drop_with_guard(&mut guard, item);
                }
            }
        }
    }

    /// Performs a generic shift-click for `slot_index` using the route table.
    ///
    /// This reproduces the hand-written `quick_move_stack` shape: find the route
    /// whose source contains the clicked slot, move the stack into the first
    /// target that accepts it, write the remainder back, and — for fake result
    /// slots — fire `on_take` (returning any crafting remainder to the player).
    ///
    /// Returns the item that was originally in the slot, or empty if nothing
    /// moved, matching [`Menu::quick_move_stack`](crate::inventory::menu::Menu::quick_move_stack).
    pub fn quick_move(
        &self,
        behavior: &MenuBehavior,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        player: &Player,
    ) -> ItemStack {
        let Some(route) = self.routes.iter().find(|r| r.from.contains(&slot_index)) else {
            return ItemStack::empty();
        };

        let clicked = behavior.slots[slot_index].get_item(guard).clone();
        if clicked.is_empty() {
            return ItemStack::empty();
        }

        // Reject stale pickups — e.g. a result slot whose recipe no longer
        // matches the inputs. Normal slots always allow it.
        if !behavior.slots[slot_index].may_pickup(guard, player) {
            return ItemStack::empty();
        }

        let mut remaining = clicked.clone();
        let moved = route.targets.iter().any(|target| {
            behavior.move_item_stack_to(
                guard,
                &mut remaining,
                target.start,
                target.end,
                route.backwards,
            )
        });
        if !moved {
            return ItemStack::empty();
        }

        let slot = &behavior.slots[slot_index];
        if remaining.is_empty() {
            slot.set_by_player(guard, ItemStack::empty(), &clicked);
        } else {
            // Write the un-moved remainder back to the source. Fake/result slots
            // don't store items (their contents are recomputed), so only touch
            // real slots — otherwise the moved portion would be duplicated.
            if !slot.is_fake() {
                slot.set_item(guard, remaining.clone());
            }
            slot.set_changed(guard);
        }

        // Nothing actually left the slot (e.g. the target was full).
        if remaining.count == clicked.count {
            return ItemStack::empty();
        }

        // Result slots need their take callback to fire (recipe consumption,
        // experience, etc.); the remainder is returned to the player.
        if slot.is_fake() {
            let taken = clicked.copy_with_count(clicked.count - remaining.count);
            if let Some(leftover) = slot.on_take(guard, &taken, player) {
                player.add_item_or_drop_with_guard(guard, leftover);
            }
            // Output that didn't fit in the inventory is dropped, matching
            // vanilla's result-slot shift-click (`player.drop(stack, false)`).
            if !remaining.is_empty() {
                player.drop_item(remaining.clone(), false, true);
            }
        }

        clicked
    }
}
