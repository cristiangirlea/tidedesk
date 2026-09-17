//! Session permission generations and host-authoritative pointer handoff.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharingState {
    pub request: u64,
    pub generation: u64,
    pub clipboard: bool,
    pub mouse: bool,
}

impl SharingState {
    pub fn update(&mut self, request: u64, clipboard: bool, mouse: bool) -> bool {
        if (self.request, self.clipboard, self.mouse) == (request, clipboard, mouse) {
            return false;
        }
        self.request = request;
        self.generation = self.generation.wrapping_add(1);
        self.clipboard = clipboard;
        self.mouse = mouse;
        true
    }

    pub fn accepts_clipboard(self, generation: u64) -> bool {
        self.clipboard && generation == self.generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerPosition {
    pub epoch: u64,
    pub x: u16,
    pub y: u16,
    pub inside: bool,
}

/// Uses physical pixels, not rounded wire coordinates, to detect local movement.
#[derive(Debug)]
pub struct PointerAuthority {
    position: (i32, i32),
    epoch: u64,
    armed: bool,
}

impl PointerAuthority {
    pub fn new(position: (i32, i32)) -> Self {
        Self {
            position,
            epoch: 1,
            armed: false,
        }
    }

    pub fn observe(&mut self, position: (i32, i32)) -> bool {
        if self.position == position {
            return false;
        }
        self.position = position;
        self.invalidate();
        true
    }

    pub fn invalidate(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.armed = false;
    }

    pub fn arm(&mut self) {
        self.armed = true;
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn accepts(&self, epoch: u64) -> bool {
        self.armed && self.epoch == epoch
    }
    pub fn injected(&mut self, position: (i32, i32)) {
        self.position = position;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_motion_invalidates_already_queued_remote_input() {
        let mut pointer = PointerAuthority::new((50, 50));
        assert!(!pointer.accepts(pointer.epoch()));
        pointer.arm();
        let old = pointer.epoch();
        assert!(pointer.accepts(old));
        assert!(pointer.observe((900, 200)));
        assert!(!pointer.accepts(old));
        // Reading/arming an anchor never changes the physical position.
        pointer.arm();
        assert_eq!(pointer.position, (900, 200));
        assert!(pointer.accepts(pointer.epoch()));
        pointer.injected((901, 201));
        assert!(!pointer.observe((901, 201)));
        assert!(pointer.accepts(pointer.epoch()));
    }

    #[test]
    fn toggle_or_focus_loss_requires_a_new_anchor() {
        let mut pointer = PointerAuthority::new((20, 20));
        pointer.arm();
        let epoch = pointer.epoch();
        pointer.invalidate();
        assert!(!pointer.accepts(epoch));
    }

    #[test]
    fn clipboard_cannot_cross_a_disable_enable_boundary() {
        let mut state = SharingState::default();
        assert!(!state.accepts_clipboard(0));
        state.update(1, true, true);
        let old = state.generation;
        assert!(state.accepts_clipboard(old));
        state.update(2, false, true);
        assert!(!state.accepts_clipboard(old));
        state.update(3, true, true);
        assert!(!state.accepts_clipboard(old));
        assert!(state.accepts_clipboard(state.generation));
    }
}
