//! Equipment system for entities.
//!
//! This module provides the core equipment infrastructure:
//! - [`EquipmentSlot`] - Enum representing equipment slots (main hand, armor, etc.)
//! - [`EquipmentSlotType`] - Categories of equipment slots
//! - [`EntityEquipment`] - Shared equipment access
//! - [`OwnedEntityEquipment`] - Owned storage for non-player living entities

mod entity_equipment;

pub use entity_equipment::{EntityEquipment, OwnedEntityEquipment};
pub use steel_registry::equipment::{EquipmentSlot, EquipmentSlotType};
