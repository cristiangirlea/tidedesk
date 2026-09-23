//! Connecting to a host: route, QUIC handshake, fingerprint pinning, auth.
//!
//! A host is reached directly at its address (same network, a VPN or a
//! forwarded port), or over the internet through a path both computers punch
//! through their routers (see `tidedesk_core::nat`). Either way every byte
//! goes straight between the two computers: TideDesk never relays.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tidedesk_core::identity::{KnownHosts, PinStatus, normalize_fingerprint};
use tidedesk_core::nat::punch::new_session;
use tidedesk_core::nat::stun::{DEFAULT_STUN_SERVERS, resolve_servers};
use tidedesk_core::nat::{
    Agent, NatKind, NotPublic, PunchError, Punched, SharedSocket, check_public,
};
use tidedesk_core::protocol::{self, ClientMessage, PROTOCOL_VERSION, ServerMessage};
use tidedesk_core::{DEFAULT_PORT, auth, net, paths};

/// How long to punch towards the host: the person there has this long to
/// type this computer's address and press Open.
pub const PUNCH_WINDOW: Duration = Duration::from_secs(120);

pub struct Session {
    pub endpoint: quinn::Endpoint,
    pub conn: quinn::Connection,
    pub send: quinn::SendStream,
    pub recv: quinn::RecvStream,
    pub host_name: String,
    pub fingerprint: String,
    pub width: u32,
    pub height: u32,
    pub audio: bool,
    pub route: RouteInfo,
}

pub struct ConnectOptions {
    pub host: String,
    pub code: String,
    pub want_audio: bool,
    pub expected_fingerprint: Option<String>,
    pub accept_new_fingerprint: bool,
    pub route: Route,
}

/// How to reach a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// At its address: same network, a VPN, or a forwarded port.
    Direct,
    /// At its internet address through a punched path, which the person at
    /// the host opens with this computer's internet address.
    Internet { stun_servers: Vec<String> },
}

impl Route {
    /// An internet route asking the default STUN servers.
    pub fn internet() -> Self {
        Self::Internet {
            stun_servers: DEFAULT_STUN_SERVERS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Steps of opening an internet path, for the user to follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Status(String),
    /// This computer's internet address, for the person at the host to type.
    ViewerAddress(SocketAddr),
    PathOpen(Punched),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    Direct,
    Internet,
}

/// How a session reaches its host. Logged, so users can check that the
/// other end is the host itself and not a middle box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteInfo {
    pub kind: RouteKind,
    pub peer: SocketAddr,
    /// This computer's address as the host saw it (internet routes).
    pub observed_self: Option<SocketAddr>,
}

impl fmt::Display for RouteInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            RouteKind::Direct => "direct",
            RouteKind::Internet => "internet, direct",
        };
        write!(f, "{kind} to {}", self.peer)?;
        if let Some(me) = self.observed_self {
            write!(f, ", we appear as {me}")?;
        }
        Ok(())
    }
}

/// Reads a host's internet address, as its window shows it, for
/// [`Route::Internet`].
pub fn parse_internet_host(text: &str) -> Result<SocketAddr> {
    let text = text.trim();
    let addr: SocketAddr = text.parse().map_err(|_| {
        anyhow!(
            "{text:?} is not an internet address with a port, such as 203.0.113.5:40000 \
             (the host's window shows it under \"Internet address\")"
        )
    })?;
    match check_public(addr) {
        Ok(()) => Ok(addr),
        Err(NotPublic::Ipv6) => bail!("internet connections use IPv4 addresses for now"),
        Err(NotPublic::Local) => {
            bail!("{addr} is a local network address: connect to it without --internet")
        }
        Err(NotPublic::Unusable) => bail!("{addr} is not an address a host can have"),
    }
}

fn symmetric_nat() -> String {
    format!(
        "this network uses a symmetric NAT (common on mobile data and carrier-grade NAT), so no \
         direct path to the host can be opened from here. TideDesk never relays sessions: use a \
         VPN such as Tailscale, or forward UDP port {DEFAULT_PORT} on the host's router and \
         connect without --internet. See docs/internet-access.md."
    )
}

