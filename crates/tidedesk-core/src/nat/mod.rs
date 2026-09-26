//! Building blocks for direct connections across NAT routers.
//!
//! TideDesk never relays a session. To reach a computer behind a home router,
//! both sides learn their public address (STUN) and send each other small
//! "punch" datagrams so that each router lets the other side's packets in.
//! QUIC then runs over the opened path. All of this shares the one UDP socket
//! QUIC listens on, because a router's mapping belongs to that socket's port.

pub mod agent;
pub mod lan;
pub mod punch;
pub mod signal;
pub mod socket;
pub mod stun;

pub use agent::{Agent, AgentStatus, PublicStatus, PunchError};
pub use punch::{Punched, SessionId};
pub use socket::{RawDatagram, SharedSocket};

use std::net::SocketAddr;
pub use stun::{NatKind, PublicEndpoint};
pub use tidedesk_rendezvous_proto::DeviceId;

/// RFC 5389 magic cookie, found at bytes 4..8 of every STUN message.
pub const STUN_MAGIC_COOKIE: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];

/// First four bytes of every TideDesk punch packet. A zero first byte keeps it
/// apart from QUIC (whose fixed bit is always set towards TideDesk endpoints)
/// and the rest keeps it apart from STUN.
pub const PUNCH_MAGIC: [u8; 4] = [0x00, b'T', b'D', b'P'];

/// First four bytes of every local-network device-ID query and answer (see
/// [`lan`]); apart from QUIC and STUN for the same reasons as a punch.
pub const LAN_MAGIC: [u8; 4] = [0x00, b'T', b'D', b'L'];

/// Why an address cannot be another computer's address on the internet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotPublic {
    /// Internet connections use IPv4 for now.
    Ipv6,
    /// A private, loopback, link-local or carrier-grade NAT address
    /// (100.64.0.0/10, which VPNs such as Tailscale also use): reachable
    /// directly from its own network, not through a router.
    Local,
    /// Unspecified, broadcast, multicast or port 0.
    Unusable,
}

/// Checks that `addr` can be another computer's internet address.
pub fn check_public(addr: SocketAddr) -> Result<(), NotPublic> {
    let SocketAddr::V4(v4) = addr else {
        return Err(NotPublic::Ipv6);
    };
    let ip = v4.ip();
    let [a, b, ..] = ip.octets();
    let carrier_grade = a == 100 && b & 0xC0 == 64;
    if ip.is_private() || ip.is_loopback() || ip.is_link_local() || carrier_grade {
        return Err(NotPublic::Local);
    }
    if ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast() || v4.port() == 0 {
        return Err(NotPublic::Unusable);
    }
    Ok(())
}

/// Random bytes for IDs and tokens.
pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("system RNG failed");
    bytes
}

/// Whether a datagram belongs to the side channel rather than to QUIC.
///
/// Matching is by explicit magic only. The QUIC fixed bit cannot be trusted:
/// quinn clears it at random on packets to peers that allow "greasing", which
/// is why endpoints on a [`SharedSocket`] disable greasing (see `net.rs`).
pub(crate) fn is_side_channel(datagram: &[u8]) -> bool {
    stun::is_stun(datagram)
        || datagram.starts_with(&PUNCH_MAGIC)
        || datagram.starts_with(&LAN_MAGIC)
        || tidedesk_rendezvous_proto::is_signal(datagram)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_ipv4_addresses_pass() {
        let check = |s: &str| check_public(s.parse().unwrap());
        assert_eq!(check("203.0.113.5:40000"), Ok(()));
        assert_eq!(check("8.8.8.8:3478"), Ok(()));
        for local in [
            "192.168.1.20:40000",
            "10.0.0.5:40000",
            "172.16.3.4:40000",
            "127.0.0.1:40000",
            "169.254.1.1:40000",
            "100.101.102.103:41641",
        ] {
            assert_eq!(check(local), Err(NotPublic::Local), "{local}");
        }
        assert_eq!(
            check("100.128.0.1:40000"),
            Ok(()),
            "just outside 100.64.0.0/10"
        );
        assert_eq!(check("[2001:db8::1]:40000"), Err(NotPublic::Ipv6));
        for unusable in [
            "0.0.0.0:40000",
            "255.255.255.255:40000",
            "224.0.0.1:40000",
            "203.0.113.5:0",
        ] {
            assert_eq!(check(unusable), Err(NotPublic::Unusable), "{unusable}");
        }
    }

    #[test]
    fn only_stun_and_punch_datagrams_are_side_channel() {
        let mut stun = [0u8; 20];
        stun[1] = 0x01;
        stun[4..8].copy_from_slice(&STUN_MAGIC_COOKIE);
        assert!(is_side_channel(&stun));
        assert!(is_side_channel(&[0x00, b'T', b'D', b'P', 1]));
        assert!(is_side_channel(&[0x00, b'T', b'D', b'L', 1]));

        // QUIC long and short headers always have the top or fixed bit set.
        let mut quic_long = stun;
        quic_long[0] = 0xC3;
        let mut quic_short = stun;
        quic_short[0] = 0x41;
        assert!(!is_side_channel(&quic_long));
        assert!(!is_side_channel(&quic_short));
        // Too short to be STUN, wrong magic for a punch.
        assert!(!is_side_channel(&stun[..19]));
        assert!(!is_side_channel(b"\x00TDX"));
        // Rendezvous service messages.
        let hello =
            tidedesk_rendezvous_proto::encode(&tidedesk_rendezvous_proto::ToServer::hello([1; 8]));
        assert!(is_side_channel(&hello));
    }
}
