//! `tidedesk`: one program for both sides of a session.
//!
//! `tidedesk host …` shares this computer (what `tidedesk-host` does) and
//! `tidedesk view …` connects to another (what `tidedesk-view` does). With no
//! mode word it shares (that is what the Start menu entry, the startup task
//! and autostart launch), unless the first argument names a computer to
//! connect to. This holds until one window offers both sides.

// Release builds are GUI apps with no console window; each side borrows the
// terminal it was started from.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::ffi::OsString;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
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
        &format!("TideDesk {}", env!("CARGO_PKG_VERSION")),
        "",
        "Usage: tidedesk host [OPTIONS]           share this computer (also with no mode word)",
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
        (Mode::Host, args) => tidedesk_host::main("tidedesk host", args, &["host"]),
        (Mode::View, args) => tidedesk_view::main("tidedesk view", args, &["view"]),
        (Mode::Usage, _) => print!("{}", usage()),
        (Mode::Version, _) => println!("TideDesk {}", env!("CARGO_PKG_VERSION")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
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
    fn without_a_mode_word_the_host_runs_with_every_argument() {
        assert_eq!(
            dispatch(&argv(&["tidedesk"])),
            (Mode::Host, argv(&["tidedesk"]))
        );
        assert_eq!(
            dispatch(&argv(&[r"C:\x\tidedesk.exe", "--tray"])),
            (Mode::Host, argv(&[r"C:\x\tidedesk.exe", "--tray"]))
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
