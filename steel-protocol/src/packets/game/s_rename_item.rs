//! Serverbound packet for renaming the item inside of an anvil's first slot

use steel_macros::{ReadFrom, ServerPacket};

/// Sent by the client when the player changes their selected hotbar slot.
#[derive(ServerPacket, ReadFrom, Clone, Debug)]
pub struct SRenameItem {
    /// The new name
    #[read(as = Prefixed(VarInt), bound = 32500)]
    pub name: String,
}
