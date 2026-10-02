//! Records every session in the session history
//! ([`tidedesk_core::history`]) however it ends, tells a [`SessionSink`]
//! when one is set, and says whether the licence shows the full history.

use std::sync::{Arc, Mutex, RwLock};
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
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Told when a session starts and when it ends, for a program built on
/// TideDesk that keeps its own record of sessions. Every session that
/// starts also ends, one that could not start fully included. Called on
/// the session's own task: keep it short.
pub trait SessionSink: Send + Sync {
    fn started(&self, session: &Entry);
    /// `session.ended` and `session.ended_because` are filled in.
    fn ended(&self, session: &Entry);
}

static SINK: RwLock<Option<Arc<dyn SessionSink>>> = RwLock::new(None);

/// Sets the sink told of every session from now on, replacing any earlier one.
pub fn set_sink(sink: Arc<dyn SessionSink>) {
    *SINK.write().unwrap() = Some(sink);
}

fn sink() -> Option<Arc<dyn SessionSink>> {
    SINK.read().unwrap().clone()
}

/// Writes the session to the history however the session ends, and tells
/// the sink.
pub struct Recorder {
    entry: Option<Entry>,
    /// Whether the history is written; tests leave the real one alone.
    history: bool,
}

impl Recorder {
    /// A session starts: the sink hears of it now, the history at the end.
    pub fn start(entry: Entry) -> Self {
        if let Some(sink) = sink() {
            sink.started(&entry);
        }
        Recorder {
            entry: Some(entry),
            history: true,
        }
    }

    pub fn ended_because(&mut self, why: String) {
        if let Some(entry) = &mut self.entry {
            entry.ended_because = why;
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let Some(mut entry) = self.entry.take() else {
            return;
        };
        entry.ended = SystemTime::now();
        if let Some(sink) = sink() {
            sink.ended(&entry);
        }
        if self.history
            && let Err(e) = history::append(entry.record())
        {
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

    /// Writes down everything it is told. Set once for all the tests,
    /// which run at the same time: each reads back its own viewer's sessions.
    #[derive(Default)]
    struct Heard(Mutex<Vec<(&'static str, Entry)>>);

    impl SessionSink for Heard {
        fn started(&self, session: &Entry) {
            self.0.lock().unwrap().push(("started", session.clone()));
        }
        fn ended(&self, session: &Entry) {
            self.0.lock().unwrap().push(("ended", session.clone()));
        }
    }

    /// What the sink was told about `viewer`'s sessions.
    fn told(viewer: &str) -> Vec<(&'static str, Entry)> {
        static HEARD: std::sync::OnceLock<Arc<Heard>> = std::sync::OnceLock::new();
        let heard = HEARD.get_or_init(|| {
            let heard = Arc::new(Heard::default());
            set_sink(heard.clone());
            heard
        });
        let all = heard.0.lock().unwrap();
        all.iter()
            .filter(|(_, e)| e.viewer == viewer)
            .cloned()
            .collect()
    }

    fn entry(viewer: &str) -> Entry {
        let now = SystemTime::now();
        Entry {
            started: now,
            ended: now,
            viewer: viewer.into(),
            fingerprint: Some("5179 2FA6 7B0B BD0F".into()),
            address: "192.168.1.50:51000".into(),
            admitted_by: "access code",
            ended_because: "the session could not start".into(),
        }
    }

    fn recorder(entry: Entry) -> Recorder {
        let mut recorder = Recorder::start(entry);
        recorder.history = false;
        recorder
    }

    #[test]
    fn a_sink_hears_each_session_start_and_end_once() {
        let viewer = "sink test: a session";
        told(viewer);
        let mut session = recorder(entry(viewer));
        assert_eq!(told(viewer).len(), 1, "told at the start");
        std::thread::sleep(Duration::from_millis(20));
        session.ended_because("the viewer closed the session".into());
        drop(session);

        let told = told(viewer);
        let what: Vec<_> = told.iter().map(|(what, _)| *what).collect();
        assert_eq!(what, ["started", "ended"]);
        let ended = &told[1].1;
        assert_eq!(ended.ended_because, "the viewer closed the session");
        assert!(ended.ended > ended.started, "the end time is filled in");
        assert_eq!(ended.admitted_by, "access code");
    }

    #[test]
    fn a_session_that_could_not_start_still_ends() {
        let viewer = "sink test: no start";
        told(viewer);
        drop(recorder(entry(viewer)));
        let told = told(viewer);
        assert_eq!(told.len(), 2);
        assert_eq!(told[1].1.ended_because, "the session could not start");
    }
}
