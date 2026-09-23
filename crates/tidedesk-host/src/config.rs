//! Saved host settings (`host.toml` in the config directory).

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tidedesk_core::nat::stun::DEFAULT_STUN_SERVERS;
use tidedesk_core::{DEFAULT_PORT, paths};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostConfig {
    /// Display index to share.
    pub display: usize,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub share_audio: bool,
    pub allow_clipboard: bool,
    pub allow_mouse: bool,
    /// UDP port; takes effect on the next start.
    pub port: u16,
    /// Show the host window's button in the taskbar. Off: tray icon only.
    pub show_in_taskbar: bool,
    /// Start with the window hidden in the tray.
    pub start_in_tray: bool,
    /// Ask STUN servers for this computer's internet address.
    pub discover_public_address: bool,
    /// `host[:port]` of the STUN servers to ask; empty means the defaults.
    pub stun_servers: Vec<String>,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            display: 0,
            fps: 30,
            bitrate_kbps: 4000,
            share_audio: true,
            allow_clipboard: false,
            allow_mouse: true,
            port: DEFAULT_PORT,
            show_in_taskbar: false,
            start_in_tray: false,
            discover_public_address: true,
            stun_servers: default_stun_servers(),
        }
    }
}

impl HostConfig {
    pub const FPS_RANGE: std::ops::RangeInclusive<u32> = 5..=60;
    pub const BITRATE_RANGE: std::ops::RangeInclusive<u32> = 500..=20_000;

    fn path() -> Result<PathBuf> {
        Ok(paths::config_dir()?.join("host.toml"))
    }

    /// Loads saved settings; a missing or unreadable file yields the defaults.
    pub fn load() -> Self {
        let Ok(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("ignoring invalid {}: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
        .clamped()
    }

    pub fn save(&self) -> Result<()> {
        let text = toml::to_string_pretty(self)?;
        std::fs::write(Self::path()?, text).context("saving host settings")
    }

    fn clamped(mut self) -> Self {
        self.fps = self
            .fps
            .clamp(*Self::FPS_RANGE.start(), *Self::FPS_RANGE.end());
        self.bitrate_kbps = self
            .bitrate_kbps
            .clamp(*Self::BITRATE_RANGE.start(), *Self::BITRATE_RANGE.end());
        self.stun_servers = stun_servers_or_defaults(self.stun_servers);
        self
    }
}

fn default_stun_servers() -> Vec<String> {
    DEFAULT_STUN_SERVERS.iter().map(|s| s.to_string()).collect()
}

/// Reads a comma- or space-separated list of STUN servers as typed in the
/// settings; an empty list means the defaults.
pub fn parse_stun_servers(text: &str) -> Vec<String> {
    stun_servers_or_defaults(text.split([',', ' ']).map(String::from).collect())
}

fn stun_servers_or_defaults(servers: Vec<String>) -> Vec<String> {
    let servers: Vec<String> = servers
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if servers.is_empty() {
        default_stun_servers()
    } else {
        servers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_files_fill_in_defaults_and_values_are_clamped() {
        let cfg: HostConfig = toml::from_str("fps = 500\nshow_in_taskbar = true").unwrap();
        let cfg = cfg.clamped();
        assert_eq!(cfg.fps, 60);
        assert!(cfg.show_in_taskbar);
        assert_eq!(cfg.port, DEFAULT_PORT);
    }

    #[test]
    fn defaults_enable_discovery_with_two_stun_servers() {
        let cfg = HostConfig::default();
        assert!(cfg.discover_public_address);
        assert_eq!(cfg.stun_servers, DEFAULT_STUN_SERVERS);
        assert_eq!(cfg.stun_servers.len(), 2);
        // Settings saved by older versions gain the new fields.
        let old: HostConfig = toml::from_str(
            "fps = 30
port = 47800",
        )
        .unwrap();
        assert_eq!(old.clamped().stun_servers, DEFAULT_STUN_SERVERS);
    }

    #[test]
    fn empty_stun_list_falls_back_to_defaults() {
        let blank: HostConfig = toml::from_str("stun_servers = [\" \", \"\"]").unwrap();
        assert_eq!(blank.clamped().stun_servers, DEFAULT_STUN_SERVERS);
        assert_eq!(parse_stun_servers("  , "), DEFAULT_STUN_SERVERS);
        assert_eq!(
            parse_stun_servers(" stun.example.org, 192.0.2.1:3478 ,other"),
            ["stun.example.org", "192.0.2.1:3478", "other"]
        );
    }

    #[test]
    fn round_trips_through_toml() {
        let cfg = HostConfig {
            display: 1,
            start_in_tray: true,
            ..Default::default()
        };
        let back: HostConfig = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(back, cfg);
    }
}