fn no_reply(host: SocketAddr, me: SocketAddr) -> String {
    format!(
        "could not open a direct path to {host}. Make sure the host has this computer's internet \
         address ({me}) entered and Open pressed within the last two minutes. If either network \
         uses a symmetric NAT (common on mobile data and carrier-grade NAT), a direct connection \
         is impossible: TideDesk never relays, so use a VPN such as Tailscale or forward UDP port \
         {DEFAULT_PORT} on the host's router. See docs/internet-access.md."
    )
}

/// While punching, when to start trying the host's address directly too.
const DIRECT_TRY_AFTER: Duration = Duration::from_secs(5);
const DIRECT_TRY_TIMEOUT: Duration = Duration::from_secs(3);
const DIRECT_TRY_EVERY: Duration = Duration::from_secs(5);

/// Resolves once a QUIC handshake with `host` succeeds: a host whose UDP port
/// is forwarded answers without anyone opening a path at its side.
async fn answers_directly(endpoint: &quinn::Endpoint, host: SocketAddr) {
    tokio::time::sleep(DIRECT_TRY_AFTER).await;
    loop {
        if let Ok(connecting) = endpoint.connect(host, "tidedesk-host")
            && let Ok(Ok(conn)) = tokio::time::timeout(DIRECT_TRY_TIMEOUT, connecting).await
        {
            conn.close(0u32.into(), b"reachable");
            return;
        }
        tokio::time::sleep(DIRECT_TRY_EVERY).await;
    }
}

/// `host`, `host:port`, `[v6]:port` → `(display form, socket address)`.
async fn resolve(host: &str) -> Result<(String, SocketAddr)> {
    let with_port = net::with_default_port(host, DEFAULT_PORT);
    let addr = tokio::net::lookup_host(&with_port)
        .await
        .with_context(|| format!("resolving {with_port}"))?
        .next()
        .with_context(|| format!("{with_port} did not resolve to an address"))?;
    Ok((with_port.to_lowercase(), addr))
}

/// What a host presents before any secret is exchanged.
#[derive(Clone)]
pub struct Probe {
    /// The key used for pinning: normalised `host:port`, or the internet address.
    pub address: String,
    pub fingerprint: String,
    pub status: PinStatus,
}

/// A way to a host, ready for handshakes: for an internet route, the path is
/// already open and kept alive until [`Dialer::connect`] is done.
pub struct Dialer {
    address: String,
    endpoint: quinn::Endpoint,
    route: RouteInfo,
    /// Where `known_hosts.txt` lives; the user's configuration by default.
    config_dir: Option<PathBuf>,
    /// Keeps the punched path's keepalives running.
    _agent: Option<Arc<Agent>>,
}

