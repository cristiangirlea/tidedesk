//! The viewer never moves its cursor in response to passive host updates.
use crate::layout::Placement;
use tidedesk_core::protocol::InputEvent;
use tidedesk_core::sharing::PointerPosition;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    NeedsAnchor,
    Waiting(u64),
    Warping { epoch: u64, x: f64, y: f64 },
    Active { epoch: u64, x: f64, y: f64 },
}

#[derive(Debug, PartialEq)]
pub enum Motion {
    None,
    Request(u64),
    Move { epoch: u64, x: u16, y: u16 },
}

/// What becomes of a mouse button that is pressed.
#[derive(Debug, PartialEq)]
pub enum Press {
    /// It goes to the host, after the pointer (`to`) if that is not where
    /// the host last had it.
    Send { epoch: u64, to: Option<(u16, u16)> },
    /// It waits for the host's pointer position, to be asked for with this
    /// request.
    Request(u64),
    /// Nothing to send: it waits for the position asked for already.
    Wait,
}

pub struct PointerFlow {
    phase: Phase,
    next_request: u64,
    /// Where in the window a button was last pressed before the handoff.
    pressed: Option<(f64, f64)>,
    /// What waits for the handoff: for each button pressed the way to its
    /// place and the button, and buttons let go of.
    waiting: Vec<InputEvent>,
    /// What waited, and the epoch to send it with, once the handoff is made.
    claim: Option<(u64, Vec<InputEvent>)>,
}

impl Default for PointerFlow {
    fn default() -> Self {
        Self {
            phase: Phase::NeedsAnchor,
            next_request: 0,
            pressed: None,
            waiting: Vec::new(),
            claim: None,
        }
    }
}

impl PointerFlow {
    /// Presses that wait for a handoff at most: more than anyone makes in
    /// the second it may take.
    const MOST_WAITING: usize = 16;

    pub fn invalidate(&mut self) {
        self.phase = Phase::NeedsAnchor;
        self.pressed = None;
        self.waiting.clear();
        self.claim = None;
    }

    /// A `button` pressed with the viewer's pointer at `x`, `y`. A click
    /// says where it is meant, so before the handoff it is not lost but
    /// waits for it, and then takes the host's pointer to its place instead
    /// of the viewer's going to the host's (see [`PointerFlow::claim`]).
    /// Tablets, pens and tools put the pointer somewhere and click at once.
    pub fn pressed(&mut self, x: f64, y: f64, placement: Placement, button: InputEvent) -> Press {
        let wait = |flow: &mut Self| {
            if flow.waits(button) < Self::MOST_WAITING {
                let (rx, ry) = placement.remote_coords(x, y);
                flow.waiting.push(InputEvent::MouseMove { x: rx, y: ry });
                flow.waiting.push(button);
                flow.pressed = Some((x, y));
            }
        };
        match self.phase {
            Phase::NeedsAnchor => {
                wait(self);
                self.next_request = self.next_request.wrapping_add(1);
                self.phase = Phase::Waiting(self.next_request);
                Press::Request(self.next_request)
            }
            Phase::Waiting(_) => {
                wait(self);
                Press::Wait
            }
            // The host has answered and the pointer is on its way there:
            // nothing will come for the button to wait for.
            Phase::Warping { .. } => Press::Wait,
            Phase::Active { epoch, .. } => {
                let to = match self.moved(x, y, placement) {
                    Motion::Move { x, y, .. } => Some((x, y)),
                    _ => None,
                };
                Press::Send { epoch, to }
            }
        }
    }

    /// How many times `event` waits for the handoff.
    fn waits(&self, event: InputEvent) -> usize {
        self.waiting.iter().filter(|e| **e == event).count()
    }

    /// A `button` let go of: the epoch to send it with. None while it waits
    /// behind its press, and when the host would not take it.
    pub fn released(&mut self, button: InputEvent) -> Option<u64> {
        let epoch = self.epoch();
        // Behind its press where that waits, and never left out there: the
        // button would stay down on the host.
        if let InputEvent::MouseButton { button: which, .. } = button
            && epoch.is_none()
        {
            let press = InputEvent::MouseButton {
                button: which,
                pressed: true,
            };
            if self.waits(button) < self.waits(press) {
                self.waiting.push(button);
            }
        }
        epoch
    }

