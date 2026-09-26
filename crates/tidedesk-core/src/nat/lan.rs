//! Finding a host on the local network by its device ID.
//!
//! A viewer given a device ID asks its own network for it with a broadcast
//! query, alongside asking the connection service. The host with that ID
//! answers from the port QUIC uses, so the answer's source address is the one
//! to dial. Nothing here vouches for the host: the viewer still checks that
//! the certificate it is shown hashes to the ID, so a computer answering in
//! the host's place can only send the viewer somewhere that check fails.
//!
//! Query and answer are the same 24 bytes, so answering cannot be used to
//! amplify traffic. Layout (big endian):
//!
//! | bytes  | field                                          |
//! |--------|------------------------------------------------|
//! | 0..4   | magic `00 'T' 'D' 'L'`                         |
//! | 4      | version (1)                                    |
//! | 5      | kind: 0 query, 1 answer                        |
//! | 6..8   | reserved, zero                                 |
//! | 8..16  | the device ID asked for                        |
//! | 16..24 | nonce: random per search; an answer echoes it  |

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tidedesk_rendezvous_proto::DeviceId;

use super::{LAN_MAGIC, NotPublic, check_public, random_bytes};

pub const LAN_LEN: usize = 24;

/// How long a search waits for an answer. On a local network one comes back
/// within milliseconds; the repeats cover lost broadcasts, common on Wi-Fi.
pub const SEARCH_WINDOW: Duration = Duration::from_secs(3);

/// When a search sends its query, counted from its start.
const QUERY_AT: [Duration; 4] = [
    Duration::ZERO,
    Duration::from_millis(250),
    Duration::from_millis(750),
    Duration::from_millis(1750),
];

const VERSION: u8 = 1;

pub type Nonce = [u8; 8];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Query,
    Answer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packet {
    pub kind: Kind,
    pub device_id: DeviceId,
    pub nonce: Nonce,
}

impl Packet {
    pub fn encode(&self) -> [u8; LAN_LEN] {
        let mut packet = [0u8; LAN_LEN];
        packet[..4].copy_from_slice(&LAN_MAGIC);
        packet[4] = VERSION;
        packet[5] = match self.kind {
            Kind::Query => 0,
            Kind::Answer => 1,
        };
        packet[8..16].copy_from_slice(&self.device_id.0);
        packet[16..24].copy_from_slice(&self.nonce);
        packet
    }

    /// `None` for anything that is not a version 1 query or answer.
    pub fn decode(datagram: &[u8]) -> Option<Self> {
        let packet: &[u8; LAN_LEN] = datagram.try_into().ok()?;
        if packet[..4] != LAN_MAGIC || packet[4] != VERSION {
            return None;
        }
        let kind = match packet[5] {
            0 => Kind::Query,
            1 => Kind::Answer,
            _ => return None,
        };
        Some(Self {
            kind,
            device_id: DeviceId(packet[8..16].try_into().ok()?),
            nonce: packet[16..24].try_into().ok()?,
        })
    }
}

/// The host's side: what to send back to `from`, if `datagram` is a query
/// for `own` from a computer on this host's local network.
pub fn answer(own: DeviceId, from: SocketAddr, datagram: &[u8]) -> Option<[u8; LAN_LEN]> {
    let query = Packet::decode(datagram)?;
    let local = check_public(from) == Err(NotPublic::Local);
    if query.kind != Kind::Query || query.device_id != own || !local {
        return None;
    }
    let answer = Packet {
        kind: Kind::Answer,
        ..query
    };
    Some(answer.encode())
}

/// The viewer's side: asks `targets` (broadcast addresses) for a device ID
/// until the host answers or [`SEARCH_WINDOW`] passes.
///
/// Pure state machine, like [`super::stun::Discovery`].
pub struct Search {
    query: Packet,
    targets: Vec<SocketAddr>,
    started: Instant,
    /// How many of [`QUERY_AT`] have been sent.
    sent: usize,
    found: Option<SocketAddr>,
    gave_up: bool,
}

