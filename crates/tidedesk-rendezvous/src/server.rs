//! The service's rules, without I/O: a datagram in, datagrams out.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use tidedesk_rendezvous_proto::{
    Challenge, DeviceId, ErrorCode, FromServer, HELLO_PADDING, Nonce, Session, ToServer, Token,
    decode, encode, public_key_from_cert, verify_registration,
};

/// Challenges change every window and are accepted for this one and the
/// previous one: between 10 and 20 seconds.
const CHALLENGE_WINDOW: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// How long a registration lasts without a refresh.
    pub ttl: Duration,
    pub max_hosts: usize,
    /// Live registrations from one IP address: a household or office has a
    /// few hosts, not thousands.
    pub max_hosts_per_ip: usize,
    /// Datagrams per second allowed from one IP address, and the burst.
    pub rate: f64,
    pub burst: f64,
    /// Addresses whose rate is tracked at once; datagrams from further new
    /// addresses are dropped until older ones go quiet.
    pub max_tracked_addresses: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(75),
            max_hosts: 10_000,
            max_hosts_per_ip: 32,
            rate: 10.0,
            burst: 20.0,
            max_tracked_addresses: 100_000,
        }
    }
}

/// Which of the two listening ports a datagram came in on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Port {
    Main,
    /// Only answers Hello: the address seen here, compared with the one seen
    /// on the main port, tells a symmetric NAT.
    Alt,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counters {
    pub registered: u64,
    pub lookups: u64,
    pub not_found: u64,
    pub rejected: u64,
    pub rate_limited: u64,
}

pub struct Server {
    config: Config,
    started: Instant,
    hosts: HashMap<DeviceId, Host>,
    buckets: HashMap<IpAddr, Bucket>,
    rng: SystemRandom,
    /// Makes challenges and sessions: only this service can produce them,
    /// and it needs to remember neither.
    key: hmac::Key,
    counters: Counters,
}

struct Host {
    addr: SocketAddr,
    public_key: Vec<u8>,
    token: Token,
    expires: Instant,
}

