#![expect(missing_docs)]

use std::path::PathBuf;
use stabby::dynptr;
use stabby::libloading::StabbyLibrary;
use stabby::sync::Arc;
use steel_api::{Player, Plugin};

struct MyPlayer;

impl Player for MyPlayer {
    extern "C" fn greet(&self) {
        println!("hello world");
    }
}

fn main() {
    let path = PathBuf::from(
        "/home/alvsch/Programming/Projects/SteelMC/steel-plugin/target/debug/libsteel_plugin.so",
    );

    let lib = unsafe { libloading::Library::new(path) }.unwrap();

    let plugin_root =
        unsafe { lib.get_stabbied::<extern "C" fn() -> Plugin>(b"plugin_root") }.unwrap();
    
    let plugin = plugin_root();

    let player = Arc::new(MyPlayer);
    let ptr: dynptr!(Arc<dyn Player>) = player.into();

    (plugin.on_enable)(ptr);
}
