use abi_stable::abi_stability::check_layout_compatibility;
use abi_stable::pmr::TD_Opaque;
use abi_stable::std_types::RString;
use abi_stable::{RRef, StableAbi};
use libloading::Library;
use std::path::{Path, PathBuf};
use steel_api::{
    PLUGIN_ROOT_NAME, PLUGIN_ROOT_REPORT_NAME, Player, Player_TO, Plugin, PluginReport,
};
use thiserror::Error;

struct MyPlayer {
    name: String,
}

impl Player for MyPlayer {
    extern "C" fn get_name(&self) -> RString {
        RString::from(self.name.as_str())
    }
}

struct PluginBundle {
    _library: Library,
    plugin: Plugin,
}

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("library error {0}")]
    LibraryError(#[from] libloading::Error),
    #[error("invalid abi version")]
    InvalidAbiVersion,
    #[error("invalid plugin layout")]
    InvalidPluginLayout,
}

fn load_plugin(path: &Path) -> Result<PluginBundle, PluginError> {
    let library = unsafe { Library::new(path) }?;

    let plugin_report =
        unsafe { library.get::<extern "C" fn() -> PluginReport>(PLUGIN_ROOT_REPORT_NAME) }?;

    let plugin_report = plugin_report();
    if !plugin_report.abi_header.is_valid() {
        return Err(PluginError::InvalidAbiVersion);
    }

    if let Err(_err) =
        check_layout_compatibility(<Plugin as StableAbi>::LAYOUT, plugin_report.type_layout)
            .into_result()
    {
        return Err(PluginError::InvalidPluginLayout);
    }

    let plugin_root = unsafe { library.get::<extern "C" fn() -> Plugin>(PLUGIN_ROOT_NAME) }?;

    let plugin = plugin_root();
    Ok(PluginBundle {
        _library: library,
        plugin,
    })
}

fn main() {
    let path = PathBuf::from("../steel-plugin/target/debug/libsteel_plugin.so");

    let bundle = load_plugin(&path).expect("failed to load plugin");
    let plugin = bundle.plugin;

    let player = MyPlayer {
        name: "Steve".to_string(),
    };
    let to = Player_TO::from_ptr(RRef::new(&player), TD_Opaque);

    (plugin.on_enable)(to);
}
