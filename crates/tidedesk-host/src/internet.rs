//! Opening a direct path to a viewer on another network: the person at the
//! host types the viewer's internet address and presses Open, and both
//! computers punch through their routers (see `tidedesk_core::nat::punch`).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tidedesk_core::nat::{Agent, PunchError};
use tokio::runtime::Handle;

use crate::session::HostState;

/// How long to punch towards a viewer that has not answered yet.
pub const OPEN_WINDOW: Duration = Duration::from_secs(120);

/// The viewer this host opened, or is opening, a path to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedViewer {
    /// The address as typed.
    pub typed: SocketAddr,
    pub state: PathState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathState {
    Opening {
        until: Instant,
    },
    /// The viewer answered, from `peer`.
    Open {
        peer: SocketAddr,
    },
    NoReply,
}

impl ExpectedViewer {
    /// The status line under the address field.
    pub fn describe(&self, now: Instant) -> String {
        match &self.state {
            PathState::Opening { until } => {
                let left = until.saturating_duration_since(now).as_secs();
                format!("Opening a path to {}… ({left} s left)", self.typed)
            }
            PathState::Open { peer } => {
                format!("Path open to {peer}: the viewer can connect now.")
            }
            PathState::NoReply => format!(
                "No reply from {}. Check the address, and connect from the viewer within two \
                 minutes of pressing Open. If either network uses a symmetric NAT, a direct \
                 connection is impossible: use a VPN or port forwarding instead.",
                self.typed
            ),
        }
    }
}

/// Reads the viewer's internet address as its window shows it.
pub fn parse_expected_viewer(text: &str) -> Result<SocketAddr, String> {
    let text = text.trim();
    let addr: SocketAddr = text.parse().map_err(|_| {
        if text.parse::<IpAddr>().is_ok() {
            "Add the port as well, as the viewer shows it: for example 203.0.113.5:40000."
        } else {
            "Type the internet address the viewer shows, for example 203.0.113.5:40000."
        }
        .to_string()
    })?;
    let SocketAddr::V4(v4) = addr else {
        return Err("Internet connections use IPv4 addresses for now.".into());
    };
    let ip = v4.ip();
    let [a, b, ..] = ip.octets();
    // 100.64.0.0/10: carrier-grade NAT, and VPNs such as Tailscale.
    let carrier_or_vpn = a == 100 && b & 0xC0 == 64;
    if ip.is_private() || ip.is_loopback() || ip.is_link_local() || carrier_or_vpn {
        return Err(
            "That is a local network address (or a VPN's, such as Tailscale). \
             A viewer on that network can connect to this computer's addresses above directly."
                .into(),
        );
    }
    if ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast() || v4.port() == 0 {
        return Err("That is not an address a viewer can have.".into());
    }
    Ok(addr)
}

/// Starts punching towards the viewer; the outcome lands in
/// `state.expected_viewer`. Replaces an earlier path being opened.
pub fn open_path(state: &Arc<HostState>, agent: &Arc<Agent>, runtime: &Handle, typed: SocketAddr) {
    let opening = ExpectedViewer {
        typed,
        state: PathState::Opening {
            until: Instant::now() + OPEN_WINDOW,
        },
    };
    let earlier = state.expected_viewer.lock().unwrap().replace(opening);
    if let Some(earlier) = earlier.filter(|e| e.typed != typed) {
        agent.stop_punching(earlier.typed);
    }
    state.changed();

    let (state, agent) = (state.clone(), agent.clone());
    runtime.spawn(async move {
        let outcome = match agent.punch(typed, None, OPEN_WINDOW).await {
            Ok(path) => PathState::Open { peer: path.peer },
            Err(PunchError::NoReply) => PathState::NoReply,
            // Replaced by a newer Open, whose own task reports.
            Err(PunchError::Stopped) => return,
        };
        if let Some(current) = state
            .expected_viewer
            .lock()
            .unwrap()
            .as_mut()
            .filter(|e| e.typed == typed)
        {
            current.state = outcome;
        }
        state.changed();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn expected_viewer_rejects_private_and_ipv6_addresses() {
        assert_eq!(
            parse_expected_viewer(" 203.0.113.5:40000 "),
            Ok(addr("203.0.113.5:40000"))
        );
        for local in [
            "192.168.1.20:40000",
            "10.0.0.5:40000",
            "172.16.3.4:40000",
            "127.0.0.1:40000",
            "169.254.1.1:40000",
            "100.101.102.103:41641", // carrier-grade NAT, and Tailscale
        ] {
            let err = parse_expected_viewer(local).unwrap_err();
            assert!(err.contains("local"), "{local}: {err}");
        }
        let err = parse_expected_viewer("[2001:db8::1]:40000").unwrap_err();
        assert!(err.contains("IPv4"), "{err}");
        let err = parse_expected_viewer("203.0.113.5").unwrap_err();
        assert!(err.contains("port"), "{err}");
        assert!(parse_expected_viewer("my-pc").is_err());
        assert!(parse_expected_viewer("").is_err());
        assert!(parse_expected_viewer("203.0.113.5:0").is_err());
    }

    #[test]
    fn path_status_counts_down_then_reports_the_outcome() {
        let now = Instant::now();
        let typed = addr("203.0.113.5:40000");
        let at = |state| ExpectedViewer { typed, state };

        let opening = at(PathState::Opening {
            until: now + Duration::from_secs(90),
        });
        assert_eq!(
            opening.describe(now),
            "Opening a path to 203.0.113.5:40000… (90 s left)"
        );
        let open = at(PathState::Open {
            peer: addr("203.0.113.5:40123"),
        });
        assert!(
            open.describe(now).contains("203.0.113.5:40123"),
            "{}",
            open.describe(now)
        );
        let silent = at(PathState::NoReply).describe(now);
        assert!(
            silent.starts_with("No reply from 203.0.113.5:40000."),
            "{silent}"
        );
        assert!(silent.contains("symmetric NAT"), "{silent}");
    }
}
