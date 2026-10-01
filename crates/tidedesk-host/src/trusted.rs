//! Viewers this host trusts: invited during a session, they come back
//! without the access code. Each is known by its certificate's fingerprint,
//! which only the viewer holding that certificate's key can show.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tidedesk_core::identity::normalize_fingerprint;
use tidedesk_core::paths;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trusted {
    pub fingerprint: String,
    /// The name it gave when it was trusted.
    pub name: String,
    /// When, as `YYYY-MM-DD`.
    pub since: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedViewers {
    #[serde(default, rename = "viewer")]
    viewers: Vec<Trusted>,
}

impl TrustedViewers {
    fn path() -> Result<PathBuf> {
        Ok(paths::config_dir()?.join("trusted-viewers.toml"))
    }

    pub fn load() -> Self {
        Self::path()
            .map(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path()?)
    }

    fn save_to(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?).context("saving trusted viewers")?;
        std::fs::rename(&tmp, path).context("saving trusted viewers")
    }

    pub fn list(&self) -> &[Trusted] {
        &self.viewers
    }

    pub fn trusts(&self, fingerprint: &str) -> bool {
        let fingerprint = normalize_fingerprint(fingerprint);
        self.viewers
            .iter()
            .any(|v| normalize_fingerprint(&v.fingerprint) == fingerprint)
    }

    /// Trusts a viewer; trusting one again only renames it.
    pub fn add(&mut self, fingerprint: &str, name: &str, today: &str) {
        let normalized = normalize_fingerprint(fingerprint);
        match self
            .viewers
            .iter_mut()
            .find(|v| normalize_fingerprint(&v.fingerprint) == normalized)
        {
            Some(known) => known.name = name.to_string(),
            None => self.viewers.push(Trusted {
                fingerprint: normalized,
                name: name.to_string(),
                since: today.to_string(),
            }),
        }
    }

    /// Stops trusting a viewer: whether it was trusted.
    pub fn remove(&mut self, fingerprint: &str) -> bool {
        let fingerprint = normalize_fingerprint(fingerprint);
        let before = self.viewers.len();
        self.viewers
            .retain(|v| normalize_fingerprint(&v.fingerprint) != fingerprint);
        self.viewers.len() != before
    }
}

/// Today's date as `YYYY-MM-DD`, in UTC.
pub fn today() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0) as i64;
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trusted_viewer_is_kept_until_removed() {
        let path =
            std::env::temp_dir().join(format!("tidedesk-trusted-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut trusted = TrustedViewers::load_from(&path);
        assert!(trusted.list().is_empty());

        trusted.add("ab12 cd34", "laptop", "2026-10-01");
        trusted.add("AB12CD34", "office laptop", "2026-10-02");
        assert_eq!(trusted.list().len(), 1, "the same viewer once");
        assert_eq!(trusted.list()[0].name, "office laptop");
        assert_eq!(trusted.list()[0].since, "2026-10-01");
        assert!(trusted.trusts("AB12 CD34"));
        assert!(!trusted.trusts("AB12 CD35"));

        trusted.save_to(&path).unwrap();
        let back = TrustedViewers::load_from(&path);
        assert_eq!(back, trusted);

        let mut back = back;
        assert!(back.remove("ab12cd34"));
        assert!(!back.remove("ab12cd34"));
        assert!(!back.trusts("AB12CD34"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn today_is_a_date() {
        let today = today();
        assert_eq!(today.len(), 10);
        assert!(today.as_str() >= "2026-01-01", "{today}");
    }
}
