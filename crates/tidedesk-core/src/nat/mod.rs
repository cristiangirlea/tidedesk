//! Building blocks for direct connections across NAT routers.
//!
//! TideDesk never relays a session. To reach a computer behind a home router,
//! both sides learn their public address (STUN) and send each other small
//! "punch" datagrams so that each router lets the other side's packets in.
//! QUIC then runs over the opened path. All of this shares the one UDP socket
//! QUIC listens on, because a router's mapping belongs to that socket's port.

pub mod socket;
pub mod stun;

pub use socket::{RawDatagram, SharedSocket};
pub use stun::{NatKind, PublicEndpoint};

/// RFC 5389 magic cookie, found at bytes 4..8 of every STUN message.
pub const STUN_MAGIC_COOKIE: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];

/// First four bytes of every TideDesk punch packet. A zero first byte keeps it
/// apart from QUIC (whose fixed bit is always set towards TideDesk endpoints)
/// and the rest keeps it apart from STUN.
pub const PUNCH_MAGIC: [u8; 4] = [0x00, b'T', b'D', b'P'];

/// Whether a datagram belongs to the side channel rather than to QUIC.
///
/// Matching is by explicit magic only. The QUIC fixed bit cannot be trusted:
/// quinn clears it at random on packets to peers that allow "greasing", which
/// is why endpoints on a [`SharedSocket`] disable greasing (see `net.rs`).
pub(crate) fn is_side_channel(datagram: &[u8]) -> bool {
    stun::is_stun(datagram) || datagram.starts_with(&PUNCH_MAGIC)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_stun_and_punch_datagrams_are_side_channel() {
        let mut stun = [0u8; 20];
        stun[1] = 0x01;
        stun[4..8].copy_from_slice(&STUN_MAGIC_COOKIE);
        assert!(is_side_channel(&stun));
        assert!(is_side_channel(&[0x00, b'T', b'D', b'P', 1]));

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
    }
}
