//! Hole punching.
//!
//! Both computers send small datagrams to each other's public address from
//! the port QUIC uses. Each router then has seen traffic go out to the other
//! side, so it lets the other side's packets in, and QUIC can run over that
//! path. Every datagram answered is answered with one of the same size, so
//! punching cannot be used to amplify traffic towards a third party.
//!
//! Packet layout (42 bytes, big endian):
//!
//! | bytes  | field                                                     |
//! |--------|-----------------------------------------------------------|
//! | 0..4   | magic `00 'T' 'D' 'P'`                                    |
//! | 4      | version (1)                                               |
//! | 5      | kind: 0 punch, 1 ack                                      |
//! | 6..8   | reserved, zero                                            |
//! | 8..16  | session: shared by both sides; zero while not known       |
//! | 16..24 | token: random per side; an ack echoes the punch's token   |
//! | 24..40 | ack: the address the punch came from (IPv6 or IPv4-mapped) |
//! | 40..42 | ack: that address's port                                  |

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};

use ring::rand::{SecureRandom, SystemRandom};

use super::PUNCH_MAGIC;

pub const PUNCH_LEN: usize = 42;

/// How often to punch until the other side answers.
pub const PUNCH_INTERVAL: Duration = Duration::from_millis(200);

/// How often to punch once the path is open, keeping both routers' mappings
/// alive until QUIC's own keep-alive takes over.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(2);

/// How long keepalives continue after the path opened.
pub const KEEPALIVE_MAX: Duration = Duration::from_secs(180);

const VERSION: u8 = 1;

pub type SessionId = [u8; 8];

