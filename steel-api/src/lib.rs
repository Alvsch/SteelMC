#![expect(non_local_definitions)]

// External dependencies
pub use abi_stable::{LIB_HEADER, StableAbi, sabi_types::VersionNumber, std_types::RString};
use abi_stable::{RRef, sabi_trait};
pub use steel_api_macros::export_plugin;

mod plugin;
mod report;

pub use plugin::Plugin;
pub use report::PluginReport;

pub const PLUGIN_ROOT_NAME: &[u8] = b"__plugin_root";
pub const PLUGIN_ROOT_REPORT_NAME: &[u8] = b"__plugin_root__report";

#[sabi_trait]
pub trait Player {
    extern "C" fn get_name(&self) -> RString;
}

pub type PlayerRef<'a> = Player_TO<'a, RRef<'a, ()>>;
