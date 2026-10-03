//! Problems TideDesk ran into on this computer, kept in `problems.toml` in
//! the settings folder for the person to see under History, and to report
//! if they choose. Nothing is sent by itself: a report goes only where the
//! person sends it, after they have seen all of it.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// TideDesk's version, as the release names it.
pub const VERSION: &str = match option_env!("TIDEDESK_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Problems kept; older ones go.
pub const KEPT: usize = 30;

/// The same problem again within this long counts once more instead of
/// being listed again.
const SAME_WITHIN_SECS: u64 = 10 * 60;

/// Where reports sent by email go.
pub const SUPPORT_EMAIL: &str = "support@tidedesk.app";

/// Most characters of a report put into an email link; the whole report goes
/// to the clipboard too.
const EMAIL_BODY_CHARS: usize = 1500;

/// One problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    /// Seconds since 1970, UTC: the last time it happened.
    pub at: u64,
    /// What failed, in words.
    pub what: String,
    /// The technical reason.
    pub detail: String,
    /// TideDesk's version then.
    pub version: String,
    /// How many times, close together.
    pub count: u32,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    problem: Vec<Problem>,
}

/// `problems.toml` in the settings folder.
pub fn path() -> Result<PathBuf> {
    Ok(crate::paths::config_dir()?.join("problems.toml"))
}

/// The problems at `path`, the latest first.
pub fn list_from(path: &Path) -> Vec<Problem> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<File>(&text).ok())
        .map(|file| file.problem)
        .unwrap_or_default()
}

/// Adds a problem at `now` to the ones at `path`.
pub fn record_to(path: &Path, now: u64, what: &str, detail: &str) -> Result<()> {
    let mut problems = list_from(path);
    match problems.first_mut() {
        Some(last)
            if last.what == what
                && last.detail == detail
                && now.saturating_sub(last.at) <= SAME_WITHIN_SECS =>
        {
            last.at = now;
            last.count = last.count.saturating_add(1);
        }
        _ => problems.insert(
            0,
            Problem {
                at: now,
                what: what.to_string(),
                detail: detail.to_string(),
                version: VERSION.to_string(),
                count: 1,
            },
        ),
    }
    problems.truncate(KEPT);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, toml::to_string(&File { problem: problems })?)?;
    Ok(())
}

/// The problems on this computer, the latest first.
pub fn list() -> Vec<Problem> {
    path().map(|p| list_from(&p)).unwrap_or_default()
}

/// Records a problem on this computer; a problem recording one is only
/// logged, never shown.
pub fn record(what: &str, detail: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if let Err(e) = path().and_then(|p| record_to(&p, now, what, detail)) {
        tracing::warn!("could not keep a problem: {e:#}");
    }
}

