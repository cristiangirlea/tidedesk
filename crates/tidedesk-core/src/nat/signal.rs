//! The host side of the rendezvous protocol: registers this host's device ID
//! with a rendezvous service and keeps the registration, and the router's
//! mapping for the QUIC port, alive. The service only introduces viewers;
//! sessions never pass through it.

use std::fmt;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tidedesk_rendezvous_proto::{
    Challenge, DeviceId, ErrorCode, FromServer, Nonce, Session, ToServer, Token, decode, encode,
    sign_registration,
};

use super::random_bytes;
use super::stun::NatKind;

/// How often a registered host refreshes; the service forgets hosts after
/// 75 seconds, and cheap routers forget idle mappings after 30.
pub const REFRESH_EVERY: Duration = Duration::from_secs(25);

const FIRST_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(8);
const REFRESH_RETRY: Duration = Duration::from_secs(3);
/// Unanswered refreshes before registering again.
const MISSED_LIMIT: u32 = 3;
/// Registration attempts per challenge before asking for a new one.
const REGISTER_TRIES: u32 = 3;
/// Without any answer for this long, the service counts as unreachable
/// (attempts go on).
const UNREACHABLE_AFTER: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendezvousStatus {
    /// No rendezvous service is configured.
    Off,
    Connecting,
    Registered {
        /// This host's address as the service sees it.
        public: SocketAddr,
        nat: NatKind,
    },
    Unreachable(String),
}

impl fmt::Display for RendezvousStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("off"),
            Self::Connecting => f.write_str("connecting"),
            Self::Registered { public, nat } => {
                write!(f, "registered as {public} (NAT: {nat})")
            }
            Self::Unreachable(reason) => write!(f, "unreachable ({reason})"),
        }
    }
}

/// Finds a rendezvous service given as `host[:port]` (IPv4, like all
/// internet paths here).
pub async fn resolve_service(name: &str) -> Option<SocketAddr> {
    let target =
        crate::net::with_default_port(name.trim(), tidedesk_rendezvous_proto::DEFAULT_PORT);
    tokio::net::lookup_host(&target)
        .await
        .ok()?
        .find(SocketAddr::is_ipv4)
}

/// What the service told the host that needs acting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A viewer was introduced and is about to punch towards this host.
    Incoming { session: Session, peer: SocketAddr },
}

/// The key material a registration needs.
#[derive(Clone)]
pub struct Credentials {
    pub device_id: DeviceId,
    pub cert_der: Vec<u8>,
    pub pkcs8: Vec<u8>,
}

/// One host's registration with one service.
///
/// Pure state machine: the caller sends what [`Registration::poll`] returns
/// and passes datagrams to [`Registration::on_datagram`]; time is passed in.
pub struct Registration {
    name: String,
    main: SocketAddr,
    alt: SocketAddr,
    credentials: Credentials,
    phase: Phase,
    /// The current round's Hello, sent to both ports.
    nonce: Nonce,
    reflexive: Option<SocketAddr>,
    alt_reflexive: Option<SocketAddr>,
    status: RendezvousStatus,
    /// Since when nothing has been heard from the service.
    silent_since: Instant,
}

enum Phase {
    /// Asking for a challenge.
    Hello { next: Instant, retry: Duration },
    /// Registration sent for this challenge.
    Registering {
        challenge: Challenge,
        next: Instant,
        tries: u32,
    },
    Registered {
        token: Token,
        next: Instant,
        missed: u32,
    },
}

impl Registration {
    /// `main` is the service's address; its second port, one higher, answers
    /// only Hello and tells whether this network's NAT is symmetric.
    pub fn new(name: String, main: SocketAddr, credentials: Credentials, now: Instant) -> Self {
        let alt = SocketAddr::new(main.ip(), main.port().wrapping_add(1));
        Self {
            name,
            main,
            alt,
            credentials,
            phase: Phase::Hello {
                next: now,
                retry: FIRST_RETRY,
            },
            nonce: random_bytes(),
            reflexive: None,
            alt_reflexive: None,
            status: RendezvousStatus::Connecting,
            silent_since: now,
        }
    }

