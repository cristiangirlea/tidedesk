//! The host's access code, and the one before it for a few minutes after a
//! session ended: a viewer whose connection dropped comes straight back,
//! anyone later needs the new code.

use std::time::{Duration, Instant};

/// How long the code before still works after a session ended.
pub const GRACE: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone)]
pub struct Codes {
    current: String,
    /// The code before, and when it was replaced.
    previous: Option<(String, Instant)>,
}

impl Codes {
    pub fn new(current: String) -> Self {
        Self {
            current,
            previous: None,
        }
    }

    pub fn current(&self) -> &str {
        &self.current
    }

    /// The codes a viewer may give now: the current one, and the one before
    /// it for [`GRACE`] after it was replaced.
    pub fn valid(&self, now: Instant) -> Vec<String> {
        let previous = self
            .previous
            .as_ref()
            .filter(|(_, replaced)| now.saturating_duration_since(*replaced) < GRACE)
            .map(|(code, _)| code.clone());
        std::iter::once(self.current.clone())
            .chain(previous)
            .collect()
    }

    /// A new code after a session: the old one works for [`GRACE`] more.
    pub fn after_session(&mut self, new: String, now: Instant) {
        let old = std::mem::replace(&mut self.current, new);
        self.previous = Some((old, now));
    }

    /// A new code asked for: the old ones stop working at once.
    pub fn replace(&mut self, new: String) {
        self.current = new;
        self.previous = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn after_a_session_the_old_code_works_for_five_minutes() {
        let now = Instant::now();
        let mut codes = Codes::new("OLD".into());
        assert_eq!(codes.valid(now), ["OLD"]);
        codes.after_session("NEW".into(), now);
        assert_eq!(codes.current(), "NEW");
        assert_eq!(codes.valid(now + GRACE / 2), ["NEW", "OLD"]);
        assert_eq!(codes.valid(now + GRACE), ["NEW"]);
        // Another session ends: only the code just replaced comes along.
        codes.after_session("NEWER".into(), now + GRACE);
        assert_eq!(codes.valid(now + GRACE), ["NEWER", "NEW"]);
    }

    #[test]
    fn a_new_code_asked_for_ends_the_old_ones_at_once() {
        let now = Instant::now();
        let mut codes = Codes::new("OLD".into());
        codes.after_session("NEW".into(), now);
        codes.replace("ASKED".into());
        assert_eq!(codes.valid(now), ["ASKED"]);
    }
}