/// Forgets every problem.
pub fn clear() -> Result<()> {
    let path = path()?;
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// `text` with anything shaped like an access code (`XXXX-XXXX-XX`) hidden.
pub fn without_codes(text: &str) -> String {
    let code_shaped = |word: &str| {
        let parts: Vec<&str> = word.split('-').collect();
        parts.len() == 3
            && [4, 4, 2].iter().zip(&parts).all(|(n, part)| {
                part.len() == *n && part.chars().all(|c| c.is_ascii_alphanumeric())
            })
    };
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if code_shaped(word) {
            out.push_str("[access code]");
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '-' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// The report the person sees and may send: what happened, the version,
/// the system, and `log`, the end of the log, with access codes hidden.
pub fn report(problem: &Problem, system: &str, log: &str) -> String {
    let mut text = format!(
        "TideDesk problem report\n\
         What: {}\n\
         When: {} UTC\n\
         Times: {}\n\
         Version: {}\n\
         System: {}\n\
         \n\
         Details:\n{}\n",
        problem.what,
        crate::dates::time(problem.at),
        problem.count,
        problem.version,
        system,
        problem.detail,
    );
    if !log.trim().is_empty() {
        text.push_str("\nThe end of tidedesk.log:\n");
        text.push_str(log);
        text.push('\n');
    }
    without_codes(&text)
}

/// A `mailto:` link with the report, cut to fit an email link.
pub fn email_link(problem: &Problem, report: &str) -> String {
    let mut body: String = report.chars().take(EMAIL_BODY_CHARS).collect();
    if body.len() < report.len() {
        body.push_str("\n\n(Cut short: the whole report is on the clipboard, paste it here.)");
    }
    format!(
        "mailto:{SUPPORT_EMAIL}?subject={}&body={}",
        encode(&format!("TideDesk problem: {}", problem.what)),
        encode(&body)
    )
}

/// Percent-encoding for a link: everything but letters, digits and `-._~`.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The last `lines` lines of TideDesk's log, or nothing.
pub fn log_tail(lines: usize) -> String {
    let Ok(text) = crate::logs::path().and_then(|p| Ok(std::fs::read_to_string(p)?)) else {
        return String::new();
    };
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_790_000_000;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tidedesk-problems-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("problems.toml")
    }

    #[test]
    fn problems_are_kept_latest_first_and_repeats_counted() {
        let path = temp("kept");
        assert_eq!(list_from(&path), Vec::new());
        record_to(&path, NOW, "Sharing could not start", "port 47800 in use").unwrap();
        record_to(
            &path,
            NOW + 60,
            "Sharing could not start",
            "port 47800 in use",
        )
        .unwrap();
        record_to(
            &path,
            NOW + 120,
            "A session ended because of an error",
            "video stream: encoder failed",
        )
        .unwrap();
        let list = list_from(&path);
        assert_eq!(list.len(), 2, "the repeat counted, not listed again");
        assert_eq!(list[0].what, "A session ended because of an error");
        assert_eq!((list[1].count, list[1].at), (2, NOW + 60));
        assert_eq!(list[1].version, VERSION);

        // Much later, the same problem is listed again.
        record_to(
            &path,
            NOW + 3600,
            "Sharing could not start",
            "port 47800 in use",
        )
        .unwrap();
        assert_eq!(list_from(&path).len(), 3);

        for i in 0..40u64 {
            record_to(&path, NOW + 10_000 + i, &format!("problem {i}"), "x").unwrap();
        }
        let list = list_from(&path);
        assert_eq!(list.len(), KEPT);
        assert_eq!(list[0].what, "problem 39");
    }

    #[test]
    fn a_report_says_what_happened_and_hides_access_codes() {
        assert_eq!(
            without_codes(
                "code KBT2-TEXM-NG accepted; abcd-efgh-ij too; TD-5179-2FA6-7B0B-BD0F stays"
            ),
            "code [access code] accepted; [access code] too; TD-5179-2FA6-7B0B-BD0F stays"
        );
        let problem = Problem {
            at: NOW,
            what: "Sharing could not start".into(),
            detail: "listening on 0.0.0.0:47800: address in use".into(),
            version: "0.1.0-alpha.12".into(),
            count: 3,
        };
        let report = report(
            &problem,
            "Windows x86_64",
            "INFO started\nWARN code ABCD-EFGH-JK refused",
        );
        assert!(report.starts_with("TideDesk problem report\n"));
        for line in [
            "What: Sharing could not start",
            "Version: 0.1.0-alpha.12",
            "System: Windows x86_64",
            "When: 2026-09-21",
            "Times: 3",
            "listening on 0.0.0.0:47800: address in use",
            "WARN code [access code] refused",
        ] {
            assert!(report.contains(line), "{line}");
        }
        assert!(!report.contains("ABCD-EFGH-JK"));

        let link = email_link(&problem, &report);
        assert!(link.starts_with("mailto:support@tidedesk.app?subject=TideDesk%20problem%3A%20Sharing%20could%20not%20start&body="));
        assert!(!link.contains(' ') && !link.contains('\n'));
        let long = "x".repeat(10_000);
        assert!(
            email_link(&problem, &long).len() < 4_000,
            "short enough for a mail link"
        );
    }
}
