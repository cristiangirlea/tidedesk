//! Problem reports a person chose to send to TideDesk.
//!
//! The app connects over QUIC with the [`ALPN`] protocol, checks that the
//! server's certificate is the one it was built with, opens one stream,
//! writes a postcard-encoded [`Report`] and finishes the stream. The server
//! answers with an [`Answer`] and closes. A report is only ever the text the
//! person saw before sending; the server keeps no address it came from.
//!
//! As with the rendezvous messages, variants are only ever added at the end.

use serde::{Deserialize, Serialize};

/// QUIC application protocol of report sending.
pub const ALPN: &[u8] = b"tidedesk-report/1";

/// UDP port of the report service.
pub const DEFAULT_PORT: u16 = 47902;

/// Most characters of report text the service takes.
pub const MAX_TEXT: usize = 32 * 1024;

/// Most characters of the version.
pub const MAX_VERSION: usize = 64;

/// Most bytes of an encoded [`Report`]: the text in UTF-8 at four bytes a
/// character at worst, and room for the rest.
pub const MAX_REPORT_BYTES: usize = MAX_TEXT * 4 + 1024;

/// Most bytes of an encoded [`Answer`].
pub const MAX_ANSWER_BYTES: usize = 256;

/// What the app sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// TideDesk's version.
    pub version: String,
    /// The report, as the person saw it.
    pub text: String,
}

impl Report {
    /// Whether the service takes it: not empty, and within the limits.
    pub fn fits(&self) -> bool {
        !self.text.trim().is_empty()
            && self.text.chars().count() <= MAX_TEXT
            && self.version.chars().count() <= MAX_VERSION
    }
}

/// What the service answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Answer {
    /// Kept, under this reference, which the person can quote.
    Received(String),
    /// Too many reports from this address lately; try later.
    TooMany,
    /// Empty, too long, or not a report.
    Refused,
}

pub fn encode<T: Serialize>(message: &T) -> Vec<u8> {
    postcard::to_allocvec(message).expect("report messages always encode")
}

pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    postcard::from_bytes(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_goes_and_comes_back_the_same() {
        let report = Report {
            version: "0.1.0-alpha.12".into(),
            text: "TideDesk problem report\nWhat: Sharing could not start".into(),
        };
        assert_eq!(decode::<Report>(&encode(&report)), Some(report));
        let answer = Answer::Received("R-20261003-ABCD".into());
        assert_eq!(decode::<Answer>(&encode(&answer)), Some(answer));
    }

    #[test]
    fn answers_keep_their_places_on_the_wire() {
        // Apps already sent read answers by position: never reorder them.
        assert_eq!(encode(&Answer::Received("x".into())), [0, 1, b'x']);
        assert_eq!(encode(&Answer::TooMany), [1]);
        assert_eq!(encode(&Answer::Refused), [2]);
    }

    #[test]
    fn only_reports_within_the_limits_fit() {
        let report = |version: &str, text: String| Report {
            version: version.into(),
            text,
        };
        assert!(report("1", "a problem".into()).fits());
        assert!(!report("1", " \n ".into()).fits());
        assert!(report("1", "é".repeat(MAX_TEXT)).fits());
        assert!(!report("1", "a".repeat(MAX_TEXT + 1)).fits());
        assert!(!report(&"9".repeat(MAX_VERSION + 1), "a problem".into()).fits());
    }

    #[test]
    fn the_largest_report_that_fits_stays_within_its_byte_limit() {
        let report = Report {
            version: "9".repeat(MAX_VERSION),
            text: "\u{1F600}".repeat(MAX_TEXT),
        };
        assert!(report.fits());
        assert!(encode(&report).len() <= MAX_REPORT_BYTES);
        assert!(encode(&Answer::Received("R".repeat(100))).len() <= MAX_ANSWER_BYTES);
    }
}
