#![expect(non_local_definitions)]

use crate::traits::{Player_TO, Server_TO};
use abi_stable::RRef;
use abi_stable::derive_macro_reexports::VersionStrings;

pub use abi_stable::{LIB_HEADER, StableAbi, sabi_types::VersionNumber, std_types::RString};
pub use steel_api_macros::export_plugin;

macro_rules! define_trait_with_arc_forward {
    (
        $(
            $(#[$trait_meta:meta])*
            $vis:vis trait $trait:ident {
                $(
                    $(#[$fn_meta:meta])*
                    fn $name:ident
                    (
                        &$self:ident
                        $(, $arg:ident : $arg_ty:ty )*
                    )
                    $(-> $ret:ty)?;
                )*
            }
        )+
    ) => {
        $(
            $(#[$trait_meta])*
            $vis trait $trait {
                $(
                    $(#[$fn_meta])*
                    fn $name(
                        &$self
                        $(, $arg : $arg_ty )*
                    )
                    $(-> $ret)?;
                )*
            }

            impl $trait for std::sync::Arc<dyn $trait> {
                $(
                    fn $name(
                        &$self
                        $(, $arg : $arg_ty )*
                    )
                    $(-> $ret)?
                    {
                        (**$self).$name($($arg),*)
                    }
                )*
            }
        )+
    };
}

mod report;
pub use report::PluginReport;

mod abi_types;
pub mod traits;

#[repr(C)]
#[derive(StableAbi)]
pub struct Plugin {
    pub name: RString,
    pub version: VersionStrings,
    pub on_enable: extern "C" fn(Context),
    pub on_disable: extern "C" fn(Context),
}

#[repr(C)]
#[derive(StableAbi)]
pub struct Context<'a> {
    pub server: Server_TO<'a, RRef<'a, ()>>,
    pub player: Player_TO<'a, RRef<'a, ()>>,
    pub plugin_data_folder: RString,
    // config, logger etc.
    // register commands
    // register events
}

impl<'a> Context<'a> {
    pub fn reborrow(&'a self) -> Context<'a> {
        Self {
            server: self.server.sabi_reborrow(),
            player: self.player.sabi_reborrow(),
            plugin_data_folder: self.plugin_data_folder.clone(),
        }
    }
}