impl Search {
    pub fn new(device_id: DeviceId, targets: Vec<SocketAddr>, now: Instant) -> Self {
        Self {
            query: Packet {
                kind: Kind::Query,
                device_id,
                nonce: random_bytes(),
            },
            targets,
            started: now,
            sent: 0,
            found: None,
            gave_up: false,
        }
    }

    /// Queries due at `now`, one per target.
    pub fn poll(&mut self, now: Instant) -> Vec<(SocketAddr, [u8; LAN_LEN])> {
        if self.found.is_some() || self.gave_up {
            return Vec::new();
        }
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed >= SEARCH_WINDOW {
            self.gave_up = true;
            return Vec::new();
        }
        let due = QUERY_AT.iter().filter(|at| **at <= elapsed).count();
        if due <= self.sent {
            return Vec::new();
        }
        // Rounds missed while busy go out once, not in a burst.
        self.sent = due;
        let query = self.query.encode();
        self.targets.iter().map(|&to| (to, query)).collect()
    }

    /// Takes the first answer to this search: its source is the host.
    pub fn on_datagram(&mut self, from: SocketAddr, datagram: &[u8]) {
        let answer = Packet {
            kind: Kind::Answer,
            ..self.query
        };
        if self.found.is_none() && !self.gave_up && Packet::decode(datagram) == Some(answer) {
            self.found = Some(from);
        }
    }

    pub fn found(&self) -> Option<SocketAddr> {
        self.found
    }

