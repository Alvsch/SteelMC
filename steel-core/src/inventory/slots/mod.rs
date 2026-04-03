mod anvil_slots;
mod armor_slot;
mod crafting_slots;
mod normal_slot;
mod recipe_handlers;
pub mod slot;

pub use anvil_slots::{AnvilInputSlot, AnvilResultSlot};
pub use armor_slot::ArmorSlot;
pub use crafting_slots::CraftingHandler;
pub use normal_slot::NormalSlot;
pub use recipe_handlers::{
    ProcessingInputSlot, ProcessingResultSlot, RecipeHandler, RecipeHandlerType,
};
pub use slot::*;
