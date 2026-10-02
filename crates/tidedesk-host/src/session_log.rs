//! The session log: one line per session in `sessions.csv`, in the
//! settings folder, when the licence turns it on. It stays on this
//! computer; nothing is sent anywhere.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

/// The licence feature that turns the log on.
pub const FEATURE: &str = "session-log";

const HEADER: &str =
    "started (UTC),ended (UTC),minutes,viewer,fingerprint,address,admitted by,ended because";

/// `sessions.csv` in the settings folder.
pub fn path() -> Result<PathBuf> {
    Ok(tidedesk_core::paths::config_dir()?.join("sessions.csv"))
}

/// Whether the licence turns the log on; looked up again every few
/// seconds, not on every frame.
pub fn on() -> bool {
    static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    match *cache {
        Some((at, on)) if at.elapsed() < Duration::from_secs(5) => on,
        _ => {
            let on = tidedesk_core::licence::allows(FEATURE);
            *cache = Some((Instant::now(), on));
            on
        }
    }
}

/// One session, as the log records it.
pub struct Entry {
    pub started: SystemTime,
    pub ended: SystemTime,
    pub viewer: String,
    pub fingerprint: Option<String>,
    pub address: String,
    pub admitted_by: &'static str,
    pub ended_because: String,
}

fn secs(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A CSV field: quoted when it holds a comma, a quote or a line break, and
/// never read by a spreadsheet as a formula (a viewer chooses its own name).
fn field(text: &str) -> String {
    let text = if text.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{text}")
    } else {
        text.to_string()
    };
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

fn line(entry: &Entry) -> String {
    let seconds = secs(entry.ended).saturating_sub(secs(entry.started));
    let fingerprint: String = entry
        .fingerprint
        .as_deref()
        .unwrap_or_default()
        .chars()
        .take(19)
        .collect();
    [
        tidedesk_core::dates::time(secs(entry.started)),
        tidedesk_core::dates::time(secs(entry.ended)),
        ((seconds + 30) / 60).to_string(),
        field(&entry.viewer),
        field(&fingerprint),
        field(&entry.address),
        field(entry.admitted_by),
        field(&entry.ended_because),
    ]
    .join(",")
}

/// Adds `entry` to the log at `path`, with the header first in a new log.
pub fn append(path: &Path, entry: &Entry) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("making the settings folder")?;
    }
    let new = !path.exists();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut text = String::new();
    if new {
        text.push_str(HEADER);
        text.push_str("\r\n");
    }
    text.push_str(&line(entry));
    text.push_str("\r\n");
    file.write_all(text.as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

/// Writes the session's line however the session ends; `None` when the
/// log is off.
pub struct Recorder(pub Option<Entry>);

impl Recorder {
    pub fn ended_because(&mut self, why: String) {
        if let Some(entry) = &mut self.0 {
            entry.ended_because = why;
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let Some(mut entry) = self.0.take() else {
            return;
        };
        entry.ended = SystemTime::now();
        if let Err(e) = path().and_then(|p| append(&p, &entry)) {
            tracing::warn!("could not write the session log: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Entry {
        let started = UNIX_EPOCH + Duration::from_secs(20_728 * 86_400 + 8 * 3600);
        Entry {
            started,
            ended: started + Duration::from_secs(12 * 60 + 40),
            viewer: "Ana's laptop".into(),
            fingerprint: Some("5179 2FA6 7B0B BD0F 5D52".into()),
            address: "192.168.1.50:51000".into(),
            admitted_by: "access code",
            ended_because: "the connection closed".into(),
        }
    }

    #[test]
    fn a_session_is_one_csv_line() {
        assert_eq!(
            line(&entry()),
            "2026-10-02 08:00:00,2026-10-02 08:12:40,13,Ana's laptop,5179 2FA6 7B0B BD0F,\
             192.168.1.50:51000,access code,the connection closed"
        );
        let odd = Entry {
            viewer: "=HYPERLINK(\"x\"), hi".into(),
            fingerprint: None,
            ended_because: "control stream: \"bye\"\nthen gone".into(),
            ..entry()
        };
        let text = line(&odd);
        assert!(text.contains(",\"'=HYPERLINK(\"\"x\"\"), hi\",,"), "{text}");
        assert!(
            text.ends_with(",\"control stream: \"\"bye\"\"\nthen gone\""),
            "{text}"
        );
    }

    #[test]
    fn the_log_gets_its_header_once() {
        let dir = std::env::temp_dir().join(format!("tidedesk-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("sessions.csv");
        append(&path, &entry()).unwrap();
        append(&path, &entry()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], HEADER);
        assert_eq!(lines[1], lines[2]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
