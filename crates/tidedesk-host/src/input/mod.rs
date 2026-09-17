//! Injection of viewer input into the host's desktop.

#[cfg(windows)]
mod windows;

use std::collections::HashSet;

use anyhow::Result;
use tidedesk_core::protocol::{InputEvent, MouseButton};
use tidedesk_core::sharing::{PointerAuthority, PointerPosition};

use crate::capture::DisplayRect;

trait Backend: Send {
    fn inject(&mut self, event: InputEvent, display: DisplayRect) -> Result<()>;
    fn position(&self) -> Result<(i32, i32)>;
}

/// Injects events for one viewer session and remembers which keys it holds, so
/// a dropped connection never leaves a key (say, Ctrl) stuck down on the host.
pub struct Injector {
    backend: Box<dyn Backend>,
    display: DisplayRect,
    held_keys: HashSet<u16>,
    held_buttons: HashSet<MouseButton>,
    pointer: PointerAuthority,
}

impl Injector {
    pub fn new(display: DisplayRect) -> Result<Self> {
        #[cfg(windows)]
        let backend: Box<dyn Backend> = Box::new(windows::SendInputBackend::new());
        #[cfg(not(windows))]
        anyhow::bail!("input injection is not implemented on this platform yet");
        #[allow(unreachable_code)]
        let pointer = PointerAuthority::new(backend.position()?);
        #[allow(unreachable_code)]
        Ok(Self {
            backend,
            display,
            held_keys: HashSet::new(),
            held_buttons: HashSet::new(),
            pointer,
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

    fn position(&self, raw: (i32, i32)) -> PointerPosition {
        let d = self.display;
        let norm = |v: i32, origin: i32, len: i32| {
            (((v as i64 - origin as i64) * 65535 + (len.max(2) - 1) as i64 / 2)
                / (len.max(2) - 1) as i64)
                .clamp(0, 65535) as u16
        };
        PointerPosition {
            epoch: self.pointer.epoch(),
            x: norm(raw.0, d.left, d.width),
            y: norm(raw.1, d.top, d.height),
            inside: raw.0 >= d.left
                && raw.1 >= d.top
                && (raw.0 as i64) < d.left as i64 + d.width as i64
                && (raw.1 as i64) < d.top as i64 + d.height as i64,
        }
    }

    /// Always samples the visible position, even without mouse-control permission.
    /// The boolean distinguishes external movement from our own injected movement.
    pub fn poll_pointer(&mut self) -> Result<(bool, PointerPosition)> {
        let raw = self.backend.position()?;
        let external = self.pointer.observe(raw);
        Ok((external, self.position(raw)))
    }

    /// A read-only handoff. Never injects a cursor move.
    pub fn anchor(&mut self) -> Result<PointerPosition> {
        let raw = self.backend.position()?;
        self.pointer.observe(raw);
        let position = self.position(raw);
        if position.inside {
            self.pointer.arm();
        }
        Ok(position)
    }

    pub fn inject_mouse(
        &mut self,
        epoch: u64,
        event: InputEvent,
    ) -> Result<Option<PointerPosition>> {
        // A release is always safe, including after host movement or permission revocation.
        if let InputEvent::MouseButton {
            button,
            pressed: false,
        } = event
        {
            if self.held_buttons.remove(&button) {
                self.backend.inject(event, self.display)?;
            }
            return Ok(None);
        }
        if matches!(event, InputEvent::Key { .. }) {
            anyhow::bail!("keyboard event in mouse message");
        }
        let raw = self.backend.position()?;
        self.pointer.observe(raw);
        let position = self.position(raw);
        if !self.pointer.accepts(epoch) || !position.inside {
            return Ok(Some(position));
        }
        self.backend.inject(event, self.display)?;
        if let InputEvent::MouseMove { x, y } = event {
            // Record only our requested destination. A physical movement during
            // injection must still be detected by the next observation.
            self.pointer.injected((
                denormalize(x, self.display.left, self.display.width),
                denormalize(y, self.display.top, self.display.height),
            ));
        }
        if let InputEvent::MouseButton {
            button,
            pressed: true,
        } = event
        {
            self.held_buttons.insert(button);
        }
        Ok(None)
    }

    pub fn release_mouse(&mut self) {
        self.pointer.invalidate();
        for button in std::mem::take(&mut self.held_buttons) {
            let _ = self.backend.inject(
                InputEvent::MouseButton {
                    button,
                    pressed: false,
                },
                self.display,
            );
        }
    }

    pub fn release_all(&mut self) {
        self.release_mouse();
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
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Mouse {
        point: (i32, i32),
        events: Vec<InputEvent>,
    }
    struct FakeBackend(Arc<Mutex<Mouse>>);
    impl Backend for FakeBackend {
        fn position(&self) -> Result<(i32, i32)> {
            Ok(self.0.lock().unwrap().point)
        }
        fn inject(&mut self, event: InputEvent, display: DisplayRect) -> Result<()> {
            let mut mouse = self.0.lock().unwrap();
            mouse.events.push(event);
            if let InputEvent::MouseMove { x, y } = event {
                mouse.point = (
                    denormalize(x, display.left, display.width),
                    denormalize(y, display.top, display.height),
                );
            }
            Ok(())
        }
    }

    fn mock() -> (Injector, Arc<Mutex<Mouse>>) {
        let mouse = Arc::new(Mutex::new(Mouse {
            point: (10, 20),
            events: Vec::new(),
        }));
        let injector = Injector {
            backend: Box::new(FakeBackend(mouse.clone())),
            display: DisplayRect {
                left: 0,
                top: 0,
                width: 1000,
                height: 1000,
            },
            held_keys: HashSet::new(),
            held_buttons: HashSet::new(),
            pointer: PointerAuthority::new((10, 20)),
        };
        (injector, mouse)
    }

    #[test]
    fn real_handoff_path_drops_stale_input_even_before_polling() {
        let (mut injector, mouse) = mock();
        let first = injector.anchor().unwrap();
        assert!(mouse.lock().unwrap().events.is_empty());
        // Local host movement happens immediately before an old remote event.
        mouse.lock().unwrap().point = (800, 900);
        let update = injector
            .inject_mouse(first.epoch, InputEvent::MouseMove { x: 10, y: 20 })
            .unwrap()
            .unwrap();
        assert!(mouse.lock().unwrap().events.is_empty());
        assert_eq!(mouse.lock().unwrap().point, (800, 900));
        assert_ne!(first.epoch, update.epoch);
        let fresh = injector.anchor().unwrap();
        assert!(mouse.lock().unwrap().events.is_empty());
        injector
            .inject_mouse(fresh.epoch, InputEvent::MouseMove { x: 53000, y: 59000 })
            .unwrap();
        assert_eq!(mouse.lock().unwrap().events.len(), 1);
        let (external, visible) = injector.poll_pointer().unwrap();
        assert!(!external);
        assert_eq!(visible.epoch, fresh.epoch);
        assert!(visible.x > 52000 && visible.y > 58000);
    }

    #[test]
    fn disabling_releases_buttons_and_invalidates_old_coordinates() {
        let (mut injector, mouse) = mock();
        let epoch = injector.anchor().unwrap().epoch;
        let down = InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        };
        injector.inject_mouse(epoch, down).unwrap();
        injector.release_mouse();
        assert_eq!(
            mouse.lock().unwrap().events.last(),
            Some(&InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false
            })
        );
        let count = mouse.lock().unwrap().events.len();
        assert!(injector.inject_mouse(epoch, down).unwrap().is_some());
        assert_eq!(mouse.lock().unwrap().events.len(), count);
    }

    #[test]
    fn host_pointer_on_another_monitor_is_not_pulled_onto_shared_screen() {
        let (mut injector, mouse) = mock();
        mouse.lock().unwrap().point = (-100, 20);
        let anchor = injector.anchor().unwrap();
        assert!(!anchor.inside);
        assert!(
            injector
                .inject_mouse(anchor.epoch, InputEvent::MouseMove { x: 0, y: 0 })
                .unwrap()
                .is_some()
        );
        assert_eq!(mouse.lock().unwrap().point, (-100, 20));
        assert!(mouse.lock().unwrap().events.is_empty());
    }

    #[test]
    fn cursor_observation_without_control_never_arms_or_moves_the_host() {
        let (mut injector, mouse) = mock();
        let (external, initial) = injector.poll_pointer().unwrap();
        assert!(!external);
        assert!(initial.inside);
        assert!(!injector.pointer.accepts(initial.epoch));
        mouse.lock().unwrap().point = (800, 900);
        let (external, moved) = injector.poll_pointer().unwrap();
        assert!(external);
        assert!(moved.x > initial.x && moved.y > initial.y);
        assert!(!injector.pointer.accepts(moved.epoch));
        injector.release_mouse();
        assert_eq!(injector.poll_pointer().unwrap().1.x, moved.x);
        assert_eq!(mouse.lock().unwrap().point, (800, 900));
        assert!(mouse.lock().unwrap().events.is_empty());
        mouse.lock().unwrap().point = (-50, 20);
        assert!(!injector.poll_pointer().unwrap().1.inside);
    }

    #[test]
    fn denormalize_covers_edges() {
        assert_eq!(denormalize(0, 1920, 2560), 1920);
        assert_eq!(denormalize(65535, 1920, 2560), 1920 + 2559);
        assert_eq!(denormalize(32768, 0, 1001), 500);
    }
}
