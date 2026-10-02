//! The license and the terms of use: readable from Settings, and accepted
//! once before a new installation starts sharing.

use std::io;
use std::path::Path;
use std::sync::LazyLock;

use egui::RichText;

/// The terms' "Last updated" date. Accepting records it; newer terms are
/// pointed out again.
pub const TERMS_VERSION: &str = "2026-10-02";

pub const LICENSE_TEXT: &str = include_str!("../../../LICENSE");
pub const TERMS_TEXT: &str = include_str!("../../../docs/terms-of-use.md");

/// Where the version last accepted on this computer is kept.
const ACCEPTED_FILE: &str = "terms-accepted.txt";

/// Files that show TideDesk was used on this computer before.
const USED_BEFORE: [&str; 3] = ["host-cert.der", "known_hosts.txt", "computers.toml"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// These terms were accepted here.
    Accepted,
    /// Nothing was used here yet: accept before sharing starts.
    New,
    /// Used before these terms: keeps working, and says that they exist, so
    /// a computer reached remotely is never locked out by an update.
    Changed,
}

pub fn consent(accepted: Option<&str>, used_before: bool) -> Consent {
    match accepted.map(str::trim) {
        Some(version) if version == TERMS_VERSION => Consent::Accepted,
        _ if used_before => Consent::Changed,
        _ => Consent::New,
    }
}

/// This computer's consent, from its configuration directory.
pub fn load(dir: &Path) -> Consent {
    let accepted = std::fs::read_to_string(dir.join(ACCEPTED_FILE)).ok();
    let used_before = USED_BEFORE.iter().any(|file| dir.join(file).exists());
    consent(accepted.as_deref(), used_before)
}

/// Records that these terms were accepted.
pub fn accept(dir: &Path) -> io::Result<()> {
    std::fs::write(dir.join(ACCEPTED_FILE), TERMS_VERSION)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading { level: usize, text: String },
    Paragraph(String),
    Bullet(String),
}

/// The license (plain text) or the terms (plain-text-friendly markdown) as
/// blocks to draw: `#` headings, `- ` bullets, and paragraphs whose wrapped
/// lines are joined.
pub fn blocks(text: &str) -> Vec<Block> {
    let mut found = Vec::new();
    // Whether the next plain line continues the last paragraph or bullet.
    let mut open = false;
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            open = false;
        } else if line.starts_with('#') {
            let level = line.chars().take_while(|c| *c == '#').count();
            let text = line[level..].trim().to_string();
            found.push(Block::Heading { level, text });
            open = false;
        } else if let Some(item) = line.strip_prefix("- ") {
            found.push(Block::Bullet(item.to_string()));
            open = true;
        } else if let (true, Some(Block::Paragraph(last) | Block::Bullet(last))) =
            (open, found.last_mut())
        {
            last.push(' ');
            last.push_str(line);
        } else {
            found.push(Block::Paragraph(line.to_string()));
            open = true;
        }
    }
    found
}

/// The two texts, read once.
pub static LICENSE: LazyLock<Vec<Block>> = LazyLock::new(|| blocks(LICENSE_TEXT));
pub static TERMS: LazyLock<Vec<Block>> = LazyLock::new(|| blocks(TERMS_TEXT));

/// Draws a text read with [`blocks`].
pub fn show(ui: &mut egui::Ui, text: &[Block]) {
    for block in text {
        match block {
            Block::Heading { level: 1, text } => {
                ui.heading(text);
            }
            Block::Heading { text, .. } => {
                ui.add_space(4.0);
                ui.label(RichText::new(text).strong());
            }
            Block::Paragraph(text) => {
                ui.label(text);
            }
            Block::Bullet(text) => {
                ui.label(format!("•  {text}"));
            }
        }
        ui.add_space(2.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tidedesk-terms-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn consent_depends_on_the_version_accepted_and_earlier_use() {
        assert_eq!(consent(None, false), Consent::New);
        assert_eq!(consent(None, true), Consent::Changed);
        assert_eq!(consent(Some("2020-01-01"), true), Consent::Changed);
        let accepted = format!(" {TERMS_VERSION}\r\n");
        assert_eq!(consent(Some(&accepted), true), Consent::Accepted);
        assert_eq!(consent(Some(&accepted), false), Consent::Accepted);
    }

    #[test]
    fn accepting_is_remembered_per_terms_version() {
        let dir = temp_dir("accept");
        assert_eq!(load(&dir), Consent::New, "a fresh installation");
        std::fs::write(dir.join("host-cert.der"), b"cert").unwrap();
        assert_eq!(load(&dir), Consent::Changed, "used before any terms");
        accept(&dir).unwrap();
        assert_eq!(load(&dir), Consent::Accepted);
        std::fs::write(dir.join(ACCEPTED_FILE), "2020-01-01").unwrap();
        assert_eq!(load(&dir), Consent::Changed, "older terms were accepted");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn blocks_render_headings_paragraphs_and_bullets() {
        let text =
            "# Title\n\nFirst line\nwrapped on.\n\n## 1. Part\n\n- one\n  more\n- two\n\nAfter.\n";
        let heading = |level, text: &str| Block::Heading {
            level,
            text: text.into(),
        };
        assert_eq!(
            blocks(text),
            [
                heading(1, "Title"),
                Block::Paragraph("First line wrapped on.".into()),
                heading(2, "1. Part"),
                Block::Bullet("one more".into()),
                Block::Bullet("two".into()),
                Block::Paragraph("After.".into()),
            ]
        );
    }

    #[test]
    fn compiled_in_texts_are_the_repository_documents() {
        assert!(LICENSE_TEXT.starts_with("TideDesk Personal Use Source License"));
        assert!(
            TERMS_TEXT.contains(&format!("Last updated: {TERMS_VERSION}")),
            "TERMS_VERSION must be the document's date"
        );
        assert!(TERMS_TEXT.contains("rendezvous.tidedesk.app"));
        // Shown as plain text in the window: no inline markup.
        for markup in ["**", "](", "`"] {
            assert!(!TERMS_TEXT.contains(markup), "{markup}");
        }
        let headings = blocks(TERMS_TEXT)
            .into_iter()
            .filter(|b| matches!(b, Block::Heading { .. }))
            .count();
        assert!(headings >= 8, "{headings}");
    }
}
