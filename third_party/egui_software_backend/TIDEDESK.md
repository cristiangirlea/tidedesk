# Vendored: egui_software_backend 0.0.3

Source: https://crates.io/crates/egui_software_backend (MIT OR Apache-2.0, license files kept).

Local changes, all in `src/winit.rs` and marked `TideDesk patch`:

- Delayed repaint requests (`request_repaint_after`) redrew immediately, and every event reset
  the loop to `ControlFlow::Wait`, so the delay was never honoured. A focused text field's blinking
  cursor therefore redrew continuously and used a whole CPU core. The requested time is now kept in
  `repaint_at` and the loop waits until it.

Examples, tests and dev-dependencies were dropped from `Cargo.toml` because their sources are not
vendored. Drop this copy once upstream fixes the issue.
