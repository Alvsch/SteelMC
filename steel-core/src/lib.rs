//! # Steel Core
//!
//! The core library for the Steel Minecraft server. Handles everything related to the PLAY state.

use std::sync::{Arc, OnceLock};

use flume::{Receiver, Sender};

use crate::{chunk::chunk_map::ChunkMap, player::Player};

pub mod behavior;
pub mod block_entity;
pub mod chunk;
pub mod chunk_saver;
pub mod command;
pub mod config;
pub mod entity;
pub mod fluid;
pub mod inventory;
pub mod level_data;
pub mod physics;
pub mod player;
pub mod poi;
pub(crate) mod portal;
pub mod server;
pub mod world;
pub mod worldgen;

static PLUGIN_API: OnceLock<Sender<PluginApi>> = OnceLock::new();

pub(crate) fn plugin_api_send(event: PluginApi) {
    if let Some(plugin_api) = PLUGIN_API.get() {
        plugin_api.send(event).expect("channel is disconnected");
    }
}

/// Events emitted by the core server for plugins.
#[expect(missing_docs, reason = "variant names are self-explanatory")]
#[derive(Clone)]
pub enum PluginApi {
    PlayerJoinEvent(Arc<Player>),
    PlayerLeaveEvent(Arc<Player>),
}

/// Initialize the plugin api and get a receiver to all plugin api events
///
/// # Panics
/// Panics if the plugin api is already initialized
pub fn init_api() -> Receiver<PluginApi> {
    let (tx, rx) = flume::bounded(16);
    PLUGIN_API
        .set(tx)
        .expect("plugin api can only be initialized once");
    rx
}
