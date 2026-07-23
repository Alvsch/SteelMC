//! The most common imports needed when working with inventories and menus
pub use crate::inventory::{
    click::{Click, ClickOutcome, DragKind, MouseButton, QuickCraft, SwapTarget},
    container::{Container, SimpleContainer},
    equipment::{EntityEquipment, EquipmentSlot, EquipmentSlotType},
    lock::{ContainerId, ContainerLockGuard, ContainerRef},
    menu::{
        ContainerSlots, DataSlot, FillDirection, GridPlacer, Menu, MenuBehavior, MenuBuilder,
        MenuKind, MenuKindType, PlayerInventorySections, Rect, Region, Section, SectionSource,
    },
    slots::{NormalSlot, ResultHandler, RestrictedSlot, Slot, SlotType},
};

pub use crate::player::Player;
pub use steel_registry::item_stack::ItemStack;
pub use steel_utils::locks::{IntoShared, Shared};
