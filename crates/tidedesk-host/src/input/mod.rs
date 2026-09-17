//! Injection of viewer input into the host's desktop.

#[cfg(windows)]
mod windows;

use std::collections::HashSet;

use anyhow::Result;
use tidedesk_core::protocol::InputEvent;

use crate::capture::DisplayRect;

trait Backend: Send {
    fn inject(&mut self, event: InputEvent, display: DisplayRect) -> Result<()>;
}

/// Injects events for one viewer session and remembers which keys it holds, so
/// a dropped connection never leaves a key (say, Ctrl) stuck down on the host.
pub struct Injector {
    backend: Box<dyn Backend>,
    display: DisplayRect,
    held_keys: HashSet<u16>,
}

impl Injector {
    pub fn new(display: DisplayRect) -> Result<Self> {
        #[cfg(windows)]
        let backend: Box<dyn Backend> = Box::new(windows::SendInputBackend::new());
        #[cfg(not(windows))]
        anyhow::bail!("input injection is not implemented on this platform yet");
        #[allow(unreachable_code)]
        Ok(Self {
            backend,
            display,
            held_keys: HashSet::new(),
        })
    }

    pub fn inject(&mut self, event: InputEvent) -> Result<()> {
        if let InputEvent::Key { scancode, pressed } = event {
            if pressed {
                self.held_keys.insert(scancode);
            } else {
                self.held_keys.remove(&scancode);
            }
        }
        self.backend.inject(event, self.display)
    }

    pub fn release_all(&mut self) {
        for scancode in std::mem::take(&mut self.held_keys) {
            let _ = self.backend.inject(
                InputEvent::Key {
                    scancode,
                    pressed: false,
                },
                self.display,
            );
        }
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        self.release_all();
    }
}

/// Maps a normalised `0..=65535` coordinate onto `len` pixels starting at `origin`.
pub fn denormalize(v: u16, origin: i32, len: i32) -> i32 {
    origin + (v as i64 * (len.max(1) - 1) as i64 / 65535) as i32
}

#[cfg(test)]
mod tests {
    use super::denormalize;

    #[test]
    fn denormalize_covers_edges() {
        assert_eq!(denormalize(0, 1920, 2560), 1920);
        assert_eq!(denormalize(65535, 1920, 2560), 1920 + 2559);
        assert_eq!(denormalize(32768, 0, 1001), 500);
    }
}