impl Dialer {
    /// Prepares a way to `host`. For [`Route::Internet`] this learns this
    /// computer's internet address, reports it through `progress` for the
    /// person at the host, and punches until the host opens its side.
    pub async fn new(host: &str, route: &Route, progress: impl Fn(Progress)) -> Result<Self> {
        let stun_servers = match route {
            Route::Direct => {
                let (address, peer) = resolve(host).await?;
                return Ok(Self {
                    address,
                    endpoint: net::client_endpoint()?,
                    route: RouteInfo {
                        kind: RouteKind::Direct,
                        peer,
                        observed_self: None,
                    },
                    config_dir: None,
                    _agent: None,
                });
            }
            Route::Internet { stun_servers } => stun_servers,
        };

        let host_addr: SocketAddr = host
            .trim()
            .parse()
            .ok()
            .filter(SocketAddr::is_ipv4)
            .with_context(|| format!("{host} is not an IPv4 address with a port"))?;
        // IPv4 only: internet paths are IPv4, and a dual-stack socket would
        // send to the STUN servers and the host from a different mapping.
        let (socket, side_channel) = SharedSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0)))
            .context("opening a UDP socket")?;
        // The endpoint is also what reads the socket for the agent.
        let endpoint = net::client_endpoint_on(socket.clone())?;
        let agent = Agent::spawn(socket, side_channel)?;

        progress(Progress::Status(
            "Looking up this computer's internet address…".into(),
        ));
        let public = agent
            .discover(resolve_servers(stun_servers).await)
            .await
            .map_err(|reason| {
                anyhow!(
                    "could not learn this computer's internet address ({reason}); check the \
                     internet connection and the STUN servers"
                )
            })?;
        // Checked first: on one network the local address works whatever the NAT.
        if public.addr.ip() == host_addr.ip() {
            bail!(
                "the host has the same internet address as this computer ({}), so both are on \
                 the same network: connect to one of the host's local addresses instead, \
                 without --internet (the host's window lists them)",
                public.addr.ip()
            );
        }
        if public.nat == NatKind::Symmetric {
            bail!(symmetric_nat());
        }
        progress(Progress::ViewerAddress(public.addr));
        progress(Progress::Status(format!(
            "Waiting for the host to open a path to {}…",
            public.addr
        )));
        let punching = agent.punch(host_addr, Some(new_session()), PUNCH_WINDOW);
        let peer = tokio::select! {
            punched = punching => match punched {
                Ok(path) => {
                    progress(Progress::PathOpen(path.clone()));
                    path.peer
                }
                Err(PunchError::NoReply) => bail!(no_reply(host_addr, public.addr)),
                Err(PunchError::Stopped) => bail!("stopped opening a path to {host_addr}"),
            },
            () = answers_directly(&endpoint, host_addr) => {
                progress(Progress::Status(format!(
                    "{host_addr} answered directly (its port is forwarded)."
                )));
                host_addr
            }
        };
        Ok(Self {
            address: host_addr.to_string(),
            endpoint,
            route: RouteInfo {
                kind: RouteKind::Internet,
                peer,
                observed_self: Some(public.addr),
            },
            config_dir: None,
            _agent: Some(agent),
        })
    }

    /// Completes only the encrypted handshake to learn the host's fingerprint
    /// and how it compares with the pinned one, so a UI can ask before
    /// connecting.
    pub async fn probe(&self) -> Result<Probe> {
        let conn = self.handshake().await?;
        let fingerprint = net::peer_fingerprint(&conn).context("host presented no certificate")?;
        conn.close(0u32.into(), b"probe");
        let status = self.pin_status(&self.known_hosts()?, &fingerprint);
        Ok(Probe {
            address: self.address.clone(),
            fingerprint,
            status,
        })
    }

    pub async fn connect(self, opts: &ConnectOptions) -> Result<Session> {
        let conn = self.handshake().await?;

        // Verify who we are talking to *before* using the access code.
        let fp = net::peer_fingerprint(&conn).context("host presented no certificate")?;
        let display = &self.address;
        let mut known = self.known_hosts()?;
        match self.pin_status(&known, &fp) {
            // Possibly under another address: an internet address and port
            // can change with every restart, so this one is not remembered.
            PinStatus::Trusted => {}
            PinStatus::Unknown => match &opts.expected_fingerprint {
                Some(expected) if normalize_fingerprint(expected) != normalize_fingerprint(&fp) => {
                    bail!("host fingerprint {fp} does not match the one you supplied");
                }
                Some(_) => known.pin(display, &fp)?,
                None => {
                    eprintln!(
                        "First connection to {display}.\n  Host fingerprint: {fp}\n  \
                         It should match the fingerprint shown in the host's window. Remembering it."
                    );
                    known.pin(display, &fp)?;
                }
            },
            PinStatus::Mismatch { pinned } => {
                if !opts.accept_new_fingerprint {
                    conn.close(0u32.into(), b"fingerprint mismatch");
                    bail!(
                        "HOST IDENTITY CHANGED for {display}!\n  Remembered: {pinned}\n  Presented:  {}\n\
                         Someone may be intercepting the connection, or the host was reinstalled.\n\
                         If you are sure it is legitimate, reconnect with --accept-new-fingerprint.",
                        normalize_fingerprint(&fp)
                    );
                }
                known.pin(display, &fp)?;
            }
        }

        let (mut send, mut recv) = conn.open_bi().await?;
        let hello = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| "viewer".into()),
            auth_tag: auth::client_tag(&conn, &opts.code)?,
            want_audio: opts.want_audio,
        };
        protocol::write_message(&mut send, &hello).await?;

        match protocol::read_message::<_, ServerMessage>(&mut recv).await? {
            Some(ServerMessage::Welcome {
                host_name,
                width,
                height,
                audio,
            }) => Ok(Session {
                endpoint: self.endpoint,
                conn,
                send,
                recv,
                host_name,
                fingerprint: fp,
                width,
                height,
                audio,
                route: self.route,
            }),
            Some(ServerMessage::Rejected { reason }) => {
                bail!("host refused the connection: {reason}")
            }
            Some(_) => bail!("host sent session data before Welcome"),
            None => bail!("host closed the connection during the handshake"),
        }
    }

    async fn handshake(&self) -> Result<quinn::Connection> {
        let peer = self.route.peer;
        self.endpoint
            .connect(peer, "tidedesk-host")?
            .await
            .with_context(|| format!("could not reach a TideDesk host at {peer}"))
    }

    fn known_hosts(&self) -> Result<KnownHosts> {
        match &self.config_dir {
            Some(dir) => KnownHosts::load(dir),
            None => KnownHosts::load(&paths::config_dir()?),
        }
    }

    fn pin_status(&self, known: &KnownHosts, fp: &str) -> PinStatus {
        match self.route.kind {
            RouteKind::Direct => known.check(&self.address, fp),
            // A router's public address and port change while the host does not.
            RouteKind::Internet => known.check_fingerprint_first(&self.address, fp),
        }
    }

    #[cfg(test)]
    fn with_config_dir(mut self, dir: PathBuf) -> Self {
        self.config_dir = Some(dir);
        self
    }
}