    /// Datagrams due at `now`.
    pub fn poll(&mut self, now: Instant) -> Vec<(SocketAddr, Vec<u8>)> {
        match &mut self.phase {
            Phase::Hello { next, retry } if now >= *next => {
                *next = now + *retry;
                *retry = (*retry * 2).min(MAX_RETRY);
                if now.saturating_duration_since(self.silent_since) >= UNREACHABLE_AFTER
                    && !matches!(self.status, RendezvousStatus::Unreachable(_))
                {
                    self.status = RendezvousStatus::Unreachable(format!(
                        "no answer from the rendezvous service {}",
                        self.name
                    ));
                }
                let hello = encode(&ToServer::hello(self.nonce));
                vec![(self.main, hello.clone()), (self.alt, hello)]
            }
            Phase::Registering { tries, .. } if *tries >= REGISTER_TRIES => {
                self.restart(now);
                self.poll(now)
            }
            Phase::Registering {
                challenge,
                next,
                tries,
            } if now >= *next => {
                *tries += 1;
                *next = now + FIRST_RETRY;
                let challenge = *challenge;
                match self.register_message(&challenge) {
                    Some(register) => vec![(self.main, register)],
                    None => {
                        self.status =
                            RendezvousStatus::Unreachable("cannot sign the registration".into());
                        Vec::new()
                    }
                }
            }
            Phase::Registered { missed, .. } if *missed >= MISSED_LIMIT => {
                self.status = RendezvousStatus::Unreachable(format!(
                    "the rendezvous service {} stopped answering",
                    self.name
                ));
                self.restart(now);
                self.poll(now)
            }
            Phase::Registered {
                token,
                next,
                missed,
            } if now >= *next => {
                *missed += 1;
                *next = now + REFRESH_RETRY;
                let refresh = ToServer::Refresh {
                    device_id: self.credentials.device_id,
                    token: *token,
                };
                vec![(self.main, encode(&refresh))]
            }
            _ => Vec::new(),
        }
    }

    /// Handles a datagram from the socket.
    pub fn on_datagram(
        &mut self,
        from: SocketAddr,
        datagram: &[u8],
        now: Instant,
    ) -> Option<Event> {
        if from != self.main && from != self.alt {
            return None;
        }
        let message: FromServer = decode(datagram)?;
        if from == self.alt {
            // The second port only reports the address it saw.
            if let FromServer::Challenge {
                nonce, reflexive, ..
            } = message
                && nonce == self.nonce
            {
                self.alt_reflexive = Some(reflexive);
                self.refresh_status();
            }
            return None;
        }
        self.silent_since = now;
        match message {
            FromServer::Challenge {
                nonce,
                challenge,
                reflexive,
            } if nonce == self.nonce && matches!(self.phase, Phase::Hello { .. }) => {
                self.reflexive = Some(reflexive);
                self.phase = Phase::Registering {
                    challenge,
                    next: now,
                    tries: 0,
                };
            }
            FromServer::Registered {
                device_id,
                token,
                reflexive,
                ..
            } if device_id == self.credentials.device_id => {
                self.reflexive = Some(reflexive);
                self.phase = Phase::Registered {
                    token,
                    next: now + REFRESH_EVERY,
                    missed: 0,
                };
                self.refresh_status();
            }
            FromServer::Error { code } => match code {
                // A stale challenge, or a service that forgot this host.
                ErrorCode::UnknownChallenge | ErrorCode::NotRegistered => self.restart(now),
                refused => {
                    self.status = RendezvousStatus::Unreachable(format!(
                        "the rendezvous service {} refused this host ({refused:?})",
                        self.name
                    ));
                    self.restart(now);
                    // Refusals do not go away quickly: ask again only rarely.
                    self.phase = Phase::Hello {
                        next: now + MAX_RETRY,
                        retry: MAX_RETRY,
                    };
                }
            },
            FromServer::Incoming { session, peer }
                if matches!(self.phase, Phase::Registered { .. }) =>
            {
                return Some(Event::Incoming { session, peer });
            }
            _ => {}
        }
        None
    }

    /// When [`Registration::poll`] next has work.
    pub fn next_deadline(&self) -> Instant {
        match &self.phase {
            Phase::Hello { next, .. }
            | Phase::Registering { next, .. }
            | Phase::Registered { next, .. } => *next,
        }
    }

    pub fn status(&self) -> &RendezvousStatus {
        &self.status
    }

    /// A new round: new nonce, Hello to both ports now.
    fn restart(&mut self, now: Instant) {
        self.nonce = random_bytes();
        self.alt_reflexive = None;
        self.phase = Phase::Hello {
            next: now,
            retry: FIRST_RETRY,
        };
    }

    fn register_message(&self, challenge: &Challenge) -> Option<Vec<u8>> {
        let Credentials {
            device_id,
            cert_der,
            pkcs8,
        } = &self.credentials;
        let signature = sign_registration(pkcs8, device_id, challenge).ok()?;
        Some(encode(&ToServer::Register {
            device_id: *device_id,
            cert_der: cert_der.clone(),
            challenge: *challenge,
            signature,
        }))
    }

