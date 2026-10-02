//! TideDesk's log file, in the settings folder: why a session ended and
//! what else went wrong, written down instead of shown on screen. It stays
//! small: past [`MAX_BYTES`] it is moved aside once, and the one before
//! that is dropped.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// `tidedesk.log` in the settings folder.
pub fn path() -> Result<PathBuf> {
    let dir = crate::paths::config_dir()?;
    std::fs::create_dir_all(&dir).context("making the settings folder")?;
    Ok(dir.join("tidedesk.log"))
}

/// Opens the log to add to it; one grown past [`MAX_BYTES`] becomes
/// `tidedesk.old.log` first, replacing the one there.
pub fn open(path: &Path) -> Result<File> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::rename(path, path.with_extension("old.log"));
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn the_log_is_added_to_and_kept_small() {
        let dir = std::env::temp_dir().join(format!("tidedesk-logs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tidedesk.log");

        writeln!(open(&path).unwrap(), "first").unwrap();
        writeln!(open(&path).unwrap(), "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");

        let big = vec![b'x'; MAX_BYTES as usize + 1];
        std::fs::write(&path, &big).unwrap();
        writeln!(open(&path).unwrap(), "after").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after\n");
        let old = dir.join("tidedesk.old.log");
        assert_eq!(std::fs::metadata(&old).unwrap().len(), big.len() as u64);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
