use abi_stable::std_types::RString;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, Level};
use steel_api::traits::{Player, Server};
use steel_loader::PluginLoader;

struct MyPlayer;

impl Player for MyPlayer {
    fn get_name(&self) -> RString {
        RString::from("Steve")
    }
}

struct MyServer;
impl Server for MyServer {}

fn main() {
    tracing_subscriber::fmt()
        .with_target(true)
        .with_level(true)
        .with_max_level(Level::TRACE)
        .init();

    let plugin_dir = PathBuf::from("../steel-plugin")
        .canonicalize()
        .expect("failed");
    let plugin_lib = plugin_dir
        .join("target/debug/libsteel_plugin.so")
        .canonicalize()
        .expect("failed");

    let mut loader = PluginLoader::new(Arc::new(MyServer), Arc::new(MyPlayer), plugin_dir.clone());

    info!("loading plugin");
    loader.load(&plugin_lib);
    info!("loaded plugin");

}
