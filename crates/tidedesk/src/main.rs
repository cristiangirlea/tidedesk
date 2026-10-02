//! `tidedesk`: one program for both sides of a session.
//!
//! Started plain (the Start menu entry, the startup task, autostart) it opens
//! the one window, with sharing this computer and connecting to another as
//! tabs. `tidedesk host …` runs the sharing side alone (headless too) and
//! `tidedesk view …` the connecting side; a first argument that names a
//! computer connects to it.

// Release builds are GUI apps with no console window; each side borrows the
// terminal it was started from.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod terms;

use std::ffi::OsString;

/// This build's version: the release's, which the release workflow sets,
/// else the package's.
const VERSION: &str = match option_env!("TIDEDESK_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The one window, hidden in the tray when asked.
    App {
        hidden: bool,
    },
    Host,
    View,
    Usage,
    Version,
}

/// Which side runs, and the command line it sees: its own program name
/// first, then everything after the mode word.
fn dispatch(argv: &[OsString]) -> (Mode, Vec<OsString>) {
    let program = argv
        .first()
        .cloned()
        .unwrap_or_else(|| OsString::from("tidedesk"));
    let rest = |from: usize| {
        std::iter::once(program.clone())
            .chain(argv.iter().skip(from).cloned())
            .collect::<Vec<_>>()
    };
    let word = argv.get(1).and_then(|w| w.to_str());
    match word {
        None => (Mode::App { hidden: false }, Vec::new()),
        Some("--tray") if argv.len() == 2 => (Mode::App { hidden: true }, Vec::new()),
        Some("host") => (Mode::Host, rest(2)),
        Some("view") => (Mode::View, rest(2)),
        Some("--help" | "-h" | "help") if argv.len() == 2 => (Mode::Usage, Vec::new()),
        Some("--version" | "-V" | "version") if argv.len() == 2 => (Mode::Version, Vec::new()),
        // The host takes no positional argument, so a bare one names a
        // computer to connect to: `tidedesk my-pc --code …` works.
        Some(word) if !word.starts_with('-') => (Mode::View, rest(1)),
        _ => (Mode::Host, rest(1)),
    }
}

fn usage() -> String {
    [
        &format!("TideDesk {VERSION}"),
        "",
        "Usage: tidedesk                          the window: share this computer, connect to another",
        "       tidedesk --tray                   the same, started hidden in the tray",
        "       tidedesk host [OPTIONS]           share this computer only (or headless)",
        "       tidedesk view [HOST] [OPTIONS]    connect to another computer (a bare HOST works too)",
        "",
        "`tidedesk host --help` and `tidedesk view --help` list the options.",
        "",
    ]
    .join("\n")
}

fn main() {
    tidedesk_host::attach_console();
    let argv: Vec<OsString> = std::env::args_os().collect();
    match dispatch(&argv) {
        (Mode::App { hidden }, _) => {
            // The window's TideDesk has no terminal: it writes to its log file
            // what it would print, and shows nothing more on screen.
            let log = tidedesk_core::logs::path().and_then(|p| tidedesk_core::logs::open(&p));
            let subscriber = tracing_subscriber::fmt().with_target(false);
            match log {
                Ok(file) => {
                    use tracing_subscriber::fmt::writer::MakeWriterExt;
                    let file = std::sync::Mutex::new(file);
                    subscriber
                        .with_ansi(false)
                        .with_writer(std::io::stderr.and(file))
                        .init();
                }
                Err(_) => subscriber.init(),
            }
            if let Err(e) = app::run(hidden) {
                tidedesk_host::error_box(app::WINDOW_TITLE, &format!("{e:#}"));
                std::process::exit(1);
            }
        }
        (Mode::Host, args) => tidedesk_host::main("tidedesk host", args, &["host"]),
        (Mode::View, args) => tidedesk_view::main("tidedesk view", args, &["view"]),
        (Mode::Usage, _) => print!("{}", usage()),
        (Mode::Version, _) => println!("TideDesk {VERSION}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    /// TideDesk's text, in the app and around it, uses no long dashes:
    /// a colon, a comma or a new sentence instead (#70).
    #[test]
    fn no_long_dashes_in_the_text() {
        fn look(dir: &std::path::Path, found: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if path.is_dir() {
                    if !name.starts_with('.') && name != "target" {
                        look(&path, found);
                    }
                    continue;
                }
                let text = ["rs", "md", "txt", "toml", "ps1", "yml"]
                    .iter()
                    .any(|kind| name.ends_with(&format!(".{kind}")));
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                if !text {
                    continue;
                }
                for (n, line) in content.lines().enumerate() {
                    if line.contains(['\u{2014}', '\u{2013}']) {
                        found.push(format!("{}:{}", path.display(), n + 1));
                    }
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut found = Vec::new();
        look(&root, &mut found);
        assert!(found.is_empty(), "long dashes in: {found:#?}");
    }

    #[test]
    fn a_mode_word_picks_the_side_and_is_taken_off_the_line() {
        assert_eq!(
            dispatch(&argv(&["tidedesk", "host", "--headless"])),
            (Mode::Host, argv(&["tidedesk", "--headless"]))
        );
        assert_eq!(
            dispatch(&argv(&[
                "tidedesk",
                "view",
                "TD-1A2B-3C4D-5E6F-7A8B",
                "--code",
                "x"
            ])),
            (
                Mode::View,
                argv(&["tidedesk", "TD-1A2B-3C4D-5E6F-7A8B", "--code", "x"])
            )
        );
        // A side's own help stays its own.
        assert_eq!(
            dispatch(&argv(&["tidedesk", "view", "--help"])).0,
            Mode::View
        );
    }

    #[test]
    fn a_bare_computer_name_connects_to_it() {
        assert_eq!(
            dispatch(&argv(&["tidedesk", "my-pc", "--code", "x"])),
            (Mode::View, argv(&["tidedesk", "my-pc", "--code", "x"]))
        );
        assert_eq!(
            dispatch(&argv(&["tidedesk", "TD-1A2B-3C4D-5E6F-7A8B"])).0,
            Mode::View
        );
    }

    #[test]
    fn nothing_after_the_program_opens_the_window() {
        assert_eq!(
            dispatch(&argv(&["tidedesk"])).0,
            Mode::App { hidden: false }
        );
        assert_eq!(
            dispatch(&argv(&[r"C:\x\tidedesk.exe", "--tray"])).0,
            Mode::App { hidden: true },
            "the startup entry and autostart open it hidden in the tray"
        );
    }

    #[test]
    fn host_options_without_a_mode_word_still_run_the_host() {
        assert_eq!(
            dispatch(&argv(&["tidedesk", "--headless"])),
            (Mode::Host, argv(&["tidedesk", "--headless"]))
        );
        assert_eq!(
            dispatch(&argv(&["tidedesk", "--tray", "--headless"])),
            (Mode::Host, argv(&["tidedesk", "--tray", "--headless"]))
        );
    }

    #[test]
    fn help_and_version_are_answered_at_the_top_level() {
        for word in ["--help", "-h", "help"] {
            assert_eq!(
                dispatch(&argv(&["tidedesk", word])).0,
                Mode::Usage,
                "{word}"
            );
        }
        for word in ["--version", "-V", "version"] {
            assert_eq!(
                dispatch(&argv(&["tidedesk", word])).0,
                Mode::Version,
                "{word}"
            );
        }
        let text = usage();
        assert!(
            text.contains("tidedesk host") && text.contains("tidedesk view"),
            "{text}"
        );
    }
}
