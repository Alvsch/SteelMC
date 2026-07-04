//! Validated container-click input.
//!
//! [`SContainerClick`](steel_protocol::packets::game::SContainerClick) carries
//! three raw fields (`slot_num: i16`, `button: i8`, [`ClickType`]) whose
//! meanings depend on each other: `-999` means "outside the window", the swap
//! button is a hotbar index or `40` for the offhand, and a drag click
//! bit-packs its phase and kind into `button`. [`Click::parse`] decodes and
//! validates all of that once at the packet boundary. Every slot index inside
//! a [`Click`] is in range for the menu it was parsed against, so the click
//! handlers in [`Menu`](crate::inventory::Menu) start at their actual logic
//! instead of re-validating raw integers.

use steel_protocol::packets::game::ClickType;

use crate::inventory::FillDirection;

/// Raw slot value sent when the player clicks outside the window.
pub const SLOT_CLICKED_OUTSIDE: i16 = -999;

/// Player-inventory index of the offhand slot.
const OFFHAND_INVENTORY_SLOT: usize = 40;

/// A container click, validated from the raw protocol fields.
///
/// Produced by [`Click::parse`]; every `slot` is guaranteed in range for the
/// menu the click was parsed against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    /// Left/right click on a slot.
    Pickup {
        /// The clicked slot.
        slot: usize,
        /// Which mouse button was used.
        button: MouseButton,
    },
    /// Left/right click outside the window, dropping the carried stack
    /// (`slot_num == -999` on the wire).
    DropCarried {
        /// Left drops the whole stack, right drops a single item.
        button: MouseButton,
    },
    /// Shift-click.
    QuickMove {
        /// The clicked slot.
        slot: usize,
    },
    /// Number key 1-9 or the offhand key, swapping a menu slot with a player
    /// inventory slot.
    Swap {
        /// The clicked menu slot.
        slot: usize,
        /// The player inventory slot to swap with.
        with: SwapTarget,
    },
    /// Middle-click copy (creative only).
    Clone {
        /// The clicked slot.
        slot: usize,
    },
    /// Drop key: Q (single item) or Ctrl+Q (whole stack).
    Throw {
        /// The clicked slot.
        slot: usize,
        /// True for Ctrl+Q (drop the whole stack, repeating while the slot
        /// refills with the same item).
        whole_stack: bool,
    },
    /// Double-click, collecting matching stacks into the cursor.
    PickupAll {
        /// The double-clicked slot.
        slot: usize,
        /// Which end of the menu to start collecting from.
        direction: FillDirection,
    },
    /// One phase of a drag (paint) operation.
    QuickCraft(QuickCraft),
}

/// A mouse button, decoded from the raw `button` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    /// The primary (left) button.
    Left,
    /// The secondary (right) button.
    Right,
}

/// The player-inventory slot a [`Click::Swap`] exchanges with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapTarget {
    /// A hotbar slot (`0..=8`, the number keys).
    Hotbar(u8),
    /// The offhand slot (the swap-hands key, `40` on the wire).
    Offhand,
}

impl SwapTarget {
    /// The player inventory index this target maps to.
    #[must_use]
    pub const fn inventory_slot(self) -> usize {
        match self {
            Self::Hotbar(index) => index as usize,
            Self::Offhand => OFFHAND_INVENTORY_SLOT,
        }
    }
}

/// One phase of a drag operation, decoded from the bit-packed `button` field
/// (phase in bits 0-1, kind in bits 2-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickCraft {
    /// Begin a drag; requires the state machine to be idle.
    Start {
        /// Which kind of drag is starting.
        kind: DragKind,
    },
    /// Add a slot to the active drag.
    AddSlot {
        /// The slot under the cursor.
        slot: usize,
        /// Which kind of drag is active.
        kind: DragKind,
    },
    /// Finish the drag and distribute the carried items.
    End {
        /// Which kind of drag is ending.
        kind: DragKind,
    },
}

/// The kind of drag being performed, named after the initiating button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragKind {
    /// Left-button drag: distribute the carried stack evenly.
    Left,
    /// Right-button drag: place one item per slot.
    Right,
    /// Middle-button drag (creative only): place a full stack per slot.
    Clone,
}

impl DragKind {
    /// The legacy `quickcraft_type` value this kind stores in the menu's drag
    /// state (see the `QUICKCRAFT_TYPE_*` constants in
    /// [`menu`](crate::inventory::menu)).
    pub(crate) const fn quickcraft_type(self) -> i32 {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Clone => 2,
        }
    }
}

