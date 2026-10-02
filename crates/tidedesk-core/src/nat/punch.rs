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

use super::{PUNCH_MAGIC, random_bytes};

pub const PUNCH_LEN: usize = 42;

/// How often to punch until the other side answers.
pub const PUNCH_INTERVAL: Duration = Duration::from_millis(200);

/// How often to punch once the path is open, keeping both routers' mappings
/// alive until QUIC's own keep-alive takes over.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(2);

/// How long keepalives continue after the path opened.
pub const KEEPALIVE_MAX: Duration = Duration::from_secs(180);

/// Longest punching window: bounds the datagrams sent to an address nobody
/// answers from, such as a mistyped one.
pub const MAX_WINDOW: Duration = Duration::from_secs(180);

/// After this long without an answer, punches also guess ports above the
/// peer's: see [`Exchange`].
pub const GUESS_AFTER: Duration = Duration::from_secs(1);
/// How long guessing lasts.
pub const GUESS_FOR: Duration = Duration::from_secs(10);
/// How many ports above the peer's are guessed.
pub const GUESSED_PORTS: u16 = 16;
/// Guessed ports punched per round.
pub const GUESSES_PER_ROUND: usize = 4;

const VERSION: u8 = 1;

pub type SessionId = [u8; 8];

/// A session ID for a new exchange. Never zero, which means "not known".
pub fn new_session() -> SessionId {
    loop {
        let session = random_bytes();
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
/// punches then follow the port that answered.
///
/// A router that gives every destination a new port ("symmetric NAT") sends
/// the peer's packets to us from a port nobody told us, and a router on our
/// side that only lets in what it has sent to drops them. So, while nothing
/// has been heard, punches also go to the [`GUESSED_PORTS`] ports above the
/// given one, [`GUESSES_PER_ROUND`] a round, from [`GUESS_AFTER`] for
/// [`GUESS_FOR`]: such routers mostly hand out ports in turn. That is a few
/// hundred small packets at most, and stops as soon as the peer answers. Two
/// such routers, one on each side, still cannot be punched through. With a `session`, packets of
/// other sessions are ignored. Without one, every session is answered and
/// punches carry the latest, so the other side can retry with a new one.
///
/// Pure state machine: time is passed in, and the caller sends what
/// [`Exchange::poll`] and [`Exchange::on_datagram`] return.
pub struct Exchange {
    peer: SocketAddr,
    session: Option<SessionId>,
    /// The session was given, not taken from the peer.
    fixed_session: bool,
    token: [u8; 8],
    state: State,
    next_send: Instant,
    /// End of the window while punching; end of keepalives once open.
    until: Instant,
    /// Keepalives are over.
    finished: bool,
    /// The address we were given, which guesses count up from.
    given: SocketAddr,
    started: Instant,
    /// The peer has been heard from, so its port is known.
    heard: bool,
    /// Guessed addresses still to punch this round.
    guesses: Vec<SocketAddr>,
    /// The next port above the given one to guess, 1 to [`GUESSED_PORTS`].
    next_guess: u16,
}

impl Exchange {
    pub fn new(
        peer: SocketAddr,
        session: Option<SessionId>,
        window: Duration,
        now: Instant,
    ) -> Self {
        let session = session.filter(|s| *s != [0; 8]);
        Self {
            peer,
            session,
            fixed_session: session.is_some(),
            token: random_bytes(),
            state: State::Punching,
            next_send: now,
            until: now + window.min(MAX_WINDOW),
            finished: false,
            given: peer,
            started: now,
            heard: false,
            guesses: Vec::new(),
            next_guess: 1,
        }
    }

    /// The punch due at `now`, if any.
    pub fn poll(&mut self, now: Instant) -> Option<(SocketAddr, [u8; PUNCH_LEN])> {
        if !self.active(now) {
            return None;
        }
        let punch = Packet {
            kind: Kind::Punch,
            session: self.session.unwrap_or_default(),
            token: self.token,
            observed: None,
        }
        .encode();
        // The rest of this round's guesses, unless the peer was heard.
        if let Some(guess) = self.guesses.pop() {
            if !self.heard {
                return Some((guess, punch));
            }
            self.guesses.clear();
        }
        if now < self.next_send {
            return None;
        }
        self.next_send = now
            + match self.state {
                State::Open(_) => KEEPALIVE_INTERVAL,
                _ => PUNCH_INTERVAL,
            };
        let guessing = self.state == State::Punching
            && !self.heard
            && now >= self.started + GUESS_AFTER
            && now < self.started + GUESS_AFTER + GUESS_FOR;
        if guessing {
            for _ in 0..GUESSES_PER_ROUND {
                let port = u32::from(self.given.port()) + u32::from(self.next_guess);
                if let Ok(port) = u16::try_from(port) {
                    self.guesses.push(SocketAddr::new(self.given.ip(), port));
                }
                self.next_guess = self.next_guess % GUESSED_PORTS + 1;
            }
        }
        Some((self.peer, punch))
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
        if known && self.fixed_session && self.session != Some(packet.session) {
            return None; // another exchange with the same computer
        }
        if packet.kind == Kind::Ack && packet.token != self.token {
            return None;
        }
        // Our own punch, come back through a guessed port or a router.
        if packet.kind == Kind::Punch && packet.token == self.token {
            return None;
        }
        // From here on the packet is the peer's: follow its port and session.
        self.peer = from;
        self.heard = true;
        self.guesses.clear();
        if known {
            self.session = Some(packet.session);
        }
        match packet.kind {
            Kind::Punch => {
                let ack = Packet {
                    kind: Kind::Ack,
                    // Their session if they have one: ours may be older.
                    session: if known {
                        packet.session
                    } else {
                        self.session.unwrap_or_default()
                    },
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
        // Guesses still due go out at once.
        if !self.guesses.is_empty() {
            return Some(self.started);
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
        assert!(Packet::decode(&p).is_some());
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
    fn a_responder_without_a_session_answers_a_retry() {
        // The viewer's first attempt opened the path; it then retries from a
        // new socket with a new session while the host still keeps alive.
        let t0 = Instant::now();
        let mut host = Exchange::new(addr(VIEWER), None, WINDOW, t0);
        let (_, mine) = host.poll(t0).unwrap();
        let mine = Packet::decode(&mine).unwrap();
        host.on_datagram(addr(VIEWER), &packet(Kind::Punch, [1; 8], [1; 8]), t0);
        host.on_datagram(addr(VIEWER), &packet(Kind::Ack, [1; 8], mine.token), t0);
        assert!(matches!(host.state(), State::Open(_)));

        let retry = addr("203.0.113.5:40777");
        let (to, ack) = host
            .on_datagram(retry, &packet(Kind::Punch, [2; 8], [3; 8]), t0)
            .expect("the retry is answered");
        let ack = Packet::decode(&ack).unwrap();
        assert_eq!((to, ack.session, ack.token), (retry, [2; 8], [3; 8]));
        let (to, keepalive) = host.poll(t0 + KEEPALIVE_INTERVAL).unwrap();
        assert_eq!(
            (to, Packet::decode(&keepalive).unwrap().session),
            (retry, [2; 8])
        );
    }

    #[test]
    fn windows_are_capped_at_three_minutes() {
        let t0 = Instant::now();
        let mut viewer = Exchange::new(addr(HOST), Some([1; 8]), Duration::MAX, t0);
        assert!(viewer.poll(t0).is_some());
        assert_eq!(viewer.poll(t0 + MAX_WINDOW), None);
        assert_eq!(viewer.state(), &State::Expired);
    }

    #[test]
    fn exchange_times_out_without_peer() {
        let t0 = Instant::now();
        let mut viewer = Exchange::new(addr(HOST), Some([1; 8]), Duration::from_secs(2), t0);
        let (mut punches, mut guesses) = (0, 0);
        while let Some(at) = viewer.next_deadline() {
            assert!(
                at <= t0 + Duration::from_secs(2),
                "no work after the window"
            );
            if let Some((to, _)) = viewer.poll(at) {
                if to == addr(HOST) {
                    punches += 1;
                } else {
                    guesses += 1;
                }
            }
        }
        assert_eq!(punches, 10, "one punch every 200 ms");
        assert_eq!(
            guesses,
            5 * GUESSES_PER_ROUND,
            "guesses from the first second on"
        );
        assert_eq!(viewer.state(), &State::Expired);

        let late = packet(Kind::Punch, [1; 8], [5; 8]);
        let later = t0 + Duration::from_secs(3);
        assert_eq!(viewer.on_datagram(addr(HOST), &late, later), None);
    }

    /// Every packet an exchange sends until `end`, with when.
    fn sent_until(exchange: &mut Exchange, end: Instant) -> Vec<(Instant, SocketAddr)> {
        let mut sent = Vec::new();
        let mut clock = exchange.started;
        while let Some(at) = exchange.next_deadline() {
            let at = at.max(clock);
            clock = at;
            if at >= end {
                break;
            }
            if let Some((to, _)) = exchange.poll(at) {
                sent.push((at, to));
            }
        }
        sent
    }

    #[test]
    fn unanswered_punches_also_guess_the_next_ports_for_a_while() {
        let t0 = Instant::now();
        let mut viewer = Exchange::new(addr(HOST), Some([1; 8]), MAX_WINDOW, t0);
        let sent = sent_until(&mut viewer, t0 + Duration::from_secs(20));
        let guessed: Vec<_> = sent.iter().filter(|(_, to)| *to != addr(HOST)).collect();
        // Only after the first second, only for GUESS_FOR, only above the port.
        assert!(guessed.iter().all(|(at, _)| *at >= t0 + GUESS_AFTER));
        assert!(
            guessed
                .iter()
                .all(|(at, _)| *at < t0 + GUESS_AFTER + GUESS_FOR)
        );
        assert!(guessed.iter().all(|(_, to)| {
            to.ip() == addr(HOST).ip()
                && to.port() > addr(HOST).port()
                && to.port() <= addr(HOST).port() + GUESSED_PORTS
        }));
        let rounds = (GUESS_FOR.as_millis() / PUNCH_INTERVAL.as_millis()) as usize;
        assert_eq!(
            guessed.len(),
            rounds * GUESSES_PER_ROUND,
            "a bounded number"
        );
        // Every guessed port is tried, again and again.
        let ports: std::collections::HashSet<u16> =
            guessed.iter().map(|(_, to)| to.port()).collect();
        assert_eq!(ports.len(), usize::from(GUESSED_PORTS));
    }

    #[test]
    fn an_exchange_ignores_its_own_punches_coming_back() {
        // A guessed port can be this computer's own, as on the loopback, or a
        // router can loop a packet back: a punch with our token is ours.
        let t0 = Instant::now();
        let mut viewer = Exchange::new(addr("127.0.0.1:50000"), Some([1; 8]), MAX_WINDOW, t0);
        let (_, own) = viewer.poll(t0).unwrap();
        assert_eq!(viewer.on_datagram(addr("127.0.0.1:50003"), &own, t0), None);
        assert_eq!(viewer.state(), &State::Punching);
        let sent = sent_until(&mut viewer, t0 + Duration::from_secs(2));
        assert!(
            sent.iter().any(|(_, to)| *to != addr("127.0.0.1:50000")),
            "still guessing: nothing was heard"
        );
    }

    #[test]
    fn guessing_stops_once_the_peer_is_heard() {
        let t0 = Instant::now();
        let mut viewer = Exchange::new(addr(HOST), Some([1; 8]), MAX_WINDOW, t0);
        let real = addr("198.51.100.20:47807");
        let punch = packet(Kind::Punch, [1; 8], [3; 8]);
        assert!(
            viewer
                .on_datagram(real, &punch, t0 + Duration::from_millis(1500))
                .is_some()
        );
        let sent = sent_until(&mut viewer, t0 + Duration::from_secs(5));
        assert!(
            sent.iter()
                .filter(|(at, _)| *at > t0 + Duration::from_millis(1500))
                .all(|(_, to)| *to == real)
        );
    }

    /// A home router in the simulation below: how it maps this computer's
    /// packets out, and which packets it lets in.
    struct Router {
        ip: std::net::IpAddr,
        /// A new port per destination ("symmetric NAT"), handed out in turn
        /// from `next`; otherwise always `next`.
        symmetric: bool,
        /// A symmetric router that picks each new port at random instead.
        random: bool,
        next: u16,
        ports: std::collections::HashMap<SocketAddr, u16>,
        /// Port-restricted filtering: only from where it has sent to.
        sent_to: std::collections::HashSet<(u16, SocketAddr)>,
    }

    impl Router {
        fn new(ip: &str, symmetric: bool, first_port: u16) -> Self {
            Self {
                ip: ip.parse().unwrap(),
                symmetric,
                random: false,
                next: first_port,
                ports: Default::default(),
                sent_to: Default::default(),
            }
        }

        /// The address a packet to `to` leaves from.
        fn out(&mut self, to: SocketAddr) -> SocketAddr {
            let port = if self.symmetric {
                let (next, random) = (&mut self.next, self.random);
                *self.ports.entry(to).or_insert_with(|| {
                    let port = *next;
                    // A fixed-seed scramble: far from the last port, but
                    // the same in every run.
                    *next = if random {
                        (port.wrapping_mul(40_503).wrapping_add(12_345) | 1024).max(1024)
                    } else {
                        port + 1
                    };
                    port
                })
            } else {
                self.next
            };
            self.sent_to.insert((port, to));
            SocketAddr::new(self.ip, port)
        }

        fn lets_in(&self, from: SocketAddr, port: u16) -> bool {
            self.sent_to.contains(&(port, from))
        }
    }

    /// Runs two exchanges behind two routers for `limit` of simulated time.
    fn simulate(
        a: &mut Exchange,
        router_a: &mut Router,
        b: &mut Exchange,
        router_b: &mut Router,
        limit: Duration,
    ) {
        let t0 = Instant::now();
        let mut now = t0;
        while now < t0 + limit {
            for side in 0..2 {
                let (from, router_from, to, router_to) = if side == 0 {
                    (&mut *a, &mut *router_a, &mut *b, &mut *router_b)
                } else {
                    (&mut *b, &mut *router_b, &mut *a, &mut *router_a)
                };
                while let Some((target, packet)) = from.poll(now) {
                    let source = router_from.out(target);
                    if target.ip() != router_to.ip || !router_to.lets_in(source, target.port()) {
                        continue;
                    }
                    if let Some((back, ack)) = to.on_datagram(source, &packet, now) {
                        let source_back = router_to.out(back);
                        if back.ip() == router_from.ip
                            && router_from.lets_in(source_back, back.port())
                        {
                            from.on_datagram(source_back, &ack, now);
                        }
                    }
                }
            }
            now += Duration::from_millis(50);
        }
    }

    const SERVICE: &str = "192.0.2.1:47900";

    #[test]
    fn a_symmetric_router_on_one_side_is_punched_through() {
        // The host's router gave the connection service port 50000; the
        // viewer's next packet leaves from 50001.
        let mut host_router = Router::new("198.51.100.20", true, 50000);
        let introduced_host = host_router.out(addr(SERVICE));
        let mut viewer_router = Router::new("203.0.113.5", false, 40000);
        let introduced_viewer = viewer_router.out(addr(SERVICE));

        let mut host = Exchange::new(introduced_viewer, None, MAX_WINDOW, Instant::now());
        let mut viewer = Exchange::new(introduced_host, Some([1; 8]), MAX_WINDOW, Instant::now());
        simulate(
            &mut viewer,
            &mut viewer_router,
            &mut host,
            &mut host_router,
            Duration::from_secs(15),
        );
        match viewer.state() {
            State::Open(path) => assert_eq!(path.peer, addr("198.51.100.20:50001")),
            other => panic!("the viewer did not get through: {other:?}"),
        }
        assert!(matches!(host.state(), State::Open(_)));
    }

    #[test]
    fn routers_that_pick_ports_at_random_still_cannot_be_punched_through() {
        // Symmetric on both sides, picking at random: nothing to guess.
        let mut host_router = Router::new("198.51.100.20", true, 50000);
        host_router.random = true;
        let introduced_host = host_router.out(addr(SERVICE));
        let mut viewer_router = Router::new("203.0.113.5", true, 40000);
        viewer_router.random = true;
        let introduced_viewer = viewer_router.out(addr(SERVICE));

        let mut host = Exchange::new(
            introduced_viewer,
            None,
            Duration::from_secs(15),
            Instant::now(),
        );
        let mut viewer = Exchange::new(
            introduced_host,
            Some([1; 8]),
            Duration::from_secs(15),
            Instant::now(),
        );
        simulate(
            &mut viewer,
            &mut viewer_router,
            &mut host,
            &mut host_router,
            Duration::from_secs(16),
        );
        assert_eq!(
            viewer.state(),
            &State::Expired,
            "this needs IPv6, port mapping or a VPN"
        );

        // The same on one side only, when the other only lets in what it sent to.
        let mut host_router = Router::new("198.51.100.20", true, 50000);
        host_router.random = true;
        let introduced_host = host_router.out(addr(SERVICE));
        let mut viewer_router = Router::new("203.0.113.5", false, 40000);
        let introduced_viewer = viewer_router.out(addr(SERVICE));
        let mut host = Exchange::new(
            introduced_viewer,
            None,
            Duration::from_secs(15),
            Instant::now(),
        );
        let mut viewer = Exchange::new(
            introduced_host,
            Some([1; 8]),
            Duration::from_secs(15),
            Instant::now(),
        );
        simulate(
            &mut viewer,
            &mut viewer_router,
            &mut host,
            &mut host_router,
            Duration::from_secs(16),
        );
        assert_eq!(viewer.state(), &State::Expired);
    }
}
