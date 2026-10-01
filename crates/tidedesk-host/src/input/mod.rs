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
    /// Windows shows a desktop of its own, where the pointer cannot be seen.
    away: bool,
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
            away: false,
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
    ///
    /// For a permission prompt and for the lock screen Windows shows a desktop
    /// of its own, and will not say where the pointer is while it does. The
    /// pointer then counts as off the shared screen, where it was last seen;
    /// going there and coming back both count as external movement.
    pub fn poll_pointer(&mut self) -> (bool, PointerPosition) {
        let raw = self.backend.position();
        let mut external = raw.as_ref().is_ok_and(|raw| self.pointer.observe(*raw));
        if self.away != raw.is_err() {
            self.away = raw.is_err();
            match &raw {
                Err(e) => {
                    tracing::info!("the pointer cannot be seen until the desktop is back: {e:#}")
                }
                Ok(_) => tracing::info!("the desktop is back"),
            }
            self.pointer.invalidate();
            external = true;
        }
        let mut position = self.position(self.pointer.position());
        position.inside &= !self.away;
        (external, position)
    }

    /// A read-only handoff. Never injects a cursor move.
    pub fn anchor(&mut self) -> PointerPosition {
        let (_, position) = self.poll_pointer();
        if position.inside {
            self.pointer.arm();
        }
        position
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
        let (_, position) = self.poll_pointer();
        if !self.pointer.accepts(epoch) || !position.inside {
            return Ok(Some(position));
        }
        if self.backend.inject(event, self.display).is_err() {
            // Refused: Windows put up a desktop of its own since the look.
            self.pointer.invalidate();
            let epoch = self.pointer.epoch();
            return Ok(Some(PointerPosition { epoch, ..position }));
        }
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
        /// Windows shows a desktop of its own and answers nothing about ours.
        hidden: bool,
    }
    struct FakeBackend(Arc<Mutex<Mouse>>);
    impl Backend for FakeBackend {
        fn position(&self) -> Result<(i32, i32)> {
            let mouse = self.0.lock().unwrap();
            anyhow::ensure!(!mouse.hidden, "access is denied");
            Ok(mouse.point)
        }
        fn inject(&mut self, event: InputEvent, display: DisplayRect) -> Result<()> {
            let mut mouse = self.0.lock().unwrap();
            anyhow::ensure!(!mouse.hidden, "access is denied");
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
            ..Mouse::default()
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
            away: false,
        };
        (injector, mouse)
    }

    #[test]
    fn real_handoff_path_drops_stale_input_even_before_polling() {
        let (mut injector, mouse) = mock();
        let first = injector.anchor();
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
        let fresh = injector.anchor();
        assert!(mouse.lock().unwrap().events.is_empty());
        injector
            .inject_mouse(fresh.epoch, InputEvent::MouseMove { x: 53000, y: 59000 })
            .unwrap();
        assert_eq!(mouse.lock().unwrap().events.len(), 1);
        let (external, visible) = injector.poll_pointer();
        assert!(!external);
        assert_eq!(visible.epoch, fresh.epoch);
        assert!(visible.x > 52000 && visible.y > 58000);
    }

    #[test]
    fn disabling_releases_buttons_and_invalidates_old_coordinates() {
        let (mut injector, mouse) = mock();
        let epoch = injector.anchor().epoch;
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
        let anchor = injector.anchor();
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
        let (external, initial) = injector.poll_pointer();
        assert!(!external);
        assert!(initial.inside);
        assert!(!injector.pointer.accepts(initial.epoch));
        mouse.lock().unwrap().point = (800, 900);
        let (external, moved) = injector.poll_pointer();
        assert!(external);
        assert!(moved.x > initial.x && moved.y > initial.y);
        assert!(!injector.pointer.accepts(moved.epoch));
        injector.release_mouse();
        assert_eq!(injector.poll_pointer().1.x, moved.x);
        assert_eq!(mouse.lock().unwrap().point, (800, 900));
        assert!(mouse.lock().unwrap().events.is_empty());
        mouse.lock().unwrap().point = (-50, 20);
        assert!(!injector.poll_pointer().1.inside);
    }

    /// Windows shows a desktop of its own for a permission prompt and for the
    /// lock screen, and says nothing about the pointer while it does.
    #[test]
    fn a_pointer_that_cannot_be_seen_is_off_the_screen() {
        let (mut injector, mouse) = mock();
        let held = injector.anchor().epoch;
        mouse.lock().unwrap().hidden = true;

        // Going there counts as moved, once: the viewer lets go of it.
        let (moved, gone) = injector.poll_pointer();
        assert!(moved && !gone.inside);
        assert_ne!(gone.epoch, held);
        assert_eq!(injector.poll_pointer(), (false, gone));
        // Nothing reaches it there, and it cannot be taken hold of.
        let to = InputEvent::MouseMove { x: 500, y: 500 };
        assert_eq!(injector.inject_mouse(held, to).unwrap(), Some(gone));
        assert_eq!(injector.anchor(), gone);
        assert_eq!(injector.inject_mouse(gone.epoch, to).unwrap(), Some(gone));
        assert!(mouse.lock().unwrap().events.is_empty());

        // Back where it was, it is the viewer's to take again.
        mouse.lock().unwrap().hidden = false;
        let (moved, back) = injector.poll_pointer();
        assert!(moved && back.inside);
        assert_eq!((back.x, back.y), (gone.x, gone.y));
        let anchor = injector.anchor();
        assert_eq!(injector.inject_mouse(anchor.epoch, to).unwrap(), None);
        assert_eq!(mouse.lock().unwrap().events, [to]);
    }

    /// The prompt can come up between looking at the pointer and moving it.
    #[test]
    fn a_move_that_is_refused_ends_the_viewers_hold() {
        struct Refusing(FakeBackend);
        impl Backend for Refusing {
            fn position(&self) -> Result<(i32, i32)> {
                Ok(self.0.0.lock().unwrap().point)
            }
            fn inject(&mut self, event: InputEvent, display: DisplayRect) -> Result<()> {
                self.0.inject(event, display)
            }
        }
        let (mut injector, mouse) = mock();
        injector.backend = Box::new(Refusing(FakeBackend(mouse.clone())));
        let held = injector.anchor().epoch;
        mouse.lock().unwrap().hidden = true;

        let to = InputEvent::MouseMove { x: 500, y: 500 };
        let told = injector.inject_mouse(held, to).unwrap().unwrap();
        assert_ne!(told.epoch, held);
        mouse.lock().unwrap().hidden = false;
        assert!(injector.inject_mouse(held, to).unwrap().is_some());
        assert!(mouse.lock().unwrap().events.is_empty());
    }

    #[test]
    fn denormalize_covers_edges() {
        assert_eq!(denormalize(0, 1920, 2560), 1920);
        assert_eq!(denormalize(65535, 1920, 2560), 1920 + 2559);
        assert_eq!(denormalize(32768, 0, 1001), 500);
    }
}
