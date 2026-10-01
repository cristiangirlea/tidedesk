//! The host's saved password, kept as the key derived from it (see
//! [`tidedesk_core::password`]), sealed for this Windows account. The
//! password itself is never written anywhere.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tidedesk_core::password::{self, Key};
use tidedesk_core::{paths, secret};

fn path() -> Result<PathBuf> {
    Ok(paths::config_dir()?.join("password.key"))
}

/// The saved password's key, if there is one.
pub fn load() -> Option<Key> {
    load_from(&path().ok()?)
}

/// Saves `password` for the host with this certificate fingerprint: its key.
pub fn save(password: &str, fingerprint: &str) -> Result<Key> {
    save_to(&path()?, password, fingerprint)
}

pub fn remove() -> Result<()> {
    remove_at(&path()?)
}

fn load_from(path: &Path) -> Option<Key> {
    let sealed = std::fs::read(path).ok()?;
    secret::unprotect(&sealed)?.try_into().ok()
}

fn save_to(path: &Path, password: &str, fingerprint: &str) -> Result<Key> {
    if let Some(problem) = password::problem(password) {
        bail!("{problem}");
    }
    let key = password::derive_key(password, fingerprint)?;
    let sealed = secret::protect(&key)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, sealed).context("saving the password")?;
    std::fs::rename(&tmp, path).context("saving the password")?;
    Ok(key)
}

fn remove_at(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).context("removing the password")
        }
        _ => Ok(()),
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_saved_password_is_kept_as_its_key_only() {
        let path =
            std::env::temp_dir().join(format!("tidedesk-password-{}.key", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_from(&path), None);

        let why = save_to(&path, "short", "AAAA").unwrap_err().to_string();
        assert!(why.contains("12 characters"), "{why}");
        assert!(!path.exists());

        let key = save_to(&path, "correct horse battery", "AAAA").unwrap();
        assert_eq!(load_from(&path), Some(key));
        let file = std::fs::read(&path).unwrap();
        assert!(
            !file.windows(7).any(|w| w == b"correct"),
            "the password is not in the file"
        );
        assert!(
            !file.windows(32).any(|w| w == key),
            "nor the key in the clear"
        );

        remove_at(&path).unwrap();
        assert_eq!(load_from(&path), None);
        remove_at(&path).unwrap();
    }
}
