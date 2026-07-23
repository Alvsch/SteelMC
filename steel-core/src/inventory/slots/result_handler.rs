use std::sync::Arc;

use enum_dispatch::enum_dispatch;
use steel_registry::item_stack::ItemStack;

use crate::{inventory::lock::ContainerLockGuard, player::Player};

/// A trait for recipe handlers that update slots in containers according to recipes
#[enum_dispatch]
pub trait ResultHandler: Send + Sync {
    /// Recalculate the result based on current inputs.
    fn update_result(&self, guard: &mut ContainerLockGuard);

    /// Consume inputs when the result is taken. Return overflow remainders.
    fn on_result_taken(&self, guard: &mut ContainerLockGuard, player: &Player)
    -> Option<ItemStack>;

    /// Whether the stored result still matches the current inputs.
    fn is_result_valid(&self, guard: &ContainerLockGuard, player: &Player) -> bool;
}

impl<T: ResultHandler + ?Sized> ResultHandler for Arc<T> {
    fn update_result(&self, guard: &mut ContainerLockGuard) {
        (**self).update_result(guard);
    }

    fn on_result_taken(
        &self,
        guard: &mut ContainerLockGuard,
        player: &Player,
    ) -> Option<ItemStack> {
        (**self).on_result_taken(guard, player)
    }

    fn is_result_valid(&self, guard: &ContainerLockGuard, player: &Player) -> bool {
        (**self).is_result_valid(guard, player)
    }
}