impl Click {
    /// Parses the raw fields of a container-click packet against a menu with
    /// `slot_count` slots.
    ///
    /// Returns `None` for malformed input — an out-of-range slot, an invalid
    /// swap button, an unknown mouse button, or an invalid drag encoding.
    /// Callers should ignore the click (vanilla's behavior for packets its
    /// clients never send) but may still want to resync the client.
    #[must_use]
    pub fn parse(
        slot_num: i16,
        button: i8,
        click_type: ClickType,
        slot_count: usize,
    ) -> Option<Self> {
        // In-range slot index, or None for -999/-1/garbage.
        let slot = || usize::try_from(slot_num).ok().filter(|&i| i < slot_count);
        let mouse_button = || match button {
            0 => Some(MouseButton::Left),
            1 => Some(MouseButton::Right),
            _ => None,
        };

        match click_type {
            ClickType::Pickup => {
                let button = mouse_button()?;
                if slot_num == SLOT_CLICKED_OUTSIDE {
                    Some(Self::DropCarried { button })
                } else {
                    Some(Self::Pickup {
                        slot: slot()?,
                        button,
                    })
                }
            }
            ClickType::QuickMove => Some(Self::QuickMove { slot: slot()? }),
            ClickType::Swap => {
                let with = match button {
                    0..=8 => SwapTarget::Hotbar(button as u8),
                    40 => SwapTarget::Offhand,
                    _ => return None,
                };
                Some(Self::Swap {
                    slot: slot()?,
                    with,
                })
            }
            ClickType::Clone => Some(Self::Clone { slot: slot()? }),
            ClickType::Throw => Some(Self::Throw {
                slot: slot()?,
                whole_stack: match button {
                    0 => false,
                    1 => true,
                    _ => return None,
                },
            }),
            ClickType::PickupAll => Some(Self::PickupAll {
                slot: slot()?,
                direction: match button {
                    0 => FillDirection::Forward,
                    1 => FillDirection::Backward,
                    _ => return None,
                },
            }),
            ClickType::QuickCraft => {
                // Kind in bits 2-3, phase in bits 0-1 (Java's
                // getQuickcraftType / getQuickcraftHeader).
                let kind = match (button >> 2) & 3 {
                    0 => DragKind::Left,
                    1 => DragKind::Right,
                    2 => DragKind::Clone,
                    _ => return None,
                };
                match button & 3 {
                    0 => Some(Self::QuickCraft(QuickCraft::Start { kind })),
                    1 => Some(Self::QuickCraft(QuickCraft::AddSlot {
                        slot: slot()?,
                        kind,
                    })),
                    2 => Some(Self::QuickCraft(QuickCraft::End { kind })),
                    _ => None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOTS: usize = 46;

    #[test]
    fn pickup_slot_and_outside() {
        assert_eq!(
            Click::parse(5, 0, ClickType::Pickup, SLOTS),
            Some(Click::Pickup {
                slot: 5,
                button: MouseButton::Left
            })
        );
        assert_eq!(
            Click::parse(SLOT_CLICKED_OUTSIDE, 1, ClickType::Pickup, SLOTS),
            Some(Click::DropCarried {
                button: MouseButton::Right
            })
        );
        // Unknown mouse button.
        assert_eq!(Click::parse(5, 2, ClickType::Pickup, SLOTS), None);
    }

    #[test]
    fn out_of_range_slots_rejected() {
        assert_eq!(Click::parse(-1, 0, ClickType::Pickup, SLOTS), None);
        assert_eq!(Click::parse(46, 0, ClickType::QuickMove, SLOTS), None);
        assert_eq!(Click::parse(-999, 0, ClickType::Throw, SLOTS), None);
        // Last valid index is fine.
        assert_eq!(
            Click::parse(45, 0, ClickType::QuickMove, SLOTS),
            Some(Click::QuickMove { slot: 45 })
        );
    }

    #[test]
    fn swap_targets() {
        assert_eq!(
            Click::parse(3, 8, ClickType::Swap, SLOTS),
            Some(Click::Swap {
                slot: 3,
                with: SwapTarget::Hotbar(8)
            })
        );
        assert_eq!(
            Click::parse(3, 40, ClickType::Swap, SLOTS),
            Some(Click::Swap {
                slot: 3,
                with: SwapTarget::Offhand
            })
        );
        assert_eq!(SwapTarget::Hotbar(4).inventory_slot(), 4);
        assert_eq!(SwapTarget::Offhand.inventory_slot(), 40);
        // 9 and negatives are not valid swap buttons.
        assert_eq!(Click::parse(3, 9, ClickType::Swap, SLOTS), None);
        assert_eq!(Click::parse(3, -1, ClickType::Swap, SLOTS), None);
    }

    #[test]
    fn throw_and_pickup_all_buttons() {
        assert_eq!(
            Click::parse(7, 1, ClickType::Throw, SLOTS),
            Some(Click::Throw {
                slot: 7,
                whole_stack: true
            })
        );
        assert_eq!(Click::parse(7, 2, ClickType::Throw, SLOTS), None);
        assert_eq!(
            Click::parse(7, 0, ClickType::PickupAll, SLOTS),
            Some(Click::PickupAll {
                slot: 7,
                direction: FillDirection::Forward
            })
        );
        assert_eq!(
            Click::parse(7, 1, ClickType::PickupAll, SLOTS),
            Some(Click::PickupAll {
                slot: 7,
                direction: FillDirection::Backward
            })
        );
    }

    #[test]
    fn quickcraft_encoding() {
        // Phase in the low bits, kind in bits 2-3.
        assert_eq!(
            Click::parse(-999, 0, ClickType::QuickCraft, SLOTS),
            Some(Click::QuickCraft(QuickCraft::Start {
                kind: DragKind::Left
            }))
        );
        assert_eq!(
            Click::parse(10, (1 << 2) | 1, ClickType::QuickCraft, SLOTS),
            Some(Click::QuickCraft(QuickCraft::AddSlot {
                slot: 10,
                kind: DragKind::Right
            }))
        );
        assert_eq!(
            Click::parse(-999, (2 << 2) | 2, ClickType::QuickCraft, SLOTS),
            Some(Click::QuickCraft(QuickCraft::End {
                kind: DragKind::Clone
            }))
        );
        // Kind 3 and phase 3 are invalid; AddSlot needs an in-range slot.
        assert_eq!(
            Click::parse(-999, 3 << 2, ClickType::QuickCraft, SLOTS),
            None
        );
        assert_eq!(Click::parse(-999, 3, ClickType::QuickCraft, SLOTS), None);
        assert_eq!(
            Click::parse(100, (1 << 2) | 1, ClickType::QuickCraft, SLOTS),
            None
        );
    }
}