/// A session ID for a new exchange. Never zero, which means "not known".
pub fn new_session() -> SessionId {
    let rng = SystemRandom::new();
    loop {
        let mut session = [0u8; 8];
        rng.fill(&mut session).expect("system RNG failed");
        if session != [0; 8] {
            return session;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Punch,
    Ack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packet {
    pub kind: Kind,
    pub session: SessionId,
    pub token: [u8; 8],
    /// In an ack: where the answered punch came from, i.e. the punching
    /// side's public address as the other side sees it.
    pub observed: Option<SocketAddr>,
}

impl Packet {
    pub fn encode(&self) -> [u8; PUNCH_LEN] {
        let mut packet = [0u8; PUNCH_LEN];
        packet[..4].copy_from_slice(&PUNCH_MAGIC);
        packet[4] = VERSION;
        packet[5] = match self.kind {
            Kind::Punch => 0,
            Kind::Ack => 1,
        };
        packet[8..16].copy_from_slice(&self.session);
        packet[16..24].copy_from_slice(&self.token);
        if let Some(observed) = self.observed {
            let ip = match observed.ip() {
                IpAddr::V4(v4) => v4.to_ipv6_mapped(),
                IpAddr::V6(v6) => v6,
            };
            packet[24..40].copy_from_slice(&ip.octets());
            packet[40..].copy_from_slice(&observed.port().to_be_bytes());
        }
        packet
    }

    /// `None` for anything that is not a version 1 punch or ack.
    pub fn decode(datagram: &[u8]) -> Option<Self> {
        let packet: &[u8; PUNCH_LEN] = datagram.try_into().ok()?;
        if packet[..4] != PUNCH_MAGIC || packet[4] != VERSION {
            return None;
        }
        let kind = match packet[5] {
            0 => Kind::Punch,
            1 => Kind::Ack,
            _ => return None,
        };
        let ip = Ipv6Addr::from(<[u8; 16]>::try_from(&packet[24..40]).ok()?);
        let port = u16::from_be_bytes([packet[40], packet[41]]);
        Some(Self {
            kind,
            session: packet[8..16].try_into().ok()?,
            token: packet[16..24].try_into().ok()?,
            observed: (!ip.is_unspecified()).then(|| SocketAddr::new(ip.to_canonical(), port)),
        })
    }
}

/// Whether a datagram is a punch or ack this version understands.
pub fn is_punch(datagram: &[u8]) -> bool {
    Packet::decode(datagram).is_some()
}

/// A path that answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Punched {
    /// The other side's address as it reached us; its port can differ from
    /// the one we were given.
    pub peer: SocketAddr,
    /// Our public address as the other side saw it.
    pub observed_self: Option<SocketAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Punching,
    Open(Punched),
    /// The window passed without an answer.
    Expired,
}

/// One side of a punch exchange with one peer.
///
/// Both sides run the same exchange: punch every [`PUNCH_INTERVAL`], answer
/// the other side's punches with acks, and count the path as open once an
/// ack echoes our token (so our packets reach the other side and its
/// answers reach us). Then keepalives follow every [`KEEPALIVE_INTERVAL`]
/// for up to [`KEEPALIVE_MAX`].
///
/// Packets are accepted from the peer's IP address on any port: routers may
/// use another port towards us than the one the peer learnt from STUN, and
/// punches then follow the port that answered. With a `session`, packets of
/// other sessions are ignored; without one, the first session seen is taken.
///
/// Pure state machine: time is passed in, and the caller sends what
/// [`Exchange::poll`] and [`Exchange::on_datagram`] return.
pub struct Exchange {
    peer: SocketAddr,
    session: Option<SessionId>,
    token: [u8; 8],
    state: State,
    next_send: Instant,
    /// End of the window while punching; end of keepalives once open.
    until: Instant,
    /// Keepalives are over.
    finished: bool,
}

impl Exchange {
    pub fn new(
        peer: SocketAddr,
        session: Option<SessionId>,
        window: Duration,
        now: Instant,
    ) -> Self {
        let mut token = [0u8; 8];
        SystemRandom::new()
            .fill(&mut token)
            .expect("system RNG failed");
        Self {
            peer,
            session: session.filter(|s| *s != [0; 8]),
            token,
            state: State::Punching,
            next_send: now,
            until: now + window,
            finished: false,
        }
    }

    /// The punch due at `now`, if any.
    pub fn poll(&mut self, now: Instant) -> Option<(SocketAddr, [u8; PUNCH_LEN])> {
        if !self.active(now) || now < self.next_send {
            return None;
        }
        self.next_send = now
            + match self.state {
                State::Open(_) => KEEPALIVE_INTERVAL,
                _ => PUNCH_INTERVAL,
            };
        let punch = Packet {
            kind: Kind::Punch,
            session: self.session.unwrap_or_default(),
            token: self.token,
            observed: None,
        };
        Some((self.peer, punch.encode()))
    }

    /// Handles a datagram from the socket; returns the ack to send, if any.
    pub fn on_datagram(
        &mut self,
        from: SocketAddr,
        datagram: &[u8],
        now: Instant,
    ) -> Option<(SocketAddr, [u8; PUNCH_LEN])> {
        if !self.active(now) || from.ip() != self.peer.ip() {
            return None;
        }
        let packet = Packet::decode(datagram)?;
        let known = packet.session != [0; 8];
        if known && self.session.is_some_and(|ours| ours != packet.session) {
            return None; // another exchange with the same computer
        }
        if packet.kind == Kind::Ack && packet.token != self.token {
            return None;
        }
        // From here on the packet is the peer's: follow its port and session.
        self.peer = from;
        if known {
            self.session = Some(packet.session);
        }
        match packet.kind {
            Kind::Punch => {
                let ack = Packet {
                    kind: Kind::Ack,
                    session: self.session.unwrap_or_default(),
                    token: packet.token,
                    observed: Some(from),
                };
                Some((from, ack.encode()))
            }
            Kind::Ack => {
                if self.state == State::Punching {
                    self.state = State::Open(Punched {
                        peer: from,
                        observed_self: packet.observed,
                    });
                    self.next_send = now + KEEPALIVE_INTERVAL;
                    self.until = now + KEEPALIVE_MAX;
                }
                None
            }
        }
    }

    /// When [`Exchange::poll`] next has work, or `None` once finished.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.finished || self.state == State::Expired {
            return None;
        }
        Some(self.next_send.min(self.until))
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// Moves on when the window or the keepalive time is over; whether the
    /// exchange still sends and answers.
    fn active(&mut self, now: Instant) -> bool {
        if now >= self.until {
            match self.state {
                State::Punching => self.state = State::Expired,
                State::Open(_) => self.finished = true,
                State::Expired => {}
            }
        }
        !self.finished && self.state != State::Expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nat::{is_side_channel, stun};

    const HOST: &str = "198.51.100.20:47800";
    const VIEWER: &str = "203.0.113.5:40000";
    const WINDOW: Duration = Duration::from_secs(120);

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn packet(kind: Kind, session: SessionId, token: [u8; 8]) -> [u8; PUNCH_LEN] {
        Packet {
            kind,
            session,
            token,
            observed: None,
        }
        .encode()
    }

    #[test]
    fn punch_packets_are_neither_quic_nor_stun() {
        let p = packet(Kind::Punch, [0xFF; 8], [0xFF; 8]);
        assert_eq!(p.len(), PUNCH_LEN);
        assert_eq!(
            p[0] & 0xC0,
            0,
            "QUIC always sets the long-header or the fixed bit"
        );
        assert!(!stun::is_stun(&p));
        assert!(is_side_channel(&p));
        assert!(is_punch(&p));
    }

    #[test]
    fn packet_round_trips_including_observed_address() {
        for observed in [None, Some(addr(VIEWER)), Some(addr("[2001:db8::7]:40001"))] {
            let ack = Packet {
                kind: Kind::Ack,
                session: [1, 2, 3, 4, 5, 6, 7, 8],
                token: [9; 8],
                observed,
            };
            assert_eq!(Packet::decode(&ack.encode()), Some(ack));
        }
        let good = packet(Kind::Punch, [1; 8], [2; 8]);
        let mut other_version = good;
        other_version[4] = 2;
        let mut unknown_kind = good;
        unknown_kind[5] = 7;
        assert_eq!(Packet::decode(&other_version), None);
        assert_eq!(Packet::decode(&unknown_kind), None);
        assert_eq!(Packet::decode(&good[..41]), None);
        assert_eq!(Packet::decode(&[good.as_slice(), &[0]].concat()), None);
    }

    #[test]
    fn ack_is_exactly_as_large_as_punch() {
        let t0 = Instant::now();
        let mut host = Exchange::new(addr(VIEWER), None, WINDOW, t0);
        let punch = packet(Kind::Punch, [1; 8], [2; 8]);
        let (_, ack) = host.on_datagram(addr(VIEWER), &punch, t0).unwrap();
        assert_eq!(ack.len(), punch.len());
        // Anything longer is not a punch and gets no answer.
        let padded = [punch.as_slice(), &[0; 100]].concat();
        assert_eq!(host.on_datagram(addr(VIEWER), &padded, t0), None);
        assert_ne!(new_session(), [0; 8]);
    }

    #[test]
    fn exchange_punches_every_200ms_then_keeps_alive_after_ack() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let session = [7; 8];
        let mut viewer = Exchange::new(addr(HOST), Some(session), WINDOW, t0);

        let (to, first) = viewer.poll(t0).expect("punches at once");
        assert_eq!(to, addr(HOST));
        let first = Packet::decode(&first).unwrap();
        assert_eq!(
            (first.kind, first.session, first.observed),
            (Kind::Punch, session, None)
        );
        assert_eq!(viewer.poll(ms(100)), None);
        assert_eq!(viewer.next_deadline(), Some(ms(200)));
        assert!(viewer.poll(ms(200)).is_some());
        assert!(viewer.poll(ms(400)).is_some());

        let ack = Packet {
            kind: Kind::Ack,
            session,
            token: first.token,
            observed: Some(addr(VIEWER)),
        };
        assert_eq!(viewer.on_datagram(addr(HOST), &ack.encode(), ms(450)), None);
        let open = Punched {
            peer: addr(HOST),
            observed_self: Some(addr(VIEWER)),
        };
        assert_eq!(viewer.state(), &State::Open(open));

        assert_eq!(viewer.next_deadline(), Some(ms(2450)));
        assert_eq!(viewer.poll(ms(600)), None, "no more 200 ms punches");
        assert!(viewer.poll(ms(2450)).is_some(), "a keepalive");
        assert_eq!(viewer.next_deadline(), Some(ms(4450)));
        assert_eq!(viewer.poll(ms(450) + KEEPALIVE_MAX), None);
        assert_eq!(viewer.next_deadline(), None, "keepalives are over");
    }

    #[test]
    fn responder_learns_the_peers_real_port_when_ip_matches() {
        let t0 = Instant::now();
        let mut host = Exchange::new(addr(VIEWER), None, WINDOW, t0);
        let (to, _) = host.poll(t0).unwrap();
        assert_eq!(to, addr(VIEWER));

        // The viewer's router uses another port towards the host.
        let real = addr("203.0.113.5:40123");
        let session = [9; 8];
        let (to, ack) = host
            .on_datagram(real, &packet(Kind::Punch, session, [1; 8]), t0)
            .expect("a punch is answered");
        assert_eq!(to, real);
        let ack = Packet::decode(&ack).unwrap();
        assert_eq!(
            (ack.kind, ack.session, ack.token, ack.observed),
            (Kind::Ack, session, [1; 8], Some(real))
        );
        let (to, next) = host.poll(t0 + PUNCH_INTERVAL).unwrap();
        assert_eq!(to, real, "punches follow the port that answered");
        assert_eq!(
            Packet::decode(&next).unwrap().session,
            session,
            "in the viewer's session"
        );

        let stranger = packet(Kind::Punch, session, [2; 8]);
        assert_eq!(
            host.on_datagram(addr("192.0.2.99:40000"), &stranger, t0),
            None
        );
    }

    #[test]
    fn responder_ignores_other_sessions_when_one_is_expected() {
        let t0 = Instant::now();
        let session = [3; 8];
        let mut host = Exchange::new(addr(VIEWER), Some(session), WINDOW, t0);
        let (_, mine) = host.poll(t0).unwrap();
        let mine = Packet::decode(&mine).unwrap();

        let other = packet(Kind::Punch, [4; 8], [1; 8]);
        assert_eq!(host.on_datagram(addr(VIEWER), &other, t0), None);
        host.on_datagram(addr(VIEWER), &packet(Kind::Ack, session, [0xEE; 8]), t0);
        assert_eq!(host.state(), &State::Punching, "an ack must echo our token");

        // A side that does not know the session yet is still answered.
        let unaware = packet(Kind::Punch, [0; 8], [1; 8]);
        let (_, ack) = host.on_datagram(addr(VIEWER), &unaware, t0).unwrap();
        assert_eq!(Packet::decode(&ack).unwrap().session, session);

        host.on_datagram(addr(VIEWER), &packet(Kind::Ack, session, mine.token), t0);
        assert!(matches!(host.state(), State::Open(_)));
    }

    #[test]
    fn exchange_times_out_without_peer() {
        let t0 = Instant::now();
        let mut viewer = Exchange::new(addr(HOST), Some([1; 8]), Duration::from_secs(2), t0);
        let mut sent = 0;
        while let Some(at) = viewer.next_deadline() {
            assert!(
                at <= t0 + Duration::from_secs(2),
                "no work after the window"
            );
            sent += usize::from(viewer.poll(at).is_some());
        }
        assert_eq!(sent, 10, "one punch every 200 ms");
        assert_eq!(viewer.state(), &State::Expired);

        let late = packet(Kind::Punch, [1; 8], [5; 8]);
        let later = t0 + Duration::from_secs(3);
        assert_eq!(viewer.on_datagram(addr(HOST), &late, later), None);
    }
}
