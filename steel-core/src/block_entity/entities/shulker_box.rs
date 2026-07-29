use std::sync::{Arc, Weak};

use simdnbt::{
    ToNbtTag,
    borrow::{BaseNbtCompound, NbtCompound as NbtCompoundView},
    owned::{NbtCompound, NbtList, NbtTag},
};
use steel_registry::{
    ItemStackTemplate,
    data_components::{ItemContainerContents, vanilla_components::CONTAINER},
    item_stack::ItemStack,
    vanilla_block_entity_types,
};
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey, locks::SyncMutex};

use crate::{
    block_entity::{BlockEntity, BlockEntityBase},
    inventory::{
        container::Container,
        lock::{ContainerRef, SharedContainer},
    },
    world::World,
};

/// Number of slots in a shulker box (3 rows of 9).
pub const SHULKER_BOX_SLOTS: usize = 27;

/// The current animation state of a shulker box.
#[derive(Debug, Clone, Copy)]
pub enum AnimationStatus {
    /// Fully closed.
    Closed,
    /// Opening.
    Opening,
    /// Fully open.
    Opened,
    /// Closing.
    Closing,
}
/// Behavior for shulker box blocks.
pub struct ShulkerBoxBlockEntity {
    base: Arc<BlockEntityBase>,
    container: Arc<SyncMutex<ShulkerBoxContainer>>,
    container_ref: ContainerRef,
    animation_status: AnimationStatus,
}

struct ShulkerBoxContainer {
    items: Vec<ItemStack>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ShulkerBoxEntity`.
unsafe impl DowncastType for ShulkerBoxBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/shulker_box");
}

// SAFETY: This key is owned by Steel and uniquely identifies the independently
// lockable inventory data used by a barrel block entity.
unsafe impl DowncastType for ShulkerBoxContainer {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:container/shulker_box");
}

impl ShulkerBoxBlockEntity {
    /// Creates a new barrel block entity.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        let base = Arc::new(BlockEntityBase::new(
            &vanilla_block_entity_types::SHULKER_BOX,
            level,
            pos,
            state,
        ));
        let container = Arc::new(SyncMutex::new(ShulkerBoxContainer {
            items: vec![ItemStack::empty(); SHULKER_BOX_SLOTS],
        }));
        let shared_container: SharedContainer = container.clone();
        Self {
            container_ref: ContainerRef::owned_by_block_entity(shared_container, Arc::clone(&base)),
            base,
            container,
            animation_status: AnimationStatus::Closed,
        }
    }

    /// Checks if the block entity's container has any items
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.container.lock().is_empty()
    }

    /// Collects all the items inside the block entity's container
    #[must_use]
    pub fn collect_items(&self) -> Vec<ItemStack> {
        self.container.lock().items.clone()
    }

    /// Collect all the items inside the shulker box into an `ItemContainerContents`
    ///
    /// # Panics
    /// Panics if shulker box somehow has more than 256 slots. This should never happen
    #[must_use]
    pub fn collect_components(&self) -> ItemContainerContents {
        let container = self.container.lock();

        let size = container.get_container_size();
        let slots: Vec<Option<ItemStackTemplate>> = (0..size)
            .map(|i| {
                let item = container.get_item(i);
                if item.is_empty() {
                    None
                } else {
                    ItemStackTemplate::from_stack(item).ok()
                }
            })
            .collect();

        ItemContainerContents::new(slots)
            .expect("shulker box slot count is always within the 256 container-contents limit")
    }

    /// Get the current animation state
    #[must_use]
    pub fn animation_status(&self) -> AnimationStatus {
        self.animation_status
    }
}

impl BlockEntity for ShulkerBoxBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    fn load_additional(&self, nbt: &BaseNbtCompound<'_>) {
        // Convert to NbtCompound view for accessing methods
        let nbt_view: NbtCompoundView<'_, '_> = nbt.into();
        let mut container = self.container.lock();
        container.items.fill(ItemStack::empty());

        // Load items from NBT using borrowed NBT for proper ItemStack parsing
        if let Some(items_list) = nbt_view.list("Items")
            && let Some(compounds) = items_list.compounds()
        {
            for compound in compounds {
                // Each item has a "Slot" byte and item data
                if let Some(slot) = compound.byte("Slot") {
                    let slot = slot as usize;
                    if slot < SHULKER_BOX_SLOTS {
                        // Parse item directly from the borrowed compound
                        if let Some(item) = ItemStack::from_borrowed_compound(&compound) {
                            container.items[slot] = item;
                        }
                    }
                }
            }
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        // Save items to NBT (only non-empty slots)
        let container = self.container.lock();
        let mut items: Vec<NbtCompound> = Vec::new();
        for (slot, item) in container.items().iter().enumerate() {
            if !item.is_empty() {
                // Use ItemStack's ToNbtTag implementation for proper component serialization
                if let NbtTag::Compound(mut item_nbt) = item.clone().to_nbt_tag() {
                    item_nbt.insert("Slot", slot as i8);
                    items.push(item_nbt);
                }
            }
        }
        nbt.insert("Items", NbtList::Compound(items));
    }

    fn apply_components_from_item(&self, item: &ItemStack) {
        let Some(contents) = item.get(CONTAINER) else {
            return;
        };

        let mut container = self.container.lock();
        container.items.fill(ItemStack::empty());
        for (slot, template) in contents.items().iter().enumerate() {
            if slot >= SHULKER_BOX_SLOTS {
                break;
            }
            if let Some(template) = template {
                container.items_mut()[slot] = ItemStack::with_count_and_patch(
                    template.item(),
                    template.count(),
                    template.components().clone(),
                );
            }
        }
    }

    fn container_ref(&self) -> Option<ContainerRef> {
        Some(self.container_ref.clone())
    }
}

impl Container for ShulkerBoxContainer {
    fn items(&self) -> &[ItemStack] {
        &self.items
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        &mut self.items
    }

    fn get_container_size(&self) -> usize {
        SHULKER_BOX_SLOTS
    }

    fn set_item(&mut self, slot: usize, mut stack: ItemStack) {
        if slot < SHULKER_BOX_SLOTS {
            let max_stack_size = self.get_max_stack_size_for_item(&stack);
            if !stack.is_empty() && stack.count() > max_stack_size {
                stack.set_count(max_stack_size);
            }
            self.items[slot] = stack;
        }
    }

    fn get_max_stack_size(&self) -> i32 {
        64
    }

    fn set_changed(&mut self) {}
}
