//! The viewer never moves its cursor in response to passive host updates.
use crate::layout::Placement;
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

pub struct PointerFlow {
    phase: Phase,
    next_request: u64,
}

impl Default for PointerFlow {
    fn default() -> Self {
        Self {
            phase: Phase::NeedsAnchor,
            next_request: 0,
        }
    }
}

impl PointerFlow {
    pub fn invalidate(&mut self) {
        self.phase = Phase::NeedsAnchor;
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