    /// Updates the status of a registered host with what is known.
    fn refresh_status(&mut self) {
        let (Phase::Registered { .. }, Some(public)) = (&self.phase, self.reflexive) else {
            return;
        };
        let nat = match self.alt_reflexive {
            Some(other) if other == public => NatKind::EndpointIndependent,
            Some(_) => NatKind::Symmetric,
            None => NatKind::Unknown,
        };
        self.status = RendezvousStatus::Registered { public, nat };
    }
}

#[cfg(test)]
mod tests {
    use tidedesk_rendezvous_proto::verify_registration;

    use super::*;

    const SERVICE: &str = "198.51.100.1:47900";
    const ALT: &str = "198.51.100.1:47901";
    const ME: &str = "203.0.113.5:40000";

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn credentials() -> Credentials {
        let generated = rcgen::generate_simple_self_signed(vec!["tidedesk-host".into()]).unwrap();
        let cert_der = generated.cert.der().to_vec();
        Credentials {
            device_id: DeviceId::from_cert(&cert_der),
            cert_der,
            pkcs8: generated.signing_key.serialize_der(),
        }
    }

    fn sent(datagrams: &[(SocketAddr, Vec<u8>)]) -> Vec<(SocketAddr, ToServer)> {
        datagrams
            .iter()
            .map(|(to, d)| (*to, decode(d).expect("a valid request")))
            .collect()
    }

    fn from_service(r: &mut Registration, message: &FromServer, now: Instant) -> Option<Event> {
        r.on_datagram(addr(SERVICE), &encode(message), now)
    }

    /// Polls for the round's Hellos, one to each port with one nonce.
    fn hello_nonce(r: &mut Registration, now: Instant) -> Nonce {
        let hellos = sent(&r.poll(now));
        let nonces: Vec<(SocketAddr, Nonce)> = hellos
            .iter()
            .map(|(to, m)| match m {
                ToServer::Hello { nonce, .. } => (*to, *nonce),
                other => panic!("expected Hellos, got {other:?}"),
            })
            .collect();
        assert_eq!(nonces.len(), 2);
        assert_eq!((nonces[0].0, nonces[1].0), (addr(SERVICE), addr(ALT)));
        assert_eq!(nonces[0].1, nonces[1].1);
        nonces[0].1
    }

    /// Answers the Hello; returns the challenge and the registration sent.
    fn challenged(r: &mut Registration, nonce: Nonce, now: Instant) -> (Challenge, ToServer) {
        let challenge = [7; 16];
        let answer = FromServer::Challenge {
            nonce,
            challenge,
            reflexive: addr(ME),
        };
        assert_eq!(from_service(r, &answer, now), None);
        let register = sent(&r.poll(now));
        assert_eq!(register.len(), 1);
        assert_eq!(register[0].0, addr(SERVICE));
        (challenge, register[0].1.clone())
    }

    fn register_answered(r: &mut Registration, credentials: &Credentials, now: Instant) -> Token {
        let token = [9; 16];
        let answer = FromServer::Registered {
            device_id: credentials.device_id,
            token,
            reflexive: addr(ME),
            ttl_secs: 75,
        };
        from_service(r, &answer, now);
        token
    }

    fn registered(r: &mut Registration, credentials: &Credentials, now: Instant) -> Token {
        let nonce = hello_nonce(r, now);
        challenged(r, nonce, now);
        register_answered(r, credentials, now)
    }

    #[test]
    fn client_retransmits_hello_then_registers_after_challenge() {
        let t0 = Instant::now();
        let credentials = credentials();
        let name = "rendezvous.example".to_string();
        let mut r = Registration::new(name, addr(SERVICE), credentials.clone(), t0);
        assert_eq!(r.status(), &RendezvousStatus::Connecting);

        let nonce = hello_nonce(&mut r, t0);
        assert!(r.poll(t0 + Duration::from_millis(500)).is_empty());
        assert_eq!(
            hello_nonce(&mut r, t0 + FIRST_RETRY),
            nonce,
            "resent after a second"
        );
        assert_eq!(
            r.next_deadline(),
            t0 + FIRST_RETRY * 3,
            "then after two more"
        );

        let later = t0 + Duration::from_secs(12);
        hello_nonce(&mut r, later);
        assert!(
            matches!(r.status(), RendezvousStatus::Unreachable(why) if why.contains("rendezvous.example")),
            "{:?}",
            r.status()
        );

        let (challenge, register) = challenged(&mut r, nonce, later);
        let ToServer::Register {
            device_id,
            cert_der,
            challenge: signed,
            signature,
        } = register
        else {
            panic!("expected Register, got {register:?}");
        };
        assert_eq!((device_id, signed), (credentials.device_id, challenge));
        assert!(verify_registration(
            &cert_der, &device_id, &challenge, &signature
        ));

        register_answered(&mut r, &credentials, later);
        let expected = RendezvousStatus::Registered {
            public: addr(ME),
            nat: NatKind::Unknown,
        };
        assert_eq!(r.status(), &expected);
        assert_eq!(r.next_deadline(), later + REFRESH_EVERY);
    }

