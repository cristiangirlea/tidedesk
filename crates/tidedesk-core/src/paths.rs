//! Per-user configuration directory.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// `%APPDATA%\TideDesk` on Windows, `$XDG_CONFIG_HOME/tidedesk` (or
/// `~/.config/tidedesk`) elsewhere. Created on first use.
pub fn config_dir() -> Result<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("TideDesk"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|p| p.join("tidedesk"))
    };
    let dir = base.context("cannot locate a per-user configuration directory")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}