/// Token bucket for one IP address.
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Server {
    pub fn new(config: Config) -> Self {
        let rng = SystemRandom::new();
        let key = hmac::Key::generate(hmac::HMAC_SHA256, &rng).expect("system RNG failed");
        Self {
            config,
            started: Instant::now(),
            hosts: HashMap::new(),
            buckets: HashMap::new(),
            rng,
            key,
            counters: Counters::default(),
        }
    }

    /// Handles one datagram; returns the datagrams to send, and where.
    pub fn handle(
        &mut self,
        from: SocketAddr,
        port: Port,
        datagram: &[u8],
        now: Instant,
    ) -> Vec<(SocketAddr, Vec<u8>)> {
        if !self.allow(from.ip(), now) {
            self.counters.rate_limited += 1;
            return Vec::new();
        }
        let Some(message) = decode::<ToServer>(datagram) else {
            return Vec::new();
        };
        let reply = match (port, message) {
            (_, ToServer::Hello { nonce, padding }) => {
                if padding.len() < HELLO_PADDING {
                    return Vec::new(); // an answer larger than the question
                }
                FromServer::Challenge {
                    nonce,
                    challenge: self.challenge_in(from, self.window(now)),
                    reflexive: from,
                }
            }
            (Port::Alt, _) => return Vec::new(),
            (_, message) if !self.proven(from, &message, now) => {
                self.counters.rejected += 1;
                FromServer::Error {
                    code: ErrorCode::UnknownChallenge,
                }
            }
            (
                Port::Main,
                ToServer::Register {
                    device_id,
                    cert_der,
                    challenge,
                    signature,
                },
            ) => self.register(from, device_id, &cert_der, challenge, &signature, now),
            (Port::Main, ToServer::Refresh { device_id, token }) => {
                self.refresh(from, device_id, token, now)
            }
            (
                Port::Main,
                ToServer::Lookup {
                    device_id, nonce, ..
                },
            ) => {
                return self.lookup(from, device_id, nonce, now);
            }
        };
        vec![(from, encode(&reply))]
    }

    /// Forgets expired registrations and quiet addresses.
    pub fn sweep(&mut self, now: Instant) {
        self.hosts.retain(|_, host| host.expires > now);
        // A bucket idle this long is full again: the same as no bucket.
        let refilled = Duration::from_secs_f64(self.config.burst / self.config.rate);
        self.buckets
            .retain(|_, bucket| now.saturating_duration_since(bucket.at) < refilled);
    }

    pub fn hosts(&self) -> usize {
        self.hosts.len()
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    fn allow(&mut self, ip: IpAddr, now: Instant) -> bool {
        let Config { rate, burst, .. } = self.config;
        if !self.buckets.contains_key(&ip)
            && self.buckets.len() >= self.config.max_tracked_addresses
        {
            self.sweep(now);
            if self.buckets.len() >= self.config.max_tracked_addresses {
                return false; // flooded by new addresses: they wait
            }
        }
        let bucket = self.buckets.entry(ip).or_insert(Bucket {
            tokens: burst,
            at: now,
        });
        let refill = now.saturating_duration_since(bucket.at).as_secs_f64() * rate;
        bucket.tokens = (bucket.tokens + refill).min(burst);
        bucket.at = now;
        if bucket.tokens < 1.0 {
            return false;
        }
        bucket.tokens -= 1.0;
        true
    }

    fn window(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.started).as_secs() / CHALLENGE_WINDOW.as_secs()
    }

    /// The challenge for `to` in a window: an HMAC, so nothing is stored and
    /// only an address that received it can send it back.
    fn challenge_in(&self, to: SocketAddr, window: u64) -> Challenge {
        let tag = hmac::sign(
            &self.key,
            &[
                b"challenge".as_slice(),
                to.to_string().as_bytes(),
                &window.to_be_bytes(),
            ]
            .concat(),
        );
        tag.as_ref()[..16]
            .try_into()
            .expect("HMAC-SHA256 is 32 bytes")
    }

    /// Whether a message that needs one carries a current challenge for its
    /// source address. Refreshes are proven by their token instead.
    fn proven(&self, from: SocketAddr, message: &ToServer, now: Instant) -> bool {
        let challenge = match message {
            ToServer::Register { challenge, .. } | ToServer::Lookup { challenge, .. } => challenge,
            ToServer::Hello { .. } | ToServer::Refresh { .. } => return true,
        };
        let window = self.window(now);
        [window, window.saturating_sub(1)]
            .into_iter()
            .any(|w| self.challenge_in(from, w) == *challenge)
    }

    fn register(
        &mut self,
        from: SocketAddr,
        device_id: DeviceId,
        cert_der: &[u8],
        challenge: Challenge,
        signature: &[u8],
        now: Instant,
    ) -> FromServer {
        if !verify_registration(cert_der, &device_id, &challenge, signature) {
            return self.reject(ErrorCode::BadSignature);
        }
        let public_key = public_key_from_cert(cert_der)
            .expect("checked by verify_registration")
            .to_vec();

        // An ID comes from a certificate hash, so another key for a live ID
        // means a hash collision: never let it take the registration over.
        let live = self.hosts.get(&device_id).filter(|host| host.expires > now);
        if live.is_some_and(|host| host.public_key != public_key) {
            return self.reject(ErrorCode::IdTaken);
        }
        if live.is_none() {
            let from_same_ip = self
                .hosts
                .values()
                .filter(|host| host.expires > now && host.addr.ip() == from.ip())
                .count();
            if from_same_ip >= self.config.max_hosts_per_ip {
                return self.reject(ErrorCode::Full);
            }
            if self.hosts.len() >= self.config.max_hosts {
                self.sweep(now);
                if self.hosts.len() >= self.config.max_hosts {
                    return self.reject(ErrorCode::Full);
                }
            }
        }
        let token = self.random();
        self.hosts.insert(
            device_id,
            Host {
                addr: from,
                public_key,
                token,
                expires: now + self.config.ttl,
            },
        );
        self.counters.registered += 1;
        FromServer::Registered {
            device_id,
            token,
            reflexive: from,
            ttl_secs: self.ttl_secs(),
        }
    }

    fn reject(&mut self, code: ErrorCode) -> FromServer {
        self.counters.rejected += 1;
        FromServer::Error { code }
    }

    fn refresh(
        &mut self,
        from: SocketAddr,
        device_id: DeviceId,
        token: Token,
        now: Instant,
    ) -> FromServer {
        let ttl = self.config.ttl;
        match self.hosts.get_mut(&device_id) {
            Some(host) if host.token == token && host.expires > now => {
                host.addr = from; // the router may have moved the host
                host.expires = now + ttl;
                FromServer::Registered {
                    device_id,
                    token,
                    reflexive: from,
                    ttl_secs: self.ttl_secs(),
                }
            }
            _ => FromServer::Error {
                code: ErrorCode::NotRegistered,
            },
        }
    }

    /// Introduces the viewer and the host to each other.
    fn lookup(
        &mut self,
        from: SocketAddr,
        device_id: DeviceId,
        nonce: Nonce,
        now: Instant,
    ) -> Vec<(SocketAddr, Vec<u8>)> {
        self.counters.lookups += 1;
        let Some(host) = self.hosts.get(&device_id).filter(|host| host.expires > now) else {
            self.counters.not_found += 1;
            return vec![(from, encode(&FromServer::NotFound { nonce }))];
        };
        let session = self.session_for(&device_id, from, &nonce);
        let to_viewer = FromServer::Introduced {
            nonce,
            session,
            peer: host.addr,
        };
        let to_host = FromServer::Incoming {
            session,
            peer: from,
        };
        vec![(from, encode(&to_viewer)), (host.addr, encode(&to_host))]
    }

    /// The same lookup, retried after a lost reply, gets the same session.
    fn session_for(&self, device_id: &DeviceId, viewer: SocketAddr, nonce: &Nonce) -> Session {
        let input = [
            b"session".as_slice(),
            &device_id.0,
            viewer.to_string().as_bytes(),
            nonce,
        ]
        .concat();
        let mut session: Session = hmac::sign(&self.key, &input).as_ref()[..8]
            .try_into()
            .expect("HMAC-SHA256 is 32 bytes");
        if session == [0; 8] {
            session[0] = 1; // zero means "not known yet" to punching
        }
        session
    }

    fn ttl_secs(&self) -> u16 {
        u16::try_from(self.config.ttl.as_secs()).unwrap_or(u16::MAX)
    }

    fn random<const N: usize>(&self) -> [u8; N] {
        let mut bytes = [0u8; N];
        self.rng.fill(&mut bytes).expect("system RNG failed");
        bytes
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use tidedesk_rendezvous_proto::sign_registration;

    use super::*;

    pub(crate) fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    /// A host's certificate and PKCS#8 key.
    pub(crate) fn host_identity() -> (Vec<u8>, Vec<u8>) {
        let generated = rcgen::generate_simple_self_signed(vec!["tidedesk-host".into()]).unwrap();
        (
            generated.cert.der().to_vec(),
            generated.signing_key.serialize_der(),
        )
    }

    fn only(replies: Vec<(SocketAddr, Vec<u8>)>) -> (SocketAddr, FromServer) {
        assert_eq!(replies.len(), 1, "one reply expected");
        let (to, datagram) = &replies[0];
        (*to, decode(datagram).expect("a valid reply"))
    }

    fn send(
        server: &mut Server,
        from: SocketAddr,
        message: ToServer,
        now: Instant,
    ) -> Vec<(SocketAddr, Vec<u8>)> {
        server.handle(from, Port::Main, &encode(&message), now)
    }

    fn challenge(server: &mut Server, from: SocketAddr, now: Instant) -> Challenge {
        match only(send(server, from, ToServer::hello([1; 8]), now)).1 {
            FromServer::Challenge { challenge, .. } => challenge,
            other => panic!("expected a challenge, got {other:?}"),
        }
    }

    fn registration(cert: &[u8], key: &[u8], challenge: Challenge) -> ToServer {
        let device_id = DeviceId::from_cert(cert);
        ToServer::Register {
            device_id,
            cert_der: cert.to_vec(),
            challenge,
            signature: sign_registration(key, &device_id, &challenge).unwrap(),
        }
    }

    /// Runs Hello and Register for a host; returns its ID and token.
    fn register(
        server: &mut Server,
        from: SocketAddr,
        cert: &[u8],
        key: &[u8],
        now: Instant,
    ) -> (DeviceId, Token) {
        let challenge = challenge(server, from, now);
        match only(send(server, from, registration(cert, key, challenge), now)).1 {
            FromServer::Registered {
                device_id,
                token,
                reflexive,
                ..
            } => {
                assert_eq!(reflexive, from);
                (device_id, token)
            }
            other => panic!("expected Registered, got {other:?}"),
        }
    }

    fn lookup(
        server: &mut Server,
        from: SocketAddr,
        device_id: DeviceId,
        now: Instant,
    ) -> Vec<(SocketAddr, Vec<u8>)> {
        let challenge = challenge(server, from, now);
        send(
            server,
            from,
            ToServer::Lookup {
                device_id,
                nonce: [4; 8],
                challenge,
            },
            now,
        )
    }

    #[test]
    fn hello_returns_reflexive_address_and_challenge() {
        let mut server = Server::new(Config::default());
        let host = addr("203.0.113.5:40000");
        let (to, reply) = only(send(
            &mut server,
            host,
            ToServer::hello([9; 8]),
            Instant::now(),
        ));
        assert_eq!(to, host);
        assert!(
            matches!(reply, FromServer::Challenge { nonce: [9, ..], reflexive, .. } if reflexive == host)
        );
        assert!(
            server
                .handle(host, Port::Main, b"noise", Instant::now())
                .is_empty()
        );
        let unpadded = ToServer::Hello {
            nonce: [9; 8],
            padding: Vec::new(),
        };
        assert!(
            send(&mut server, host, unpadded, Instant::now()).is_empty(),
            "a Hello smaller than its answer is ignored"
        );
    }

    #[test]
    fn alt_port_hello_reports_second_reflexive_address() {
        let mut server = Server::new(Config::default());
        let seen_on_alt = addr("203.0.113.5:40077");
        let hello = encode(&ToServer::hello([2; 8]));
        let (to, reply) = only(server.handle(seen_on_alt, Port::Alt, &hello, Instant::now()));
        assert_eq!(to, seen_on_alt);
        assert!(
            matches!(reply, FromServer::Challenge { reflexive, .. } if reflexive == seen_on_alt)
        );
        // Registration and lookups only happen on the main port.
        let lookup = encode(&ToServer::Lookup {
            device_id: DeviceId([1; 8]),
            nonce: [1; 8],
            challenge: [0; 16],
        });
        assert!(
            server
                .handle(seen_on_alt, Port::Alt, &lookup, Instant::now())
                .is_empty()
        );
    }

    #[test]
    fn challenges_are_bound_to_the_address_and_expire() {
        let mut server = Server::new(Config::default());
        let (cert, key) = host_identity();
        let host = addr("203.0.113.5:40000");
        let t0 = server.started;
        let issued = challenge(&mut server, host, t0);

        let elsewhere = addr("198.51.100.1:5000");
        let (_, reply) = only(send(
            &mut server,
            elsewhere,
            registration(&cert, &key, issued),
            t0,
        ));
        assert_eq!(
            reply,
            FromServer::Error {
                code: ErrorCode::UnknownChallenge
            }
        );

        let stale = t0 + CHALLENGE_WINDOW * 2;
        let (_, reply) = only(send(
            &mut server,
            host,
            registration(&cert, &key, issued),
            stale,
        ));
        assert_eq!(
            reply,
            FromServer::Error {
                code: ErrorCode::UnknownChallenge
            }
        );

        let still_fresh = t0 + CHALLENGE_WINDOW;
        let (_, reply) = only(send(
            &mut server,
            host,
            registration(&cert, &key, issued),
            still_fresh,
        ));
        assert!(matches!(reply, FromServer::Registered { .. }), "{reply:?}");
    }

    #[test]
    fn register_with_valid_signature_then_refresh_keeps_entry_alive() {
        let config = Config::default();
        let mut server = Server::new(config);
        let (cert, key) = host_identity();
        let t0 = Instant::now();
        let host = addr("203.0.113.5:40000");
        let (device_id, token) = register(&mut server, host, &cert, &key, t0);
        assert_eq!(server.hosts(), 1);

        // Refreshed just before expiry, from a new port: the new address wins.
        let moved = addr("203.0.113.5:40123");
        let later = t0 + config.ttl - Duration::from_secs(1);
        let (_, reply) = only(send(
            &mut server,
            moved,
            ToServer::Refresh { device_id, token },
            later,
        ));
        assert!(matches!(reply, FromServer::Registered { reflexive, .. } if reflexive == moved));
        server.sweep(t0 + config.ttl + Duration::from_secs(1));
        assert_eq!(server.hosts(), 1, "the refresh extended the registration");

        let (_, wrong) = only(send(
            &mut server,
            moved,
            ToServer::Refresh {
                device_id,
                token: [0; 16],
            },
            later,
        ));
        assert_eq!(
            wrong,
            FromServer::Error {
                code: ErrorCode::NotRegistered
            }
        );
    }

    #[test]
    fn register_with_bad_signature_is_rejected() {
        let mut server = Server::new(Config::default());
        let (cert, _) = host_identity();
        let (_, stranger_key) = host_identity();
        let host = addr("203.0.113.5:40000");
        let now = Instant::now();
        let issued = challenge(&mut server, host, now);
        let forged = registration(&cert, &stranger_key, issued);
        let (_, reply) = only(send(&mut server, host, forged, now));
        assert_eq!(
            reply,
            FromServer::Error {
                code: ErrorCode::BadSignature
            }
        );
        assert_eq!(server.hosts(), 0);
    }

    #[test]
    fn second_key_cannot_take_a_live_id() {
        let mut server = Server::new(Config::default());
        let (cert, key) = host_identity();
        let t0 = Instant::now();
        let host = addr("203.0.113.5:40000");
        let (device_id, _) = register(&mut server, host, &cert, &key, t0);

        // Another key for the same ID needs a hash collision; pretend one
        // happened: the live registration still stands.
        server.hosts.get_mut(&device_id).unwrap().public_key = vec![0x04; 65];
        let issued = challenge(&mut server, host, t0);
        let (_, reply) = only(send(
            &mut server,
            host,
            registration(&cert, &key, issued),
            t0,
        ));
        assert_eq!(
            reply,
            FromServer::Error {
                code: ErrorCode::IdTaken
            }
        );

        // Once it has expired, the ID is free again.
        let expired = t0 + Config::default().ttl;
        let issued = challenge(&mut server, host, expired);
        let (_, reply) = only(send(
            &mut server,
            host,
            registration(&cert, &key, issued),
            expired,
        ));
        assert!(matches!(reply, FromServer::Registered { .. }), "{reply:?}");
    }

    #[test]
    fn full_service_and_busy_addresses_refuse_new_hosts() {
        let config = Config {
            max_hosts: 2,
            max_hosts_per_ip: 1,
            ..Config::default()
        };
        let mut server = Server::new(config);
        let now = Instant::now();
        let (a, a_key) = host_identity();
        let (b, b_key) = host_identity();
        let (c, c_key) = host_identity();
        register(&mut server, addr("203.0.113.5:40000"), &a, &a_key, now);

        let same_ip = addr("203.0.113.5:40001");
        let issued = challenge(&mut server, same_ip, now);
        let (_, reply) = only(send(
            &mut server,
            same_ip,
            registration(&b, &b_key, issued),
            now,
        ));
        assert_eq!(
            reply,
            FromServer::Error {
                code: ErrorCode::Full
            },
            "one host per address here"
        );

        register(&mut server, addr("198.51.100.2:40000"), &b, &b_key, now);
        let third = addr("192.0.2.7:40000");
        let issued = challenge(&mut server, third, now);
        let (_, reply) = only(send(
            &mut server,
            third,
            registration(&c, &c_key, issued),
            now,
        ));
        assert_eq!(
            reply,
            FromServer::Error {
                code: ErrorCode::Full
            }
        );
        assert_eq!(server.hosts(), 2);

        // A host already registered may always register again.
        register(&mut server, addr("203.0.113.5:40000"), &a, &a_key, now);
    }

    #[test]
    fn lookup_introduces_both_sides_with_same_session() {
        let mut server = Server::new(Config::default());
        let (cert, key) = host_identity();
        let now = Instant::now();
        let host = addr("203.0.113.5:40000");
        let viewer = addr("198.51.100.7:51234");
        let (device_id, _) = register(&mut server, host, &cert, &key, now);

        let replies = lookup(&mut server, viewer, device_id, now);
        assert_eq!(replies.len(), 2);
        let mut to_viewer = None;
        let mut to_host = None;
        for (to, datagram) in &replies {
            match decode::<FromServer>(datagram).unwrap() {
                FromServer::Introduced {
                    nonce,
                    session,
                    peer,
                } if *to == viewer => {
                    assert_eq!((nonce, peer), ([4; 8], host));
                    to_viewer = Some(session);
                }
                FromServer::Incoming { session, peer } if *to == host => {
                    assert_eq!(peer, viewer);
                    to_host = Some(session);
                }
                other => panic!("unexpected {other:?} to {to}"),
            }
        }
        assert_eq!(to_viewer, to_host);

        // A retried lookup (same nonce) gets the same session.
        let again = lookup(&mut server, viewer, device_id, now);
        let sessions: Vec<Session> = again
            .iter()
            .filter_map(|(_, d)| match decode::<FromServer>(d)? {
                FromServer::Introduced { session, .. } | FromServer::Incoming { session, .. } => {
                    Some(session)
                }
                _ => None,
            })
            .collect();
        assert_eq!(sessions, [to_viewer.unwrap(); 2]);
    }

    #[test]
    fn lookup_without_a_challenge_from_that_address_introduces_nobody() {
        // A forged source address would make the host punch a stranger.
        let mut server = Server::new(Config::default());
        let (cert, key) = host_identity();
        let now = Instant::now();
        let (device_id, _) = register(&mut server, addr("203.0.113.5:40000"), &cert, &key, now);
        let victim = addr("192.0.2.99:9");
        let forged = ToServer::Lookup {
            device_id,
            nonce: [4; 8],
            challenge: [0; 16],
        };
        let (to, reply) = only(send(&mut server, victim, forged, now));
        assert_eq!(to, victim, "only the error goes back, nothing to the host");
        assert_eq!(
            reply,
            FromServer::Error {
                code: ErrorCode::UnknownChallenge
            }
        );
    }

    #[test]
    fn lookup_unknown_id_returns_not_found() {
        let mut server = Server::new(Config::default());
        let viewer = addr("198.51.100.7:51234");
        let reply = only(lookup(
            &mut server,
            viewer,
            DeviceId([5; 8]),
            Instant::now(),
        ));
        assert_eq!(reply, (viewer, FromServer::NotFound { nonce: [4; 8] }));
        assert_eq!(server.counters().not_found, 1);
    }

    #[test]
    fn entries_expire_after_ttl() {
        let config = Config::default();
        let mut server = Server::new(config);
        let (cert, key) = host_identity();
        let t0 = Instant::now();
        let (device_id, _) = register(&mut server, addr("203.0.113.5:40000"), &cert, &key, t0);
        let expired = t0 + config.ttl;
        let viewer = addr("198.51.100.7:51234");
        let (_, reply) = only(lookup(&mut server, viewer, device_id, expired));
        assert_eq!(reply, FromServer::NotFound { nonce: [4; 8] });
        server.sweep(expired);
        assert_eq!(server.hosts(), 0);
    }

    #[test]
    fn rate_limiter_drops_excess_per_ip() {
        let config = Config::default();
        let mut server = Server::new(config);
        let now = Instant::now();
        let busy = addr("198.51.100.9:1000");
        let hello = encode(&ToServer::hello([0; 8]));
        let answered = (0..100)
            .filter(|_| !server.handle(busy, Port::Main, &hello, now).is_empty())
            .count();
        assert_eq!(answered, config.burst as usize);
        assert_eq!(server.counters().rate_limited, 100 - config.burst as u64);

        // Another address is unaffected, and the busy one recovers over time.
        assert!(
            !server
                .handle(addr("198.51.100.10:1000"), Port::Main, &hello, now)
                .is_empty()
        );
        let later = now + Duration::from_secs(1);
        assert!(!server.handle(busy, Port::Main, &hello, later).is_empty());
    }

    #[test]
    fn rate_limiter_stops_tracking_new_addresses_when_full() {
        let config = Config {
            max_tracked_addresses: 2,
            ..Config::default()
        };
        let mut server = Server::new(config);
        let now = Instant::now();
        let hello = encode(&ToServer::hello([0; 8]));
        for known in ["192.0.2.1:1", "192.0.2.2:1"] {
            assert!(
                !server
                    .handle(addr(known), Port::Main, &hello, now)
                    .is_empty()
            );
        }
        assert!(
            server
                .handle(addr("192.0.2.3:1"), Port::Main, &hello, now)
                .is_empty()
        );
        // Once the others have gone quiet, new addresses are served again.
        let quiet = now + Duration::from_secs(5);
        assert!(
            !server
                .handle(addr("192.0.2.3:1"), Port::Main, &hello, quiet)
                .is_empty()
        );
    }
}
