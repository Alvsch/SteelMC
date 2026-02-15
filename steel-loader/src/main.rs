use abi_stable::abi_stability::check_layout_compatibility;
use abi_stable::pmr::TD_Opaque;
use abi_stable::std_types::RString;
use abi_stable::{RRef, StableAbi};
use libloading::Library;
use std::path::{Path, PathBuf};
use steel_api::{
    PLUGIN_ROOT_NAME, PLUGIN_ROOT_REPORT_NAME, Player, Player_TO, Plugin, PluginReport,
};

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

fn load_plugin(path: &Path) -> PluginBundle {
    let library = unsafe { Library::new(path) }.unwrap();

    let plugin_report =
        unsafe { library.get::<extern "C" fn() -> PluginReport>(PLUGIN_ROOT_REPORT_NAME) }.unwrap();

    let plugin_report = plugin_report();
    if !plugin_report.abi_header.is_valid() {
        panic!("invalid abi version");
    }

    if let Err(_err) =
        check_layout_compatibility(<Plugin as StableAbi>::LAYOUT, plugin_report.type_layout)
            .into_result()
    {
        panic!("invalid plugin layout");
    }

    let plugin_root =
        unsafe { library.get::<extern "C" fn() -> Plugin>(PLUGIN_ROOT_NAME) }.unwrap();

    let plugin = plugin_root();
    PluginBundle {
        _library: library,
        plugin,
    }
}

fn main() {
    let path = PathBuf::from("../steel-plugin/target/debug/libsteel_plugin.so");

    let bundle = load_plugin(&path);
    let plugin = bundle.plugin;

    let player = MyPlayer {
        name: "Steve".to_string(),
    };
    let to = Player_TO::from_ptr(RRef::new(&player), TD_Opaque);

    (plugin.on_enable)(to);
}