    /// What waited for the handoff and the epoch to send it with, once:
    /// right after [`PointerFlow::anchor`] made the handoff.
    pub fn claim(&mut self) -> Option<(u64, Vec<InputEvent>)> {
        self.claim.take()
    }

    pub fn epoch(&self) -> Option<u64> {
        match self.phase {
            Phase::Active { epoch, .. } => Some(epoch),
            _ => None,
        }
    }

    /// Called after the synchronous OS reposition succeeds. Queued mouse events
    /// are sampled at their current OS location, and duplicate positions are ignored.
    pub fn warp_completed(&mut self) {
        if let Phase::Warping { epoch, x, y } = self.phase {
            self.phase = Phase::Active { epoch, x, y };
        }
    }

    pub fn moved(&mut self, x: f64, y: f64, placement: Placement) -> Motion {
        match self.phase {
            Phase::NeedsAnchor => {
                self.next_request = self.next_request.wrapping_add(1);
                self.phase = Phase::Waiting(self.next_request);
                Motion::Request(self.next_request)
            }
            Phase::Waiting(_) => Motion::None,
            Phase::Warping {
                epoch,
                x: wx,
                y: wy,
            } => {
                // SetCursorPos-generated movement must never reach the host.
                if (x - wx).abs() <= 0.5 && (y - wy).abs() <= 0.5 {
                    self.phase = Phase::Active {
                        epoch,
                        x: wx,
                        y: wy,
                    };
                }
                Motion::None
            }
            Phase::Active {
                epoch,
                x: previous_x,
                y: previous_y,
            } => {
                if (x - previous_x).abs() <= 0.5 && (y - previous_y).abs() <= 0.5 {
                    return Motion::None;
                }
                self.phase = Phase::Active { epoch, x, y };
                let (x, y) = placement.remote_coords(x, y);
                Motion::Move { epoch, x, y }
            }
        }
    }