/// Learns a directly reachable host's fingerprint; see [`Dialer::probe`].
pub async fn probe(host: &str) -> Result<Probe> {
    let dialer = Dialer::new(host, &Route::Direct, |_| {}).await?;
    let probe = dialer.probe().await?;
    // Let the probe's close reach the host before the socket goes away.
    let _ = tokio::time::timeout(Duration::from_millis(300), dialer.endpoint.wait_idle()).await;
    Ok(probe)
}

pub async fn connect(opts: &ConnectOptions, progress: impl Fn(Progress)) -> Result<Session> {
    Dialer::new(&opts.host, &opts.route, progress)
        .await?
        .connect(opts)
        .await
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Mutex;

    use tidedesk_core::identity::HostIdentity;
    use tidedesk_core::nat::stun::{self, TransactionId};

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tidedesk-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn internet_route_rejects_ipv6_and_hostnames() {
        assert_eq!(
            parse_internet_host(" 203.0.113.5:40000 ").unwrap(),
            "203.0.113.5:40000".parse::<SocketAddr>().unwrap()
        );
        for bad in [
            "my-pc:47800",
            "my-pc",
            "203.0.113.5",
            "[2001:db8::1]:40000",
            "192.168.1.5:47800",
            "203.0.113.5:0",
        ] {
            assert!(parse_internet_host(bad).is_err(), "{bad}");
        }
        let local = parse_internet_host("192.168.1.5:47800").unwrap_err();
        assert!(local.to_string().contains("without --internet"), "{local}");
    }

    #[tokio::test]
    async fn dialer_direct_route_still_resolves_default_port() {
        let dialer = Dialer::new("127.0.0.1", &Route::Direct, |_| {})
            .await
            .unwrap();
        assert_eq!(dialer.address, "127.0.0.1:47800");
        let direct = RouteInfo {
            kind: RouteKind::Direct,
            peer: "127.0.0.1:47800".parse().unwrap(),
            observed_self: None,
        };
        assert_eq!(dialer.route, direct);
        assert_eq!(direct.to_string(), "direct to 127.0.0.1:47800");
    }

    /// A STUN server that answers as if the viewer sat behind a router
    /// mapping 127.0.0.1:port to `public`:port.
    async fn fake_stun_behind_router(public: Ipv4Addr) -> SocketAddr {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 1500];
            while let Ok((len, from)) = socket.recv_from(&mut buf).await {
                if !stun::is_stun(&buf[..len]) {
                    continue;
                }
                let id: TransactionId = buf[8..20].try_into().unwrap();
                let mapped = SocketAddr::new(public.into(), from.port());
                let reply = stun::encode_binding_response(&id, mapped);
                let _ = socket.send_to(&reply, from).await;
            }
        });
        addr
    }

    /// Welcomes every viewer that says Hello, without checking its code;
    /// handshake-only connections (probes) just end.
    fn fake_host(endpoint: quinn::Endpoint) {
        tokio::spawn(async move {
            while let Some(incoming) = net::accept_validated(&endpoint).await {
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    let Ok((mut send, mut recv)) = conn.accept_bi().await else {
                        return;
                    };
                    let hello = protocol::read_message::<_, ClientMessage>(&mut recv).await;
                    if matches!(hello, Ok(Some(ClientMessage::Hello { .. }))) {
                        let welcome = ServerMessage::Welcome {
                            host_name: "test-host".into(),
                            width: 640,
                            height: 480,
                            audio: false,
                        };
                        let _ = protocol::write_message(&mut send, &welcome).await;
                    }
                    conn.closed().await;
                });
            }
        });
    }

    fn options(host: SocketAddr, route: Route) -> ConnectOptions {
        ConnectOptions {
            host: host.to_string(),
            code: "ABCD-EFGH".into(),
            want_audio: false,
            expected_fingerprint: None,
            accept_new_fingerprint: false,
            route,
        }
    }

    #[tokio::test]
    async fn internet_route_uses_a_forwarded_port_without_punching() {
        // Nobody at the host opens a path, but its UDP port is forwarded:
        // the host answers QUIC at its address, and never a punch.
        let dir = temp_dir("dialer-forwarded");
        let identity = HostIdentity::load_or_create(&dir).unwrap();
        let (host_socket, _) = SharedSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let host_addr = host_socket.local_addr().unwrap();
        fake_host(net::server_endpoint_on(host_socket, &identity).unwrap());
        let stun = fake_stun_behind_router(Ipv4Addr::new(203, 0, 113, 9)).await;
        // Known from an earlier connection at another public port.
        let earlier = "203.0.113.5:40999";
        KnownHosts::load(&dir)
            .unwrap()
            .pin(earlier, &identity.fingerprint())
            .unwrap();

        let route = Route::Internet {
            stun_servers: vec![stun.to_string()],
        };
        let dialer = tokio::time::timeout(
            Duration::from_secs(20),
            Dialer::new(&host_addr.to_string(), &route, |_| {}),
        )
        .await
        .expect("the forwarded port is tried long before the punch window ends")
        .unwrap()
        .with_config_dir(dir.clone());
        let session = dialer.connect(&options(host_addr, route)).await.unwrap();
        assert_eq!(session.route.kind, RouteKind::Internet);
        assert_eq!(session.route.peer, host_addr);

        // Trusted by its fingerprint; the ephemeral address is not remembered.
        let known = KnownHosts::load(&dir).unwrap();
        assert_eq!(known.addresses().collect::<Vec<_>>(), [earlier]);
        session.conn.close(0u32.into(), b"done");
    }

    #[tokio::test]
    async fn viewer_dialer_connects_through_punched_path_to_host_endpoint() {
        let dir = temp_dir("dialer-internet");
        let identity = HostIdentity::load_or_create(&dir).unwrap();
        let (host_socket, host_tap) = SharedSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let host_addr = host_socket.local_addr().unwrap();
        let host_endpoint = net::server_endpoint_on(host_socket.clone(), &identity).unwrap();
        let host_agent = Agent::spawn(host_socket, host_tap).unwrap();
        fake_host(host_endpoint);
        let stun = fake_stun_behind_router(Ipv4Addr::new(203, 0, 113, 9)).await;

        // The person at the host types the address the viewer shows; the
        // simulated router maps it back to the viewer's loopback port.
        let shown = Arc::new(Mutex::new(None));
        let progress = {
            let (host_agent, shown) = (host_agent.clone(), shown.clone());
            move |step: Progress| {
                if let Progress::ViewerAddress(addr) = step {
                    *shown.lock().unwrap() = Some(addr);
                    let behind_router = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), addr.port());
                    let host_agent = host_agent.clone();
                    tokio::spawn(async move {
                        let _ = host_agent
                            .punch(behind_router, None, Duration::from_secs(30))
                            .await;
                    });
                }
            }
        };
        let route = Route::Internet {
            stun_servers: vec![stun.to_string()],
        };
        let dialer = Dialer::new(&host_addr.to_string(), &route, progress)
            .await
            .unwrap()
            .with_config_dir(dir);
        let session = dialer.connect(&options(host_addr, route)).await.unwrap();

        assert_eq!(session.fingerprint, identity.fingerprint());
        assert_eq!(session.host_name, "test-host");
        assert_eq!(session.route.kind, RouteKind::Internet);
        assert_eq!(session.route.peer, host_addr);
        let shown = shown
            .lock()
            .unwrap()
            .expect("the viewer showed its address");
        assert_eq!(shown.ip(), IpAddr::from([203, 0, 113, 9]));
        session.conn.close(0u32.into(), b"done");
    }
}
