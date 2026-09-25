//! `tidedesk-host.exe`: the host on its own. Kept for one release so saved
//! shortcuts and autostart entries keep working; `tidedesk host` is the same.

// Release builds are GUI apps with no console window; console output still
// reaches a terminal the host was started from (see `attach_console`).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    tidedesk_host::main("tidedesk-host", std::env::args_os().collect(), &[]);
}
