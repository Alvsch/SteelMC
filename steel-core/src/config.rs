//! Server configuration types used at runtime.
//!
//! The full deserialization struct lives in the `steel` crate. Steel-core only
//! defines `RuntimeConfig` (the subset kept after startup) and the domain types
//! that both crates share.

use serde::Deserialize;
pub use steel_protocol::packet_traits::CompressionInfo;
use steel_protocol::packets::config::{CServerLinks, Link, ServerLinksType};
use steel_utils::codec::Or;
use text_components::TextComponent;

/// Runtime server configuration — the subset of settings needed after startup.
///
/// Stored on `Server` and accessed by game logic at runtime.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// The maximum number of players that can be on the server at once.
    pub max_players: u32,
    /// The view distance of the server.
    pub view_distance: u8,
    /// The simulation distance of the server.
    pub simulation_distance: u8,
    /// Whether the server is in online mode.
    pub online_mode: bool,
    /// Whether the server should use encryption.
    pub encryption: bool,
    /// The message of the day.
    pub motd: String,
    /// Whether to use a favicon.
    pub use_favicon: bool,
    /// The path to the favicon.
    pub favicon: String,
    /// Whether to enforce secure chat.
    pub enforce_secure_chat: bool,
    /// Whether the world generator produces flat worlds (for `is_flat` in packets).
    pub is_flat: bool,
    /// The compression settings for the server.
    pub compression: Option<CompressionInfo>,
    /// All settings and configurations for server links.
    pub server_links: Option<ServerLinks>,
}

impl RuntimeConfig {
    /// Builds the `CServerLinks` packet from config, if server links are enabled.
    #[must_use]
    pub fn server_links_packet(&self) -> Option<CServerLinks> {
        let server_links = self.server_links.as_ref()?;

        if !server_links.enable || server_links.links.is_empty() {
            return None;
        }

        let links: Vec<Link> = server_links
            .links
            .iter()
            .map(|config_link| {
                let label = match &config_link.label {
                    ConfigLabel::BuiltIn(link_type) => Or::Left(*link_type),
                    ConfigLabel::Custom(text_component) => Or::Right(text_component.clone()),
                };
                Link::new(label, config_link.url.clone())
            })
            .collect();

        Some(CServerLinks { links })
    }
}

/// Label type for server links — either built-in string or custom `TextComponent`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
#[expect(
    clippy::large_enum_variant,
    reason = "TextComponent variant is common; boxing would add indirection for every use"
)]
pub enum ConfigLabel {
    /// Built-in server link type (e.g., "`bug_report`", "website")
    BuiltIn(ServerLinksType),
    /// Custom text component with formatting
    Custom(TextComponent),
}

/// A single server link configuration entry.
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigLink {
    /// The label for this link (built-in type or custom `TextComponent`)
    pub label: ConfigLabel,
    /// The URL for this link
    pub url: String,
}

/// Server links configuration.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ServerLinks {
    /// Enable the server links feature
    pub enable: bool,
    /// List of server links to display
    #[serde(default)]
    pub links: Vec<ConfigLink>,
}

/// The different types of world generators that can be used.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldGeneratorTypes {
    /// produces a flat gras world
    Flat,
    /// creates an empty world which can be used for test
    Empty,
    /// generates vanilla terrain with noise-based biomes
    Vanilla,
}

/// Configuration for world storage.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldStorageConfig {
    /// Standard disk persistence using region files.
    Disk {
        /// Path to the world directory (e.g., "world/overworld").
        path: String,
    },
    /// RAM-only storage with empty chunks created on demand.
    /// No data is persisted — useful for testing and minigames.
    RamOnly,
}
