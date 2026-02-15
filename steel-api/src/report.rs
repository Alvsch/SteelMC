use crate::StableAbi;
use abi_stable::derive_macro_reexports::TypeLayout;
use abi_stable::library::AbiHeader;

#[repr(C)]
#[derive(Debug, StableAbi)]
pub struct PluginReport {
    pub abi_header: AbiHeader,
    pub type_layout: &'static TypeLayout,
}
