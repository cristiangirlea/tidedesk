//! The service's rules, without I/O: a datagram in, datagrams out.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use tidedesk_rendezvous_proto::{
    Challenge, DeviceId, ErrorCode, FromServer, Nonce, Session, ToServer, Token, decode, encode,
    public_key_from_cert, verify_registration,
};

/// How long a Hello's challenge can be used to register.
const CHALLENGE_TTL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// How long a registration lasts without a refresh.
    pub ttl: Duration,
    pub max_hosts: usize,
    /// Datagrams per second allowed from one IP address, and the burst.
    pub rate: f64,
    pub burst: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(75),
            max_hosts: 10_000,
            rate: 10.0,
            burst: 20.0,
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
    hosts: HashMap<DeviceId, Host>,
    /// Challenges handed out, by the address they went to, and when they expire.
    challenges: HashMap<(SocketAddr, Challenge), Instant>,
    buckets: HashMap<IpAddr, Bucket>,
    rng: SystemRandom,
    /// Makes session IDs unguessable while a retried lookup gets the same one.
    secret: [u8; 32],
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
        let mut secret = [0u8; 32];
        rng.fill(&mut secret).expect("system RNG failed");
        Self {
            config,
            hosts: HashMap::new(),
            challenges: HashMap::new(),
            buckets: HashMap::new(),
            rng,
            secret,
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
            (_, ToServer::Hello { nonce }) => {
                let challenge = if port == Port::Main {
                    self.challenge_for(from, now)
                } else {
                    [0; 16] // the second port only reports the address it saw
                };
                FromServer::Challenge {
                    nonce,
                    challenge,
                    reflexive: from,
                }
            }
            (Port::Alt, _) => return Vec::new(),
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
            (Port::Main, ToServer::Lookup { device_id, nonce }) => {
                return self.lookup(from, device_id, nonce, now);
            }
        };
        vec![(from, encode(&reply))]
    }

    /// Forgets expired registrations, challenges and rate limits.
    pub fn sweep(&mut self, now: Instant) {
        self.hosts.retain(|_, host| host.expires > now);
        self.challenges.retain(|_, expires| *expires > now);
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

    fn challenge_for(&mut self, from: SocketAddr, now: Instant) -> Challenge {
        let challenge = self.random();
        // Bounded: each address is rate limited, and sweeps drop old ones.
        if self.challenges.len() < self.config.max_hosts {
            self.challenges
                .insert((from, challenge), now + CHALLENGE_TTL);
        }
        challenge
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
        let fresh = self
            .challenges
            .remove(&(from, challenge))
            .is_some_and(|expires| expires > now);
        let failure = if !fresh {
            Some(ErrorCode::UnknownChallenge)
        } else if !verify_registration(cert_der, &device_id, &challenge, signature) {
            Some(ErrorCode::BadSignature)
        } else {
            None
        };
        if let Some(code) = failure {
            self.counters.rejected += 1;
            return FromServer::Error { code };
        }
        let public_key = public_key_from_cert(cert_der)
            .expect("checked by verify_registration")
            .to_vec();

        // An ID comes from a certificate hash, so another key for a live ID
        // means a hash collision: never let it take the registration over.
        let live = self.hosts.get(&device_id).filter(|host| host.expires > now);
        if live.is_some_and(|host| host.public_key != public_key) {
            self.counters.rejected += 1;
            return FromServer::Error {
                code: ErrorCode::IdTaken,
            };
        }
        if !self.hosts.contains_key(&device_id) && self.hosts.len() >= self.config.max_hosts {
            self.sweep(now);
            if self.hosts.len() >= self.config.max_hosts {
                return FromServer::Error {
                    code: ErrorCode::Full,
                };
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
            self.secret.as_slice(),
            &device_id.0,
            viewer.to_string().as_bytes(),
            nonce,
        ]
        .concat();
        let mut session: Session = digest(&SHA256, &input).as_ref()[..8]
            .try_into()
            .expect("SHA-256 is 32 bytes");
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

    /// Runs Hello and Register for a host; returns its ID and token.
    fn register(
        server: &mut Server,
        from: SocketAddr,
        cert: &[u8],
        key: &[u8],
        now: Instant,
    ) -> (DeviceId, Token) {
        let (_, reply) = only(send(server, from, ToServer::Hello { nonce: [1; 8] }, now));
        let FromServer::Challenge { challenge, .. } = reply else {
            panic!("expected a challenge, got {reply:?}");
        };
        let device_id = DeviceId::from_cert(cert);
        let register = ToServer::Register {
            device_id,
            cert_der: cert.to_vec(),
            challenge,
            signature: sign_registration(key, &device_id, &challenge).unwrap(),
        };
        match only(send(server, from, register, now)).1 {
            FromServer::Registered {
                token, reflexive, ..
            } => {
                assert_eq!(reflexive, from);
                (device_id, token)
            }
            other => panic!("expected Registered, got {other:?}"),
        }
    }

    #[test]
    fn hello_returns_reflexive_address_and_challenge() {
        let mut server = Server::new(Config::default());
        let host = addr("203.0.113.5:40000");
        let (to, reply) = only(send(
            &mut server,
            host,
            ToServer::Hello { nonce: [9; 8] },
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
    }

    #[test]
    fn alt_port_hello_reports_second_reflexive_address() {
        let mut server = Server::new(Config::default());
        let seen_on_alt = addr("203.0.113.5:40077");
        let hello = encode(&ToServer::Hello { nonce: [2; 8] });
        let (to, reply) = only(server.handle(seen_on_alt, Port::Alt, &hello, Instant::now()));
        assert_eq!(to, seen_on_alt);
        assert!(
            matches!(reply, FromServer::Challenge { reflexive, .. } if reflexive == seen_on_alt)
        );
        // Registration only happens on the main port.
        let lookup = encode(&ToServer::Lookup {
            device_id: DeviceId([1; 8]),
            nonce: [1; 8],
        });
        assert!(
            server
                .handle(seen_on_alt, Port::Alt, &lookup, Instant::now())
                .is_empty()
        );
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
        let FromServer::Challenge { challenge, .. } = only(send(
            &mut server,
            host,
            ToServer::Hello { nonce: [1; 8] },
            now,
        ))
        .1
        else {
            panic!("expected a challenge");
        };
        let device_id = DeviceId::from_cert(&cert);
        let forged = ToServer::Register {
            device_id,
            cert_der: cert.clone(),
            challenge,
            signature: sign_registration(&stranger_key, &device_id, &challenge).unwrap(),
        };
        assert_eq!(
            only(send(&mut server, host, forged, now)).1,
            FromServer::Error {
                code: ErrorCode::BadSignature
            }
        );
        assert_eq!(server.hosts(), 0);

        // A challenge is good for one registration, from the address it was sent to.
        let replay = ToServer::Register {
            device_id,
            cert_der: cert,
            challenge,
            signature: vec![0; 64],
        };
        let (_, reply) = only(send(&mut server, addr("198.51.100.1:5000"), replay, now));
        assert_eq!(
            reply,
            FromServer::Error {
                code: ErrorCode::UnknownChallenge
            }
        );
    }

    #[test]
    fn second_key_cannot_take_a_live_id() {
        let mut server = Server::new(Config::default());
        let (cert, key) = host_identity();
        let t0 = Instant::now();
        let (device_id, _) = register(&mut server, addr("203.0.113.5:40000"), &cert, &key, t0);

        // Someone else claims the ID with their own certificate and key: the
        // ID would not match their certificate, so the signature check fails
        // first; a live entry is never overwritten by another key.
        let (other_cert, other_key) = host_identity();
        let attacker = addr("198.51.100.66:6000");
        let FromServer::Challenge { challenge, .. } = only(send(
            &mut server,
            attacker,
            ToServer::Hello { nonce: [1; 8] },
            t0,
        ))
        .1
        else {
            panic!("expected a challenge");
        };
        let claim = ToServer::Register {
            device_id,
            cert_der: other_cert,
            challenge,
            signature: sign_registration(&other_key, &device_id, &challenge).unwrap(),
        };
        let (_, reply) = only(send(&mut server, attacker, claim, t0));
        assert!(matches!(reply, FromServer::Error { .. }));

        // The genuine host registering again from elsewhere keeps its ID.
        let (again, _) = register(&mut server, addr("203.0.113.5:41000"), &cert, &key, t0);
        assert_eq!(again, device_id);
        assert_eq!(server.hosts(), 1);
    }

    #[test]
    fn lookup_introduces_both_sides_with_same_session() {
        let mut server = Server::new(Config::default());
        let (cert, key) = host_identity();
        let now = Instant::now();
        let host = addr("203.0.113.5:40000");
        let viewer = addr("198.51.100.7:51234");
        let (device_id, _) = register(&mut server, host, &cert, &key, now);

        let replies = send(
            &mut server,
            viewer,
            ToServer::Lookup {
                device_id,
                nonce: [4; 8],
            },
            now,
        );
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
        let again = send(
            &mut server,
            viewer,
            ToServer::Lookup {
                device_id,
                nonce: [4; 8],
            },
            now,
        );
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
    fn lookup_unknown_id_returns_not_found() {
        let mut server = Server::new(Config::default());
        let viewer = addr("198.51.100.7:51234");
        let reply = only(send(
            &mut server,
            viewer,
            ToServer::Lookup {
                device_id: DeviceId([5; 8]),
                nonce: [6; 8],
            },
            Instant::now(),
        ));
        assert_eq!(reply, (viewer, FromServer::NotFound { nonce: [6; 8] }));
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
        let (_, reply) = only(send(
            &mut server,
            viewer,
            ToServer::Lookup {
                device_id,
                nonce: [1; 8],
            },
            expired,
        ));
        assert_eq!(reply, FromServer::NotFound { nonce: [1; 8] });
        server.sweep(expired);
        assert_eq!(server.hosts(), 0);
    }

    #[test]
    fn rate_limiter_drops_excess_per_ip() {
        let config = Config::default();
        let mut server = Server::new(config);
        let now = Instant::now();
        let busy = addr("198.51.100.9:1000");
        let hello = encode(&ToServer::Hello { nonce: [0; 8] });
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
}
