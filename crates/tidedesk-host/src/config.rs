//! Saved host settings (`host.toml` in the config directory).

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tidedesk_core::{DEFAULT_PORT, paths};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostConfig {
    /// Display index to share.
    pub display: usize,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub share_audio: bool,
    /// UDP port; takes effect on the next start.
    pub port: u16,
    /// Show the host window's button in the taskbar. Off: tray icon only.
    pub show_in_taskbar: bool,
    /// Start with the window hidden in the tray.
    pub start_in_tray: bool,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            display: 0,
            fps: 30,
            bitrate_kbps: 4000,
            share_audio: true,
            port: DEFAULT_PORT,
            show_in_taskbar: false,
            start_in_tray: false,
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
        self
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
