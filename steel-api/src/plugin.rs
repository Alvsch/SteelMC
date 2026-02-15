use crate::{PlayerRef, StableAbi};
use abi_stable::sabi_types::VersionStrings;
use abi_stable::std_types::RString;

#[repr(C)]
#[derive(StableAbi)]
pub struct Plugin {
    pub name: RString,
    pub version: VersionStrings,
    pub on_enable: extern "C" fn(PlayerRef),
}
