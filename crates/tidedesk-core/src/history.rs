//! The session history: who connected to this computer, when and how,
//! kept in `sessions.history` in the settings folder. Each session is one
//! line, sealed for this Windows account ([`crate::secret`]) and carrying a
//! hash of the line before it, so that an edited or removed line shows.
//! Everyone sees the last [`FREE_DAYS`]; the full history, search and export
//! come with a licence that includes [`FEATURE`]. Nothing is sent anywhere.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};

/// The licence feature that shows the full history.
pub const FEATURE: &str = "session-log";
/// Days of history everyone sees.
pub const FREE_DAYS: u64 = 30;

/// One session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// Seconds since 1970, UTC.
    pub started: u64,
    pub ended: u64,
    pub viewer: String,
    /// The start of the viewer's certificate fingerprint, when it showed one.
    pub fingerprint: String,
    pub address: String,
    /// "access code", "saved password" or "trusted viewer".
    pub admitted_by: String,
    pub ended_because: String,
}

impl Record {
    pub fn minutes(&self) -> u64 {
        (self.ended.saturating_sub(self.started) + 30) / 60
    }
}

#[derive(Serialize, Deserialize)]
struct Sealed {
    record: Record,
    /// SHA-256 of the line before, or zeros for the first.
    previous: [u8; 32],
}

/// `sessions.history` in the settings folder.
pub fn path() -> Result<PathBuf> {
    Ok(crate::paths::config_dir()?.join("sessions.history"))
}

fn hash(line: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(digest(&SHA256, line.as_bytes()).as_ref());
    out
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Adds a session to the history at `path`.
pub fn append_to(path: &Path, record: Record) -> Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let previous = existing
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map_or([0u8; 32], hash);
    let plain = postcard::to_stdvec(&Sealed { record, previous })?;
    let line = to_hex(&crate::secret::protect(&plain)?);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("making the settings folder")?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("writing {}", path.display()))
}

/// Adds a session to this computer's history.
pub fn append(record: Record) -> Result<()> {
    append_to(&path()?, record)
}

/// What the history holds, oldest first.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct History {
    pub records: Vec<Record>,
    /// A line was changed, removed or cannot be read: the history was
    /// touched outside TideDesk.
    pub changed: bool,
}

pub fn read_from(path: &Path) -> History {
    let Ok(text) = std::fs::read_to_string(path) else {
        return History::default();
    };
    let mut history = History::default();
    let mut previous = [0u8; 32];
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let sealed = from_hex(line)
            .and_then(|bytes| crate::secret::unprotect(&bytes))
            .and_then(|plain| postcard::from_bytes::<Sealed>(&plain).ok());
        match sealed {
            Some(sealed) => {
                if sealed.previous != previous {
                    history.changed = true;
                }
                history.records.push(sealed.record);
            }
            None => history.changed = true,
        }
        previous = hash(line);
    }
    history
}

/// This computer's history.
pub fn read() -> History {
    path().map(|p| read_from(&p)).unwrap_or_default()
}

/// What may be shown at `now` (seconds since 1970): everything with the
/// full history, otherwise the last [`FREE_DAYS`]; and how many older
/// sessions are left out.
pub fn shown(records: &[Record], now: u64, full: bool) -> (Vec<&Record>, usize) {
    let since = now.saturating_sub(FREE_DAYS * 86_400);
    let (recent, older): (Vec<&Record>, Vec<&Record>) =
        records.iter().partition(|r| full || r.ended >= since);
    (recent, older.len())
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

pub const CSV_HEADER: &str =
    "started (UTC),ended (UTC),minutes,viewer,fingerprint,address,admitted by,ended because";

/// One session as a CSV line.
pub fn csv_line(record: &Record) -> String {
    [
        crate::dates::time(record.started),
        crate::dates::time(record.ended),
        record.minutes().to_string(),
        field(&record.viewer),
        field(&record.fingerprint),
        field(&record.address),
        field(&record.admitted_by),
        field(&record.ended_because),
    ]
    .join(",")
}

/// Sessions as a CSV file's text, with its header.
pub fn csv<'a>(records: impl IntoIterator<Item = &'a Record>) -> String {
    let mut text = format!("{CSV_HEADER}\r\n");
    for record in records {
        text.push_str(&csv_line(record));
        text.push_str("\r\n");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(started: u64) -> Record {
        Record {
            started,
            ended: started + 12 * 60 + 40,
            viewer: "Ana's laptop".into(),
            fingerprint: "5179 2FA6 7B0B BD0F".into(),
            address: "192.168.1.50:51000".into(),
            admitted_by: "access code".into(),
            ended_because: "the connection closed".into(),
        }
    }

    const DAY: u64 = 86_400;
    const NOW: u64 = 20_728 * DAY;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tidedesk-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("sessions.history")
    }

    #[cfg(windows)]
    #[test]
    fn the_history_reads_back_sealed_and_shows_tampering() {
        let path = temp("seal");
        for day in [40, 10, 1] {
            append_to(&path, record(NOW - day * DAY)).unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("Ana"), "sealed, not readable as text");
        let history = read_from(&path);
        assert!(!history.changed);
        assert_eq!(history.records.len(), 3);
        assert_eq!(history.records[0], record(NOW - 40 * DAY));

        // A line taken out breaks the chain.
        let lines: Vec<&str> = text.lines().collect();
        std::fs::write(&path, format!("{}\n{}\n", lines[0], lines[2])).unwrap();
        let history = read_from(&path);
        assert!(history.changed);
        assert_eq!(history.records.len(), 2);

        // A line changed cannot be read.
        std::fs::write(&path, format!("{}\n00{}\n", lines[0], &lines[1][2..])).unwrap();
        assert!(read_from(&path).changed);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn everyone_sees_thirty_days_and_a_licence_all() {
        let records: Vec<Record> = [40, 31, 29, 1]
            .iter()
            .map(|d| record(NOW - d * DAY))
            .collect();
        let (recent, older) = shown(&records, NOW, false);
        assert_eq!((recent.len(), older), (2, 2));
        assert_eq!(recent[0], &records[2]);
        let (all, older) = shown(&records, NOW, true);
        assert_eq!((all.len(), older), (4, 0));
        assert_eq!(read_from(Path::new("no such file")), History::default());
    }

    #[test]
    fn sessions_export_as_csv() {
        let start = NOW + 8 * 3600;
        assert_eq!(
            csv_line(&record(start)),
            "2026-10-02 08:00:00,2026-10-02 08:12:40,13,Ana's laptop,5179 2FA6 7B0B BD0F,\
             192.168.1.50:51000,access code,the connection closed"
        );
        let odd = Record {
            viewer: "=HYPERLINK(\"x\"), hi".into(),
            fingerprint: String::new(),
            ended_because: "control stream: \"bye\"\nthen gone".into(),
            ..record(start)
        };
        let text = csv_line(&odd);
        assert!(text.contains(",\"'=HYPERLINK(\"\"x\"\"), hi\",,"), "{text}");
        assert!(
            text.ends_with(",\"control stream: \"\"bye\"\"\nthen gone\""),
            "{text}"
        );
        assert!(csv([&odd]).starts_with(CSV_HEADER));
    }
}
