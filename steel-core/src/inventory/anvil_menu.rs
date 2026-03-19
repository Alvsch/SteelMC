//! Anvil Menus
use std::sync::Arc;

use steel_registry::{item_stack::ItemStack, menu_type::MenuTypeRef, vanilla_menu_types};
use steel_utils::{BlockPos, locks::SyncMutex, translations};
use text_components::TextComponent;

use crate::{
    inventory::{
        MenuInstance, MenuProvider, SyncPlayerInv,
        container::Container,
        crafting::ResultContainer,
        lock::ContainerRef,
        menu::{Menu, MenuBehavior},
        slot::{
            AnvilResultSlot, NormalSlot, SlotType, SyncResultContainer,
            add_standard_inventory_slots,
        },
    },
    player::Player,
};

/// Slot indices for the anvil menu.
pub mod slots {
    /// Slot index for the first input item (slot 0).
    pub const FIRST_INPUT_SLOT: usize = 0;
    /// Slot index for the second input item (slot 1).
    pub const SECOND_INPUT_SLOT: usize = 1;
    /// Slot index for the result (slot 2).
    pub const RESULT_SLOT: usize = 2;
    /// Start of main inventory (slot 3).
    pub const INV_SLOT_START: usize = 3;
    /// End of main inventory (slot 30, exclusive).
    pub const INV_SLOT_END: usize = 30;
    /// Start of hotbar (slot 30).
    pub const HOTBAR_SLOT_START: usize = 30;
    /// End of hotbar (slot 39, exclusive).
    pub const HOTBAR_SLOT_END: usize = 39;
    /// Total number of slots in the anvil menu.
    pub const TOTAL_SLOTS: usize = 39;
}

/// Anvil Menu Behavior
pub struct AnvilMenu {
    /// The Menu Behavior
    behavior: MenuBehavior,
    /// The Input Slots
    input_container: Arc<SyncMutex<SimpleContainer>>,
    /// The Result Slot
    result_container: SyncResultContainer,
    /// The Position
    block_pos: BlockPos,
}

impl AnvilMenu {
    /// Creates a new Anvil Menu
    #[must_use]
    pub fn new(inventory: SyncPlayerInv, container_id: u8, pos: BlockPos) -> Self {
        let mut menu_slots = Vec::with_capacity(slots::TOTAL_SLOTS);

        let simple_container = Arc::new(SyncMutex::new(SimpleContainer::new(2)));
        let container_ref = ContainerRef::SimpleContainer(simple_container.clone());

        let result_container: SyncResultContainer =
            Arc::new(SyncMutex::new(ResultContainer::new()));

        menu_slots.push(SlotType::Normal(NormalSlot::new(container_ref.clone(), 0))); // FIXME: dont use normal slots
        menu_slots.push(SlotType::AnvilResult(AnvilResultSlot::new(
            simple_container.clone(),
            result_container.clone(),
        )));

        add_standard_inventory_slots(&mut menu_slots, &inventory);

        let mut behavior =
            MenuBehavior::new(menu_slots, container_id, Some(vanilla_menu_types::ANVIL));
        behavior.add_data_slot(0);

        Self {
            behavior,
            input_container: simple_container,
            result_container,
            block_pos: pos,
        }
    }
}

/// A Simple Container
pub struct SimpleContainer {
    items: Vec<ItemStack>,
}

impl SimpleContainer {
    /// Creates a new Simple Container
    #[must_use]
    pub fn new(size: usize) -> Self {
        Self {
            items: vec![ItemStack::empty(); size],
        }
    }
}

impl Container for SimpleContainer {
    #[doc = " Returns the number of slots in this container."]
    fn get_container_size(&self) -> usize {
        self.items.len()
    }

    #[doc = " Returns a reference to the item in the specified slot."]
    fn get_item(&self, slot: usize) -> &ItemStack {
        &self.items[slot]
    }

    #[doc = " Returns a mutable reference to the item in the specified slot."]
    fn get_item_mut(&mut self, slot: usize) -> &mut ItemStack {
        &mut self.items[slot]
    }

    #[doc = " Sets the item in the specified slot."]
    fn set_item(&mut self, slot: usize, stack: ItemStack) {
        self.items[slot] = stack;
    }

    #[doc = " Marks this container as changed (dirty) for saving/syncing."]
    fn set_changed(&mut self) {}
}

impl Menu for AnvilMenu {
    fn behavior(&self) -> &MenuBehavior {
        &self.behavior
    }

    fn behavior_mut(&mut self) -> &mut MenuBehavior {
        &mut self.behavior
    }

    fn quick_move_stack(
        &mut self,
        _guard: &mut super::lock::ContainerLockGuard,
        _slot_index: usize,
        _layer: &Player,
    ) -> ItemStack {
        todo!()
    }
}

impl MenuInstance for AnvilMenu {
    fn menu_type(&self) -> MenuTypeRef {
        vanilla_menu_types::ANVIL
    }

    fn container_id(&self) -> u8 {
        self.behavior.container_id
    }
}

/// Provider for creating a anvil menu.
pub struct AnvilMenuProvider {
    inventory: SyncPlayerInv,
    pos: BlockPos,
}

impl AnvilMenuProvider {
    /// Creates a new anvil menu provider.
    #[must_use]
    pub const fn new(inventory: SyncPlayerInv, pos: BlockPos) -> Self {
        Self { inventory, pos }
    }
}

impl MenuProvider for AnvilMenuProvider {
    fn title(&self) -> TextComponent {
        TextComponent::translated(translations::CONTAINER_REPAIR.msg())
    }

    fn create(&self, container_id: u8) -> Box<dyn MenuInstance> {
        Box::new(AnvilMenu::new(
            self.inventory.clone(),
            container_id,
            self.pos,
        ))
    }
}