    /// When [`Search::poll`] next has work; `None` once the host is found or
    /// the search gave up.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.found.is_some() || self.gave_up {
            return None;
        }
        let next = QUERY_AT.get(self.sent).copied().unwrap_or(SEARCH_WINDOW);
        Some(self.started + next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nat::{is_side_channel, punch, stun};

    const ID: DeviceId = DeviceId([0x1A, 0x2B, 0x3C, 0x4D, 0x5E, 0x6F, 0x7A, 0x8B]);

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn query(device_id: DeviceId, nonce: Nonce) -> [u8; LAN_LEN] {
        Packet {
            kind: Kind::Query,
            device_id,
            nonce,
        }
        .encode()
    }

    #[test]
    fn packets_are_side_channel_and_differ_from_stun_and_punch() {
        let q = query(ID, [7; 8]);
        assert_eq!(&q[..4], &LAN_MAGIC);
        assert_eq!(q[0], 0, "never QUIC: the fixed bit is clear");
        assert!(is_side_channel(&q));
        assert!(!stun::is_stun(&q));
        assert!(!tidedesk_rendezvous_proto::is_signal(&q));
        assert_eq!(punch::Packet::decode(&q), None);

        // Other side-channel packets of the same length are not LAN packets.
        let mut punch_like = q;
        punch_like[..4].copy_from_slice(&crate::nat::PUNCH_MAGIC);
        assert_eq!(Packet::decode(&punch_like), None);
        let mut signal_like = q;
        signal_like[..4].copy_from_slice(&tidedesk_rendezvous_proto::MAGIC);
        assert_eq!(Packet::decode(&signal_like), None);
    }

    #[test]
    fn query_and_answer_have_the_same_size_and_round_trip() {
        for kind in [Kind::Query, Kind::Answer] {
            let packet = Packet {
                kind,
                device_id: ID,
                nonce: [1, 2, 3, 4, 5, 6, 7, 8],
            };
            let bytes = packet.encode();
            assert_eq!(bytes.len(), LAN_LEN);
            assert_eq!(&bytes[6..8], &[0, 0], "reserved");
            assert_eq!(Packet::decode(&bytes), Some(packet));
        }
        let q = query(ID, [3; 8]);
        let a = answer(ID, addr("192.168.1.20:50000"), &q).expect("an answer");
        assert_eq!(a.len(), q.len(), "never larger than the query");

        let mut other_version = q;
        other_version[4] = 2;
        let mut other_kind = q;
        other_kind[5] = 2;
        for bad in [
            &other_version[..],
            &other_kind[..],
            &q[..23],
            &[&q[..], &[0]].concat(),
        ] {
            assert_eq!(Packet::decode(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn host_answers_only_queries_for_its_own_id() {
        let viewer = addr("192.168.1.20:50000");
        let a = answer(ID, viewer, &query(ID, [5; 8])).unwrap();
        let expected = Packet {
            kind: Kind::Answer,
            device_id: ID,
            nonce: [5; 8],
        };
        assert_eq!(Packet::decode(&a), Some(expected), "echoes ID and nonce");

        assert_eq!(answer(ID, viewer, &query(DeviceId([9; 8]), [5; 8])), None);
        // Hosts never answer answers, so two of them cannot keep each other busy.
        assert_eq!(answer(ID, viewer, &expected.encode()), None);
        assert_eq!(answer(ID, viewer, b"\x00TDL garbage"), None);
    }

    #[test]
    fn host_answers_only_local_ipv4_sources() {
        let q = query(ID, [5; 8]);
        for local in [
            "192.168.1.20:50000",
            "10.1.2.3:50000",
            "172.16.0.9:50000",
            "169.254.3.4:50000",
            "127.0.0.1:50000",
            "100.64.1.2:50000",
        ] {
            assert!(answer(ID, addr(local), &q).is_some(), "{local}");
        }
        for elsewhere in ["203.0.113.5:50000", "[fe80::1]:50000", "0.0.0.0:50000"] {
            assert_eq!(answer(ID, addr(elsewhere), &q), None, "{elsewhere}");
        }
    }

    #[test]
    fn search_repeats_the_query_to_every_target_then_gives_up() {
        let t0 = Instant::now();
        let targets = vec![addr("255.255.255.255:47800"), addr("192.168.1.255:47800")];
        let mut search = Search::new(ID, targets.clone(), t0);

        let first = search.poll(t0);
        assert_eq!(first.iter().map(|(to, _)| *to).collect::<Vec<_>>(), targets);
        let sent = Packet::decode(&first[0].1).unwrap();
        assert_eq!((sent.kind, sent.device_id), (Kind::Query, ID));
        assert_eq!(first[1].1, first[0].1, "one query, to every target");
        assert!(search.poll(t0).is_empty(), "nothing due twice");

        let mut rounds = 1;
        let mut now = t0;
        while let Some(next) = search.next_deadline() {
            assert!(next > now, "deadlines move forward");
            now = next;
            let due = search.poll(now);
            if !due.is_empty() {
                assert_eq!(due.len(), targets.len());
                assert!(due.iter().all(|(_, q)| *q == first[0].1), "same nonce");
                rounds += 1;
            }
        }
        assert_eq!(rounds, QUERY_AT.len());
        assert_eq!(now, t0 + SEARCH_WINDOW);
        assert_eq!(search.found(), None);
        assert!(search.poll(now + SEARCH_WINDOW).is_empty());

        // Rounds missed while busy go out once, not in a burst.
        let mut late = Search::new(ID, targets.clone(), t0);
        late.poll(t0);
        assert_eq!(late.poll(t0 + Duration::from_secs(2)).len(), targets.len());
        assert_eq!(late.next_deadline(), Some(t0 + SEARCH_WINDOW));
    }

    #[test]
    fn search_takes_the_first_answer_with_its_id_and_nonce() {
        let t0 = Instant::now();
        let mut search = Search::new(ID, vec![addr("255.255.255.255:47800")], t0);
        let sent = search.poll(t0)[0].1;
        let nonce = Packet::decode(&sent).unwrap().nonce;
        let host = addr("192.168.1.30:47800");
        let reply = |device_id, nonce| {
            Packet {
                kind: Kind::Answer,
                device_id,
                nonce,
            }
            .encode()
        };

        search.on_datagram(host, &reply(DeviceId([9; 8]), nonce));
        search.on_datagram(host, &reply(ID, [0; 8]));
        // Some systems hand a computer its own broadcast back.
        search.on_datagram(addr("192.168.1.20:50000"), &sent);
        assert_eq!(search.found(), None);
        assert!(search.next_deadline().is_some());

        search.on_datagram(host, &reply(ID, nonce));
        search.on_datagram(addr("192.168.1.31:47800"), &reply(ID, nonce));
        assert_eq!(search.found(), Some(host), "the first answer counts");
        assert_eq!(search.next_deadline(), None);
        assert!(search.poll(t0 + Duration::from_secs(1)).is_empty());
    }
}
