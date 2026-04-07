use steel_registry::item_stack::ItemStack;

use crate::{
    inventory::{
        container::Container,
        lock::{ContainerId, ContainerLockGuard},
        recipe_manager,
        slots::{
            recipe_handlers::RecipeHandler,
            slot::{SyncCraftingContainer, SyncResultContainer},
        },
    },
    player::Player,
};

#[derive(Clone)]
pub struct CraftingHandler {
    crafting_container: SyncCraftingContainer,
    result_container: SyncResultContainer,
    grid_size: usize,
}

impl CraftingHandler {
    pub const fn new(
        crafting_container: SyncCraftingContainer,
        result_container: SyncResultContainer,
        grid_size: usize,
    ) -> Self {
        Self {
            crafting_container,
            result_container,
            grid_size,
        }
    }

    #[must_use]
    pub const fn is_2x2(&self) -> bool {
        self.grid_size == 2
    }

    #[must_use]
    pub fn crafting_id(&self) -> ContainerId {
        ContainerId::from_arc(&self.crafting_container)
    }

    #[must_use]
    pub fn result_id(&self) -> ContainerId {
        ContainerId::from_arc(&self.result_container)
    }
}

impl RecipeHandler for CraftingHandler {
    fn update_result(&self, guard: &mut ContainerLockGuard) {
        let crafting = guard
            .get_crafting_container(self.crafting_id())
            .expect("crafting container not locked");

        let result_stack = recipe_manager::find_recipe(crafting, self.is_2x2())
            .map_or_else(ItemStack::empty, |r| r.assemble());

        guard
            .get_result_container_mut(self.result_id())
            .expect("result container not locked")
            .set_item(0, result_stack);
    }

    fn on_result_taken(
        &self,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) -> Option<ItemStack> {
        let mut remainder_overflow: Vec<ItemStack> = Vec::new();

        let remainders_and_positioned = {
            let crafting = guard
                .get_crafting_container(self.crafting_id())
                .expect("crafting container not locked");
            recipe_manager::get_remaining_items(crafting, self.is_2x2())
        };

        let crafting = guard
            .get_crafting_container_mut(self.crafting_id())
            .expect("crafting container not locked");

        if let Some((remainders, positioned)) = remainders_and_positioned {
            let input = &positioned.input;

            for y in 0..input.height {
                for x in 0..input.width {
                    let grid_slot = positioned.to_grid_slot(x, y, self.grid_size);
                    let remainder_idx = x + y * input.width;
                    let replacement = if remainder_idx < remainders.len() {
                        remainders[remainder_idx].clone()
                    } else {
                        ItemStack::empty()
                    };

                    {
                        let item = crafting.get_item_mut(grid_slot);
                        if !item.is_empty() {
                            item.shrink(1);
                        }
                    }

                    if !replacement.is_empty() {
                        let current_item = crafting.get_item(grid_slot).clone();

                        if current_item.is_empty() {
                            crafting.set_item(grid_slot, replacement);
                        } else if ItemStack::is_same_item_same_components(
                            &current_item,
                            &replacement,
                        ) {
                            crafting.get_item_mut(grid_slot).grow(replacement.count());
                        } else {
                            remainder_overflow.push(replacement);
                        }
                    }
                }
            }
        }

        crafting.set_changed();
        self.update_result(guard);

        if remainder_overflow.is_empty() {
            return None;
        }
        Some(remainder_overflow.remove(0))
    }
}
