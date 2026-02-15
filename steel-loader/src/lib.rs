use abi_stable::abi_stability::check_layout_compatibility;
use abi_stable::derive_macro_reexports::TD_Opaque;
use abi_stable::{RRef, StableAbi};
use libloading::Library;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use steel_api::traits::{Player, Player_TO, Server, Server_TO};
use steel_api::{Plugin, PluginReport};
use thiserror::Error;

const PLUGIN_ROOT_NAME: &[u8] = b"__plugin_root";
const PLUGIN_ROOT_REPORT_NAME: &[u8] = b"__plugin_root__report";

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("library error {0}")]
    LibraryError(#[from] libloading::Error),
    #[error("invalid abi version")]
    InvalidAbiVersion,
    #[error("invalid plugin layout")]
    InvalidPluginLayout,
}

pub struct PluginBundle {
    pub plugin: Plugin,
    _library: Library,
}

pub struct PluginLoader {
    server: Arc<dyn Server>,
    player: Arc<dyn Player>,
    path: PathBuf,
    plugins: FxHashMap<String, PluginBundle>,
}

impl PluginLoader {
    pub fn new(server: Arc<dyn Server>, player: Arc<dyn Player>, path: PathBuf) -> Self {
        Self {
            server,
            player,
            path,
            plugins: FxHashMap::default(),
        }
    }

    pub fn load(&mut self, path: &Path) {
        let bundle = load_plugin(path).unwrap();
        let name = bundle.plugin.name.to_string();

        if self.plugins.contains_key(&name) {
            panic!("plugin already exists");
        }

        self.plugins.insert(name.clone(), bundle);
        let plugin = &self.plugins.get(&name).unwrap().plugin;

        let ctx = self.create_context(plugin);
        (plugin.on_enable)(ctx);
    }

    pub fn get_plugin(&self, name: &str) -> &Plugin {
        &self.plugins.get(name).unwrap().plugin
    }

    pub fn create_context(&self, plugin: &Plugin) -> steel_api::Context<'_> {
        steel_api::Context {
            server: Server_TO::from_ptr(RRef::new(&self.server), TD_Opaque),
            player: Player_TO::from_ptr(RRef::new(&self.player), TD_Opaque),
            plugin_data_folder: self
                .path
                .join(plugin.name.as_str())
                .into_os_string()
                .into_string()
                .expect("failed")
                .into(),
        }
    }
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
        plugin,
        _library: library,
    })
}