    /// Returns a local cursor warp only after viewer activity requested an anchor.
    pub fn anchor(
        &mut self,
        request: u64,
        position: PointerPosition,
        placement: Placement,
        current: Option<(f64, f64)>,
    ) -> Option<(f64, f64)> {
        if self.phase != Phase::Waiting(request) {
            return None;
        }
        if !position.inside || placement.width == 0 || placement.height == 0 {
            self.invalidate();
            return None;
        }
        if let Some((x, y)) = self.pressed.take() {
            let epoch = position.epoch;
            self.phase = Phase::Active { epoch, x, y };
            self.claim = Some((epoch, std::mem::take(&mut self.waiting)));
            return None;
        }
        let (x, y) = placement.window_coords(position.x, position.y);
        if current.is_some_and(|(cx, cy)| (cx - x).abs() <= 0.5 && (cy - y).abs() <= 0.5) {
            self.phase = Phase::Active {
                epoch: position.epoch,
                x,
                y,
            };
            return None;
        }
        self.phase = Phase::Warping {
            epoch: position.epoch,
            x,
            y,
        };
        Some((x, y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidedesk_core::protocol::MouseButton;
    fn placement() -> Placement {
        Placement::fit(1000, 1000, 1000, 1000)
    }

    #[test]
    fn coalesced_warp_events_do_not_stall_and_duplicate_positions_do_not_move_host() {
        let mut flow = PointerFlow::default();
        let p = placement();
        flow.moved(10.0, 10.0, p);
        let (x, y) = flow
            .anchor(
                1,
                PointerPosition {
                    epoch: 5,
                    x: 40000,
                    y: 30000,
                    inside: true,
                },
                p,
                None,
            )
            .unwrap();
        flow.warp_completed();
        // A queued pre-warp notification is read using the current OS position.
        assert_eq!(flow.moved(x, y, p), Motion::None);
        assert_eq!(flow.moved(x, y, p), Motion::None);
        assert!(matches!(
            flow.moved(x + 2.0, y, p),
            Motion::Move { epoch: 5, .. }
        ));
        assert_eq!(flow.moved(x + 2.0, y, p), Motion::None);
    }

    #[test]
    fn host_move_then_viewer_move_and_synthetic_warp_never_move_host() {
        let mut flow = PointerFlow::default();
        let p = placement();
        assert_eq!(flow.moved(10.0, 10.0, p), Motion::Request(1));
        let pos = PointerPosition {
            epoch: 2,
            x: 40000,
            y: 30000,
            inside: true,
        };
        let (x, y) = flow.anchor(1, pos, p, Some((10.0, 10.0))).unwrap();
        assert_eq!(flow.moved(x, y, p), Motion::None);
        assert!(matches!(
            flow.moved(x + 1.0, y, p),
            Motion::Move { epoch: 2, .. }
        ));
        flow.invalidate();
        assert_eq!(flow.epoch(), None);
        assert_eq!(flow.moved(20.0, 20.0, p), Motion::Request(2));
        assert_eq!(flow.moved(21.0, 20.0, p), Motion::None);
    }

    #[test]
    fn stale_responses_and_motion_before_warp_are_dropped() {
        let mut flow = PointerFlow::default();
        let p = placement();
        let pos = PointerPosition {
            epoch: 7,
            x: 40000,
            y: 30000,
            inside: true,
        };
        flow.moved(10.0, 10.0, p);
        flow.invalidate();
        assert!(flow.anchor(1, pos, p, None).is_none());
        assert_eq!(flow.moved(11.0, 10.0, p), Motion::Request(2));
        let (x, y) = flow.anchor(2, pos, p, None).unwrap();
        assert_eq!(flow.moved(12.0, 10.0, p), Motion::None);
        assert_eq!(flow.moved(x, y, p), Motion::None);
        assert_eq!(flow.epoch(), Some(7));
    }

    const HOST: PointerPosition = PointerPosition {
        epoch: 5,
        x: 40000,
        y: 30000,
        inside: true,
    };
    const DOWN: InputEvent = InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: true,
    };
    const UP: InputEvent = InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: false,
    };

    fn at(x: f64, y: f64) -> InputEvent {
        let (x, y) = placement().remote_coords(x, y);
        InputEvent::MouseMove { x, y }
    }

    fn to(x: f64, y: f64) -> Motion {
        let (x, y) = placement().remote_coords(x, y);
        Motion::Move { epoch: 5, x, y }
    }

    /// Tablets, pens and tools put the pointer somewhere in one step and
    /// click at once.
    #[test]
    fn a_click_before_the_handoff_lands_where_it_was_made() {
        let mut flow = PointerFlow::default();
        let p = placement();
        assert_eq!(flow.pressed(300.0, 400.0, p, DOWN), Press::Request(1));
        assert_eq!(flow.released(UP), None);
        // The viewer's pointer stays: the click said where.
        assert_eq!(flow.anchor(1, HOST, p, Some((300.0, 400.0))), None);
        let click = vec![at(300.0, 400.0), DOWN, UP];
        assert_eq!(flow.claim(), Some((5, click)));
        assert_eq!(flow.claim(), None);
        // From there on as after any handoff.
        assert_eq!(flow.moved(300.0, 400.0, p), Motion::None);
        assert_eq!(flow.moved(302.0, 400.0, p), to(302.0, 400.0));
        assert_eq!(flow.released(UP), Some(5));
    }

    #[test]
    fn clicks_while_the_hosts_position_is_awaited_land_each_in_its_place() {
        let mut flow = PointerFlow::default();
        let p = placement();
        assert_eq!(flow.moved(300.0, 400.0, p), Motion::Request(1));
        assert_eq!(flow.pressed(300.0, 400.0, p, DOWN), Press::Wait);
        assert_eq!(flow.released(UP), None);
        assert_eq!(flow.pressed(500.0, 600.0, p, DOWN), Press::Wait);
        // Though the pointer has gone elsewhere meanwhile.
        assert_eq!(flow.anchor(1, HOST, p, Some((700.0, 100.0))), None);
        let clicks = vec![at(300.0, 400.0), DOWN, UP, at(500.0, 600.0), DOWN];
        assert_eq!(flow.claim(), Some((5, clicks)));
        // Dragged from the last of them.
        assert_eq!(flow.moved(500.0, 600.0, p), Motion::None);
        assert_eq!(flow.moved(700.0, 100.0, p), to(700.0, 100.0));
    }

    #[test]
    fn a_click_after_a_jump_takes_the_hosts_pointer_along() {
        let mut flow = PointerFlow::default();
        let p = placement();
        flow.moved(10.0, 10.0, p);
        let (x, y) = flow.anchor(1, HOST, p, None).unwrap();
        flow.warp_completed();
        assert_eq!(flow.claim(), None);
        let there = p.remote_coords(x + 200.0, y);
        let jumped = Press::Send {
            epoch: 5,
            to: Some(there),
        };
        assert_eq!(flow.pressed(x + 200.0, y, p, DOWN), jumped);
        let again = Press::Send { epoch: 5, to: None };
        assert_eq!(flow.pressed(x + 200.0, y, p, DOWN), again);
        // Word of the jump itself, should it come later, moves nothing.
        assert_eq!(flow.moved(x + 200.0, y, p), Motion::None);
    }

    #[test]
    fn a_click_is_forgotten_with_its_handoff() {
        let mut flow = PointerFlow::default();
        let p = placement();
        assert_eq!(flow.pressed(300.0, 400.0, p, DOWN), Press::Request(1));
        // The window is resized, say.
        flow.invalidate();
        assert_eq!(flow.released(UP), None);
        assert_eq!(flow.anchor(1, HOST, p, None), None);
        assert_eq!((flow.claim(), flow.epoch()), (None, None));
        // The host's pointer is on another screen: not pulled back.
        assert_eq!(flow.pressed(300.0, 400.0, p, DOWN), Press::Request(2));
        let outside = PointerPosition {
            inside: false,
            ..HOST
        };
        assert_eq!(flow.anchor(2, outside, p, None), None);
        assert_eq!((flow.claim(), flow.epoch()), (None, None));
        // The next click comes alone.
        assert_eq!(flow.pressed(500.0, 600.0, p, DOWN), Press::Request(3));
        assert_eq!(flow.anchor(3, HOST, p, None), None);
        assert_eq!(flow.claim(), Some((5, vec![at(500.0, 600.0), DOWN])));
        // A handoff after it starts with a movement, as ever.
        flow.invalidate();
        assert_eq!(flow.moved(10.0, 10.0, p), Motion::Request(4));
        assert!(flow.anchor(4, HOST, p, None).is_some());
        assert_eq!(flow.claim(), None);
    }

    /// However many wait: no button stays down on the host for a release
    /// that was left out.
    #[test]
    fn every_press_that_waits_has_its_release() {
        let mut flow = PointerFlow::default();
        let p = placement();
        for _ in 0..100 {
            flow.pressed(300.0, 400.0, p, DOWN);
            flow.released(UP);
        }
        flow.anchor(1, HOST, p, None);
        let (_, clicks) = flow.claim().unwrap();
        let count = |event| clicks.iter().filter(|e| **e == event).count();
        assert_eq!(count(DOWN), count(UP));
        assert!((1..100).contains(&count(DOWN)), "{}", count(DOWN));
    }

    #[test]
    fn outside_shared_screen_never_clamps_or_moves_host() {
        let mut flow = PointerFlow::default();
        let p = placement();
        flow.moved(10.0, 10.0, p);
        assert!(
            flow.anchor(
                1,
                PointerPosition {
                    epoch: 1,
                    x: 0,
                    y: 0,
                    inside: false
                },
                p,
                None
            )
            .is_none()
        );
        assert_eq!(flow.epoch(), None);
    }
}
