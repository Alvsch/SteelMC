//! Anvil Menus
use std::{mem, sync::Arc};

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
        simple_menu::SimpleContainer,
        slot::{
            AnvilResultSlot, NormalSlot, Slot, SlotType, SyncResultContainer,
            add_standard_inventory_slots,
        },
    },
    player::Player,
};

use super::lock::ContainerLockGuard;

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
    #[expect(dead_code, reason = "not yet implemented")]
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

        menu_slots.push(SlotType::Normal(NormalSlot::new(container_ref.clone(), 0)));
        menu_slots.push(SlotType::Normal(NormalSlot::new(container_ref.clone(), 1)));
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

impl Menu for AnvilMenu {
    fn behavior(&self) -> &MenuBehavior {
        &self.behavior
    }

    fn behavior_mut(&mut self) -> &mut MenuBehavior {
        &mut self.behavior
    }

    fn quick_move_stack(
        &mut self,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        player: &Player,
    ) -> ItemStack {
        if slot_index >= self.behavior.slots.len() {
            return ItemStack::empty();
        }

        let clicked = self.behavior.slots[slot_index].get_item(guard).clone();
        if clicked.is_empty() {
            return ItemStack::empty();
        }

        let mut stack_mut = clicked.clone();

        if slot_index == slots::RESULT_SLOT {
            if !self.behavior.move_item_stack_to(
                guard,
                &mut stack_mut,
                slots::INV_SLOT_START,
                slots::HOTBAR_SLOT_END,
                true,
            ) {
                return ItemStack::empty();
            }
        } else if (0..slots::RESULT_SLOT).contains(&slot_index) {
            if !self.behavior.move_item_stack_to(
                guard,
                &mut stack_mut,
                slots::INV_SLOT_START,
                slots::HOTBAR_SLOT_END,
                false,
            ) {
                return ItemStack::empty();
            }
        } else if (slots::INV_SLOT_START..slots::HOTBAR_SLOT_END).contains(&slot_index) {
            if !self.behavior.move_item_stack_to(
                guard,
                &mut stack_mut,
                0,
                slots::RESULT_SLOT,
                false,
            ) {
                if (slots::INV_SLOT_START..slots::INV_SLOT_END).contains(&slot_index) {
                    if !self.behavior.move_item_stack_to(
                        guard,
                        &mut stack_mut,
                        slots::HOTBAR_SLOT_START,
                        slots::HOTBAR_SLOT_END,
                        false,
                    ) {
                        return ItemStack::empty();
                    }
                } else if !self.behavior.move_item_stack_to(
                    guard,
                    &mut stack_mut,
                    slots::INV_SLOT_START,
                    slots::INV_SLOT_END,
                    false,
                ) {
                    return ItemStack::empty();
                }
            }
        } else {
            return ItemStack::empty();
        }

        if stack_mut.is_empty() {
            self.behavior.slots[slot_index].set_by_player(guard, ItemStack::empty(), &clicked);
        } else {
            self.behavior.slots[slot_index].set_changed(guard);
        }

        if stack_mut.count == clicked.count {
            return ItemStack::empty();
        }

        self.behavior.slots[slot_index].on_take(guard, &stack_mut, player);

        stack_mut
    }

    fn removed(&mut self, player: &Player) {
        let carried = mem::take(&mut self.behavior.carried);

        if !carried.is_empty() {
            player.add_item_or_drop(carried);
        }

        let items = self
            .input_container
            .lock()
            .iter_mut()
            .map(mem::take)
            .filter(|item| !item.is_empty())
            .collect::<Vec<ItemStack>>();

        for item in items {
            player.add_item_or_drop(item);
        }

        self.result_container.lock().set_item(0, ItemStack::empty());
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
