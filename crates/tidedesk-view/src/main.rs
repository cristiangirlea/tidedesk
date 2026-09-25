//! `tidedesk-view.exe`: the viewer on its own. Kept for one release so saved
//! shortcuts keep working; `tidedesk view` is the same.

// Release builds are GUI apps with no console window; see `attach_console`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    tidedesk_view::main("tidedesk-view", std::env::args_os().collect(), &[]);
}
