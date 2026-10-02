//! Records every session in the session history
//! ([`tidedesk_core::history`]) however it ends, and says whether the
//! licence shows the full history.

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tidedesk_core::history::{self, Record};

/// Whether the licence shows the full history; looked up again every few
/// seconds, not on every frame.
pub fn full() -> bool {
    static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    match *cache {
        Some((at, full)) if at.elapsed() < Duration::from_secs(5) => full,
        _ => {
            let full = tidedesk_core::licence::allows(history::FEATURE);
            *cache = Some((Instant::now(), full));
            full
        }
    }
}

/// One session, as it is being recorded.
pub struct Entry {
    pub started: SystemTime,
    pub ended: SystemTime,
    pub viewer: String,
    pub fingerprint: Option<String>,
    pub address: String,
    pub admitted_by: &'static str,
    pub ended_because: String,
}

pub fn secs(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Entry {
    fn record(self) -> Record {
        Record {
            started: secs(self.started),
            ended: secs(self.ended),
            viewer: self.viewer,
            // The first four groups, as the device ID shows them.
            fingerprint: self
                .fingerprint
                .unwrap_or_default()
                .chars()
                .take(19)
                .collect(),
            address: self.address,
            admitted_by: self.admitted_by.into(),
            ended_because: self.ended_because,
        }
    }
}

/// Writes the session to the history however the session ends.
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
        if let Err(e) = history::append(entry.record()) {
            tracing::warn!("could not record the session in the history: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_becomes_a_record() {
        let started = UNIX_EPOCH + Duration::from_secs(20_728 * 86_400 + 8 * 3600);
        let entry = Entry {
            started,
            ended: started + Duration::from_secs(12 * 60 + 40),
            viewer: "Ana's laptop".into(),
            fingerprint: Some("5179 2FA6 7B0B BD0F 5D52 A39D".into()),
            address: "192.168.1.50:51000".into(),
            admitted_by: "access code",
            ended_because: "the connection closed".into(),
        };
        let record = entry.record();
        assert_eq!(record.fingerprint, "5179 2FA6 7B0B BD0F");
        assert_eq!(record.minutes(), 13);
        assert_eq!(record.started, 20_728 * 86_400 + 8 * 3600);
    }
}
