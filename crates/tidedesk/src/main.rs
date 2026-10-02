//! `tidedesk`: TideDesk's own program, the window with nothing added.

// Release builds are GUI apps with no console window; each side borrows the
// terminal it was started from.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    tidedesk_app::run(tidedesk_app::Extensions::default());
}