    #[test]
    fn client_reregisters_after_three_missed_refreshes() {
        let t0 = Instant::now();
        let credentials = credentials();
        let mut r = Registration::new("s".into(), addr(SERVICE), credentials.clone(), t0);
        let token = registered(&mut r, &credentials, t0);

        let refresh = ToServer::Refresh {
            device_id: credentials.device_id,
            token,
        };
        let mut now = t0 + REFRESH_EVERY;
        for _ in 0..MISSED_LIMIT {
            assert_eq!(sent(&r.poll(now)), [(addr(SERVICE), refresh.clone())]);
            now += REFRESH_RETRY;
        }
        // Nothing came back: start over, and say so.
        hello_nonce(&mut r, now);
        assert!(
            matches!(r.status(), RendezvousStatus::Unreachable(_)),
            "{:?}",
            r.status()
        );

        // An answered refresh, in contrast, keeps the registration.
        let mut r = Registration::new("s".into(), addr(SERVICE), credentials.clone(), t0);
        let token = registered(&mut r, &credentials, t0);
        r.poll(t0 + REFRESH_EVERY);
        let moved = addr("203.0.113.5:40999");
        let answer = FromServer::Registered {
            device_id: credentials.device_id,
            token,
            reflexive: moved,
            ttl_secs: 75,
        };
        from_service(&mut r, &answer, t0 + REFRESH_EVERY);
        assert!(
            matches!(r.status(), RendezvousStatus::Registered { public, .. } if *public == moved)
        );
        assert_eq!(r.next_deadline(), t0 + REFRESH_EVERY * 2);
    }

    #[test]
    fn service_errors_restart_or_are_reported() {
        let t0 = Instant::now();
        let credentials = credentials();
        let mut r = Registration::new("s".into(), addr(SERVICE), credentials.clone(), t0);
        registered(&mut r, &credentials, t0);
        // The service restarted and forgot this host: register again.
        let forgot = FromServer::Error {
            code: ErrorCode::NotRegistered,
        };
        from_service(&mut r, &forgot, t0);
        hello_nonce(&mut r, t0);

        let refused = FromServer::Error {
            code: ErrorCode::Full,
        };
        from_service(&mut r, &refused, t0);
        assert!(
            matches!(r.status(), RendezvousStatus::Unreachable(why) if why.contains("refused")),
            "{:?}",
            r.status()
        );
    }

    #[test]
    fn alt_port_answer_classifies_nat() {
        let t0 = Instant::now();
        let credentials = credentials();
        for (seen_on_alt, nat) in [
            (ME, NatKind::EndpointIndependent),
            ("203.0.113.5:40001", NatKind::Symmetric),
        ] {
            let mut r = Registration::new("s".into(), addr(SERVICE), credentials.clone(), t0);
            let nonce = hello_nonce(&mut r, t0);
            let from_alt = FromServer::Challenge {
                nonce,
                challenge: [0; 16],
                reflexive: addr(seen_on_alt),
            };
            assert_eq!(r.on_datagram(addr(ALT), &encode(&from_alt), t0), None);
            challenged(&mut r, nonce, t0);
            register_answered(&mut r, &credentials, t0);
            let expected = RendezvousStatus::Registered {
                public: addr(ME),
                nat,
            };
            assert_eq!(r.status(), &expected);
        }
    }

    #[test]
    fn only_the_service_can_announce_incoming_viewers() {
        let t0 = Instant::now();
        let credentials = credentials();
        let mut r = Registration::new("s".into(), addr(SERVICE), credentials.clone(), t0);
        let viewer = addr("192.0.2.9:5000");
        let incoming = FromServer::Incoming {
            session: [3; 8],
            peer: viewer,
        };
        assert_eq!(
            from_service(&mut r, &incoming, t0),
            None,
            "not registered yet"
        );

        registered(&mut r, &credentials, t0);
        let stranger = addr("192.0.2.66:47900");
        assert_eq!(r.on_datagram(stranger, &encode(&incoming), t0), None);
        assert_eq!(r.on_datagram(addr(ALT), &encode(&incoming), t0), None);
        let expected = Event::Incoming {
            session: [3; 8],
            peer: viewer,
        };
        assert_eq!(from_service(&mut r, &incoming, t0), Some(expected));
    }
}
