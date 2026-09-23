//! The line protocol between the launcher and a session process it starts.
//!
//! A session started by the launcher writes these lines to stderr (its
//! tracing output goes to stdout, which the launcher discards) and reads
//! the launcher's answers, `trust` or `cancel`, one per line on stdin.

use std::fmt;
use std::net::SocketAddr;

use tidedesk_core::identity::{PinStatus, format_fingerprint, normalize_fingerprint};

/// Set in the environment of sessions the launcher starts.
pub const LAUNCHER_ENV: &str = "TIDEDESK_LAUNCHER";

/// Launcher answers, one per line on the session's stdin.
pub const TRUST: &str = "trust";
pub const CANCEL: &str = "cancel";

/// The reason a session gives when the launcher cancelled it.
pub const CANCELLED: &str = "cancelled";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildLine {
    /// What the session is doing, for the launcher to show.
    Status(String),
    /// This computer's internet address, for the person at the host.
    ViewerAddress(SocketAddr),
    /// The host's identity needs the user's decision before connecting.
    Fingerprint {
        /// The address it is (to be) pinned under.
        address: String,
        fingerprint: String,
        status: PinStatus,
    },
    /// The session is up, with this host.
    Connected(String),
    Error(String),
    Disconnected(String),
    /// Anything else, such as a panic message.
    Other(String),
}

impl fmt::Display for ChildLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status(text) => write!(f, "status: {text}"),
            Self::ViewerAddress(me) => write!(f, "viewer-address: {me}"),
            Self::Fingerprint {
                address,
                fingerprint,
                status,
            } => {
                // One word each, so the line splits on spaces.
                let status = match status {
                    PinStatus::Trusted => "trusted".to_string(),
                    PinStatus::Unknown => "unknown".to_string(),
                    PinStatus::Mismatch { pinned } => {
                        format!("mismatch:{}", normalize_fingerprint(pinned))
                    }
                };
                let fingerprint = normalize_fingerprint(fingerprint);
                write!(f, "fingerprint: {address} {fingerprint} {status}")
            }
            Self::Connected(host) => write!(f, "connected: {host}"),
            Self::Error(error) => write!(f, "error: {error}"),
            Self::Disconnected(reason) => write!(f, "disconnected: {reason}"),
            Self::Other(line) => f.write_str(line),
        }
    }
}

/// Reads one line a session wrote. Never fails: unknown lines are `Other`.
pub fn parse(line: &str) -> ChildLine {
    let other = || ChildLine::Other(line.to_string());
    let Some((prefix, rest)) = line.split_once(": ") else {
        return other();
    };
    match prefix {
        "status" => ChildLine::Status(rest.into()),
        "viewer-address" => rest
            .parse()
            .map(ChildLine::ViewerAddress)
            .unwrap_or_else(|_| other()),
        "fingerprint" => {
            let words: Vec<&str> = rest.split(' ').collect();
            let [address, fingerprint, status] = words[..] else {
                return other();
            };
            let status = match status {
                "trusted" => PinStatus::Trusted,
                "unknown" => PinStatus::Unknown,
                _ => match status.strip_prefix("mismatch:") {
                    Some(pinned) => PinStatus::Mismatch {
                        pinned: pinned.into(),
                    },
                    None => return other(),
                },
            };
            ChildLine::Fingerprint {
                address: address.into(),
                fingerprint: format_fingerprint(fingerprint),
                status,
            }
        }
        "connected" => ChildLine::Connected(rest.into()),
        "error" => ChildLine::Error(rest.into()),
        "disconnected" => ChildLine::Disconnected(rest.into()),
        _ => other(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "3F2A91C0000000000000000000000000000000000000000000000000000000AB";

    #[test]
    fn parse_child_line_recognises_every_prefix_and_ignores_noise() {
        let lines = [
            ChildLine::Status("Looking up this computer's internet address…".into()),
            ChildLine::ViewerAddress("203.0.113.9:40000".parse().unwrap()),
            ChildLine::Fingerprint {
                address: "203.0.113.5:40000".into(),
                fingerprint: format_fingerprint(FP),
                status: PinStatus::Unknown,
            },
            ChildLine::Fingerprint {
                address: "203.0.113.5:40000".into(),
                fingerprint: format_fingerprint(FP),
                status: PinStatus::Mismatch {
                    pinned: "AAAA1111".into(),
                },
            },
            ChildLine::Connected("Office PC".into()),
            ChildLine::Error("host refused the connection: wrong access code".into()),
            ChildLine::Disconnected(CANCELLED.into()),
        ];
        for line in lines {
            assert_eq!(parse(&line.to_string()), line, "{line}");
        }
        // Fingerprints travel as one word and come back grouped for display.
        let text = format!(
            "fingerprint: 203.0.113.5:40000 {} unknown",
            FP.to_lowercase()
        );
        assert!(matches!(
            parse(&text),
            ChildLine::Fingerprint { fingerprint, .. } if fingerprint.starts_with("3F2A 91C0 ")
        ));

        for noise in [
            "thread 'main' panicked at src/main.rs",
            "viewer-address: not an address",
            "fingerprint: 203.0.113.5:40000",
            "fingerprint: a b maybe",
            "",
        ] {
            assert_eq!(parse(noise), ChildLine::Other(noise.into()), "{noise}");
        }
    }
}
