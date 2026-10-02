//! Connecting to a host: route, QUIC handshake, fingerprint pinning, auth.
//!
//! A host is reached directly at its address (same network, a VPN or a
//! forwarded port), or over the internet through a path both computers punch
//! through their routers (see `tidedesk_core::nat`). Either way every byte
//! goes straight between the two computers: TideDesk never relays.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tidedesk_core::identity::{KnownHosts, PinStatus, normalize_fingerprint};
use tidedesk_core::nat::candidates;
use tidedesk_core::nat::punch::new_session;
use tidedesk_core::nat::signal::{Introduction, LookupOutcome, resolve_service};
use tidedesk_core::nat::stun::{DEFAULT_STUN_SERVERS, resolve_servers};
use tidedesk_core::nat::{
    Agent, DeviceId, NatKind, NotPublic, PunchError, Punched, SharedSocket, check_public,
};
use tidedesk_core::protocol::{self, ClientMessage, PROTOCOL_VERSION, RejectReason, ServerMessage};
use tidedesk_core::{DEFAULT_PORT, auth, net, password, paths};

/// How long to punch towards the host: the person there has this long to
/// type this computer's address and press Open.
pub const PUNCH_WINDOW: Duration = Duration::from_secs(120);

/// How long to punch towards a host the rendezvous service introduced: it
/// punches back at once, for 30 seconds.
const RENDEZVOUS_PUNCH_WINDOW: Duration = Duration::from_secs(20);

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
    /// By device ID, through a rendezvous service (`host[:port]`) that
    /// introduces the two computers; the path is punched as for `Internet`.
    /// The local network is asked for the ID at the same time, and a host
    /// found there is reached directly, as is a host at this computer's own
    /// internet address at the local addresses it sealed with its access code.
    Rendezvous { service: String },
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
    Rendezvous,
    /// By device ID, found on this computer's own network.
    LocalNetwork,
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
            RouteKind::Rendezvous => "by device ID, direct",
            RouteKind::LocalNetwork => "by device ID on this network, direct",
        };
        write!(f, "{kind} to {}", self.peer)?;
        if let Some(me) = self.observed_self {
            write!(f, ", we appear as {me}")?;
        }
        Ok(())
    }
}

/// A device ID in the host argument: `TD-1A2B-3C4D-5E6F-7A8B`, in any case
/// and spacing. The `TD` is required, so a host name that happens to be 16
/// hex digits stays a host name.
pub fn parse_device_id(text: &str) -> Option<DeviceId> {
    let text = text.trim();
    let prefixed = text.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("td"));
    if prefixed { text.parse().ok() } else { None }
}

/// A device ID is the start of the host certificate's hash: the computer
/// that answered must be the one asked for.
fn verify_device_id(expected: DeviceId, fingerprint: &str) -> Result<()> {
    if DeviceId::from_fingerprint_hex(fingerprint) == Some(expected) {
        return Ok(());
    }
    bail!(
        "the computer that answered is not {expected}: its certificate does not match that \
         device ID. Someone may be intercepting the connection, or another computer answered \
         in its place."
    )
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
            bail!(
                "{addr} is a local network address: connect to it directly, not over the internet"
            )
        }
        Err(NotPublic::Unusable) => bail!("{addr} is not an address a host can have"),
    }
}

/// Why no path opened from a network with a symmetric NAT: TideDesk guessed
/// its ports (see `tidedesk_core::nat::punch`), and this router did not
/// hand them out in turn.
fn symmetric_nat_no_reply(host: impl std::fmt::Display) -> String {
    format!(
        "{host} did not answer. This network uses a symmetric NAT (common on mobile data and \
         carrier-grade NAT), which gives every destination a port of its own: TideDesk tried \
         the likely ones, but this router picks them some other way. TideDesk never relays sessions: let the host's router open its port (on the host: \
         Settings, Network), use a VPN such as Tailscale, or forward UDP port {DEFAULT_PORT} \
         on the host's router. See docs/internet-access.md."
    )
}

fn no_reply(host: SocketAddr, me: SocketAddr) -> String {
    format!(
        "could not open a direct path to {host}. Make sure the host has this computer's internet \
         address ({me}) entered and Open pressed within the last two minutes. If both networks \
         use a symmetric NAT (common on mobile data and carrier-grade NAT), a direct connection \
         is impossible. TideDesk never relays sessions: let the host's router open its port (on the host: \
         Settings, Network), use a VPN such as Tailscale, or forward UDP port {DEFAULT_PORT} \
         on the host's router. See docs/internet-access.md."
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

/// Where to ask this computer's own networks for a device ID: the default
/// port at the limited broadcast address, which some systems send out of one
/// adapter only, and at each IPv4 network's own broadcast address.
fn lan_targets() -> Vec<SocketAddr> {
    let mut targets = vec![SocketAddr::from((Ipv4Addr::BROADCAST, DEFAULT_PORT))];
    for interface in if_addrs::get_if_addrs().unwrap_or_default() {
        if let if_addrs::IfAddr::V4(v4) = &interface.addr
            && !v4.is_loopback()
            && let Some(broadcast) = directed_broadcast(v4.ip, v4.prefixlen)
        {
            let target = SocketAddr::from((broadcast, DEFAULT_PORT));
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
    }
    targets
}

/// The broadcast address of the network `ip/prefix`; none for point-to-point
/// links, which have no room for one.
fn directed_broadcast(ip: Ipv4Addr, prefix: u8) -> Option<Ipv4Addr> {
    (1..=30)
        .contains(&prefix)
        .then(|| Ipv4Addr::from(u32::from(ip) | (u32::MAX >> prefix)))
}

/// How long a host found on this network, or at a local address it sealed,
/// has to complete a handshake.
const LAN_CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// Why the computer at an address is not the host asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotTheHost {
    /// No handshake within [`LAN_CHECK_TIMEOUT`].
    Unreachable,
    /// A handshake, but the certificate does not hash to the device ID.
    Impostor,
}

/// Completes a handshake with `peer` and checks that its certificate hashes
/// to `device_id`. Whatever sent this computer to `peer` is not trusted: a
/// wrong address only fails here.
async fn check_host(
    endpoint: &quinn::Endpoint,
    peer: SocketAddr,
    device_id: DeviceId,
) -> Result<(), NotTheHost> {
    let Ok(connecting) = endpoint.connect(peer, "tidedesk-host") else {
        return Err(NotTheHost::Unreachable);
    };
    let Ok(Ok(conn)) = tokio::time::timeout(LAN_CHECK_TIMEOUT, connecting).await else {
        return Err(NotTheHost::Unreachable);
    };
    let fingerprint = net::peer_fingerprint(&conn).unwrap_or_default();
    conn.close(0u32.into(), b"checked");
    verify_device_id(device_id, &fingerprint).map_err(|_| NotTheHost::Impostor)
}

/// The way to a host by device ID on this computer's own networks: asks
/// `lan` (their broadcast addresses), then checks the certificate of the
/// computer that answered, since an answer alone proves nothing. Checked
/// here, so a computer answering in the host's place cannot end the
/// service's way to the real one.
async fn on_this_network(
    agent: &Agent,
    endpoint: &quinn::Endpoint,
    device_id: DeviceId,
    lan: Vec<SocketAddr>,
) -> Result<SocketAddr> {
    let peer = agent
        .find_on_lan(device_id, lan)
        .await
        .with_context(|| format!("{device_id} was not found on this network"))?;
    match check_host(endpoint, peer, device_id).await {
        Ok(()) => Ok(peer),
        Err(NotTheHost::Unreachable) => {
            bail!("{device_id} answered on this network at {peer} but could not be reached there")
        }
        Err(NotTheHost::Impostor) => bail!(
            "the computer that answered for {device_id} on this network ({peer}) is not it: its \
             certificate does not match the device ID, so someone may be pretending to be it"
        ),
    }
}

/// Which of the local `addresses` a host sealed is the host: all are dialled
/// at once, and the first whose certificate matches `device_id` wins.
async fn at_local_addresses(
    endpoint: &quinn::Endpoint,
    device_id: DeviceId,
    addresses: &[SocketAddr],
) -> Result<SocketAddr> {
    let mut attempts = tokio::task::JoinSet::new();
    for &peer in addresses {
        let endpoint = endpoint.clone();
        attempts.spawn(async move { (peer, check_host(&endpoint, peer, device_id).await) });
    }
    let mut failed = Vec::new();
    while let Some(attempt) = attempts.join_next().await {
        match attempt {
            Ok((peer, Ok(()))) => return Ok(peer),
            Ok((peer, Err(NotTheHost::Unreachable))) => {
                failed.push(format!("{peer} did not answer"))
            }
            Ok((peer, Err(NotTheHost::Impostor))) => {
                failed.push(format!("{peer} is another computer"));
            }
            Err(e) => failed.push(format!("an attempt failed: {e}")),
        }
    }
    bail!(
        "{device_id} was not reached at the local addresses it gave: {}",
        failed.join(", ")
    )
}

/// The local addresses a host sealed for viewers with its access code (see
/// `tidedesk_core::nat::candidates`), when worth trying: only when the host
/// is at this computer's own internet address, so the two share a network,
/// and only with the code that sealed them. A wrong code opens nothing here
/// and is refused by the host later, as always.
fn local_addresses_to_try(
    introduction: &Introduction,
    device_id: DeviceId,
    code: Option<&str>,
) -> Vec<SocketAddr> {
    if introduction.reflexive.ip() != introduction.peer.ip() {
        return Vec::new();
    }
    code.and_then(|code| candidates::unseal(code, device_id, &introduction.candidates))
        .unwrap_or_default()
        .into_iter()
        .map(SocketAddr::V4)
        .collect()
}

/// Why a host at this computer's own internet address could not be reached.
fn same_network_no_reply(device_id: DeviceId, ip: IpAddr, tried: &[SocketAddr]) -> String {
    let tried = if tried.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = tried.iter().map(ToString::to_string).collect();
        format!(
            " The local addresses it gave ({}) did not answer either.",
            list.join(", ")
        )
    };
    format!(
        "{device_id} did not answer. It has the same internet address as this computer ({ip}), \
         so both are on the same network and the router may not loop traffic back.{tried} \
         Connect directly to one of the host's local addresses instead (the host's window \
         lists them)"
    )
}

/// The service's way to a host by device ID: it introduces this computer to
/// the host, then both punch. A host at this computer's own internet address
/// is first tried at the local addresses it sealed for viewers with its
/// access `code`: on one network those work whatever the router does.
async fn through_service(
    agent: &Agent,
    endpoint: &quinn::Endpoint,
    device_id: DeviceId,
    service: &str,
    code: Option<&str>,
    progress: &impl Fn(Progress),
) -> Result<RouteInfo> {
    let main = resolve_service(service)
        .await
        .with_context(|| format!("cannot find the connection service {service}"))?;
    let introduction = match agent.lookup(service.to_string(), main, device_id).await {
        LookupOutcome::Introduced(introduction) => introduction,
        LookupOutcome::NotFound => {
            bail!("{device_id} is not online right now (not registered at {service})")
        }
        LookupOutcome::Unreachable(reason) => bail!("{reason}"),
    };
    // A symmetric NAT here is tried anyway: the host guesses its ports.
    let peer = introduction.peer;
    // Opening the seal derives a key from the code, which takes a while:
    // off the runtime's threads, so the search of this network goes on.
    let local = {
        let introduction = introduction.clone();
        let code = code.map(str::to_owned);
        tokio::task::spawn_blocking(move || {
            local_addresses_to_try(&introduction, device_id, code.as_deref())
        })
        .await
        .unwrap_or_default()
    };
    if !local.is_empty() {
        progress(Progress::Status(format!(
            "{device_id} has this computer's internet address; trying the local addresses it \
             gave…"
        )));
        match at_local_addresses(endpoint, device_id, &local).await {
            Ok(found) => {
                progress(Progress::Status(format!(
                    "Found {device_id} at {found} on this network."
                )));
                return Ok(RouteInfo {
                    kind: RouteKind::LocalNetwork,
                    peer: found,
                    observed_self: None,
                });
            }
            Err(e) => progress(Progress::Status(format!("{e:#}."))),
        }
    }
    progress(Progress::Status(format!(
        "Opening a path to {device_id} at {peer}…"
    )));
    let path = match agent
        .punch(peer, Some(introduction.session), RENDEZVOUS_PUNCH_WINDOW)
        .await
    {
        Ok(path) => path,
        // Tried anyway: some routers do loop traffic back to themselves.
        Err(_) if introduction.reflexive.ip() == peer.ip() => {
            bail!(same_network_no_reply(device_id, peer.ip(), &local))
        }
        Err(_) if introduction.nat == NatKind::Symmetric => {
            bail!(symmetric_nat_no_reply(device_id))
        }
        Err(_) => bail!(
            "{device_id} did not answer at {peer}. If both networks use a symmetric NAT \
             (common on mobile data and carrier-grade NAT), a direct connection is \
             impossible. TideDesk never relays sessions: let the host's router open its port (on the host: \
             Settings, Network), use a VPN such as Tailscale, or forward UDP port {DEFAULT_PORT} \
             on the host's router. See docs/internet-access.md."
        ),
    };
    progress(Progress::PathOpen(path.clone()));
    Ok(RouteInfo {
        kind: RouteKind::Rendezvous,
        peer: path.peer,
        observed_self: Some(introduction.reflexive),
    })
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
    /// For the rendezvous route: the host must be this one.
    device_id: Option<DeviceId>,
    /// Keeps the punched path's keepalives running.
    _agent: Option<Arc<Agent>>,
}

impl Dialer {
    /// Prepares a way to `host`. For [`Route::Internet`] this learns this
    /// computer's internet address, reports it through `progress` for the
    /// person at the host, and punches until the host opens its side. For
    /// [`Route::Rendezvous`], the access `code` opens the local addresses a
    /// host on this computer's own network sealed for viewers that know it.
    pub async fn new(
        host: &str,
        route: &Route,
        code: Option<&str>,
        progress: impl Fn(Progress),
    ) -> Result<Self> {
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
                    device_id: None,
                    _agent: None,
                });
            }
            Route::Internet { stun_servers } => stun_servers,
            Route::Rendezvous { service } => {
                return Self::by_device_id(host, service, code, lan_targets(), progress).await;
            }
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
                 the same network: connect directly to one of the host's local addresses \
                 instead (the host's window lists them)",
                public.addr.ip()
            );
        }
        // A symmetric NAT here is tried anyway: the host guesses its ports.
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
                Err(PunchError::NoReply) if public.nat == NatKind::Symmetric => {
                    bail!(symmetric_nat_no_reply(host_addr))
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
            device_id: None,
            _agent: Some(agent),
        })
    }

    /// The device-ID route: asks this computer's own networks (`lan`, their
    /// broadcast addresses) and the service at once and takes whichever finds
    /// the host first. A host found on the network is dialled directly; one
    /// the service introduces, through a punched path, unless it is on this
    /// computer's network and `code` opens the local addresses it sealed.
    async fn by_device_id(
        host: &str,
        service: &str,
        code: Option<&str>,
        lan: Vec<SocketAddr>,
        progress: impl Fn(Progress),
    ) -> Result<Self> {
        let device_id = parse_device_id(host).with_context(|| {
            format!("{host} is not a device ID (they look like TD-1A2B-3C4D-5E6F-7A8B)")
        })?;
        let socket = std::net::UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0)))
            .context("opening a UDP socket")?;
        // The local-network query goes to broadcast addresses.
        socket.set_broadcast(true).context("opening a UDP socket")?;
        let (socket, side_channel) =
            SharedSocket::from_std(socket).context("opening a UDP socket")?;
        let endpoint = net::client_endpoint_on(socket.clone())?;
        let agent = Agent::spawn(socket, side_channel)?;

        progress(Progress::Status(format!(
            "Looking for {device_id} on this network and at {service}…"
        )));
        // In a block, so the search that lost stops before the host is dialled.
        let route = {
            let on_lan = on_this_network(&agent, &endpoint, device_id, lan);
            let through_service =
                through_service(&agent, &endpoint, device_id, service, code, &progress);
            tokio::pin!(on_lan, through_service);
            let (mut lan_failed, mut service_failed) = (None, None);
            loop {
                tokio::select! {
                    found = &mut on_lan, if lan_failed.is_none() => match found {
                        Ok(peer) => {
                            progress(Progress::Status(format!(
                                "Found {device_id} on this network at {peer}."
                            )));
                            break RouteInfo {
                                kind: RouteKind::LocalNetwork,
                                peer,
                                observed_self: None,
                            };
                        }
                        Err(e) => lan_failed = Some(e),
                    },
                    opened = &mut through_service, if service_failed.is_none() => match opened {
                        Ok(route) => break route,
                        Err(e) => service_failed = Some(e),
                    },
                }
                // A failure only counts once the other way has failed too.
                if let (Some(lan), Some(service)) = (&lan_failed, &service_failed) {
                    bail!("{lan:#}, and {service:#}");
                }
            }
        };
        Ok(Self {
            address: device_id.to_string(),
            endpoint,
            route,
            config_dir: None,
            device_id: Some(device_id),
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
        if let Some(device_id) = self.device_id {
            verify_device_id(device_id, &fingerprint)?;
        }
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
        if let Some(device_id) = self.device_id
            && let Err(e) = verify_device_id(device_id, &fp)
        {
            conn.close(0u32.into(), b"not the host asked for");
            return Err(e);
        }
        let display = &self.address;
        let mut known = self.known_hosts()?;
        let status = self.pin_status(&known, &fp);
        // A password goes only to a host verified before this connection:
        // met before, reached by device ID, or its fingerprint given.
        let verified = match &status {
            PinStatus::Trusted => true,
            PinStatus::Unknown => opts.expected_fingerprint.as_ref().is_some_and(|expected| {
                normalize_fingerprint(expected) == normalize_fingerprint(&fp)
            }),
            PinStatus::Mismatch { .. } => false,
        };
        match status {
            // An internet address and port can change with every restart, so
            // one trusted by its fingerprint is not remembered. A device ID
            // is stable: remember it, so the launcher lists it as recent.
            PinStatus::Trusted => {
                if self.device_id.is_some() && known.check(display, &fp) != PinStatus::Trusted {
                    known.pin(display, &fp)?;
                }
            }
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
        let client_name = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "viewer".into());
        let answer = if !password::is_password(&opts.code) {
            let hello = ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                client_name,
                auth_tag: auth::client_tag(&conn, &opts.code)?,
                want_audio: opts.want_audio,
            };
            protocol::write_message(&mut send, &hello).await?;
            protocol::read_message::<_, ServerMessage>(&mut recv).await?
        } else {
            if !verified {
                conn.close(0u32.into(), b"password for an unverified host");
                bail!(
                    "a password is only used with a computer this viewer has connected to \
                     before: connect once with its access code"
                );
            }
            let password = opts.code.clone();
            let key = tokio::task::spawn_blocking({
                let fp = fp.clone();
                move || password::derive_key(&password, &fp)
            })
            .await??;
            let (viewer, start) = password::Viewer::start(&key, auth::session_binding(&conn)?);
            let hello = ClientMessage::PasswordHello {
                protocol_version: PROTOCOL_VERSION,
                client_name,
                want_audio: opts.want_audio,
                start,
            };
            protocol::write_message(&mut send, &hello).await?;
            match protocol::read_message::<_, ServerMessage>(&mut recv).await {
                Ok(Some(ServerMessage::PasswordAnswer { answer, proof })) => {
                    let Ok(mine) = viewer.finish(&answer, &proof) else {
                        conn.close(0u32.into(), b"wrong password");
                        bail!("host refused the connection: {}", RejectReason::BadPassword);
                    };
                    protocol::write_message(
                        &mut send,
                        &ClientMessage::PasswordProof { proof: mine },
                    )
                    .await?;
                    protocol::read_message::<_, ServerMessage>(&mut recv).await?
                }
                Ok(Some(other)) => Some(other),
                // A host from before passwords cannot read the message.
                Ok(None) | Err(_) => bail!(
                    "the host closed the connection: it may run a TideDesk without passwords. \
                     Use its access code, or update TideDesk there"
                ),
            }
        };

        match answer {
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
            // Checked against the device ID, which is the certificate's hash.
            RouteKind::Rendezvous | RouteKind::LocalNetwork => PinStatus::Trusted,
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
    let dialer = Dialer::new(host, &Route::Direct, None, |_| {}).await?;
    let probe = dialer.probe().await?;
    // Let the probe's close reach the host before the socket goes away.
    let _ = tokio::time::timeout(Duration::from_millis(300), dialer.endpoint.wait_idle()).await;
    Ok(probe)
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tidedesk_core::identity::HostIdentity;
    use tidedesk_core::nat::stun::{self, TransactionId};

    use super::*;

    fn temp_dir_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("tidedesk-test-{name}-{}", std::process::id()))
    }

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
        assert!(local.to_string().contains("directly"), "{local}");
    }

    #[test]
    fn device_ids_are_recognised_in_the_host_argument() {
        let id = DeviceId([0x1A, 0x2B, 0x3C, 0x4D, 0x5E, 0x6F, 0x7A, 0x8B]);
        assert_eq!(parse_device_id("TD-1A2B-3C4D-5E6F-7A8B"), Some(id));
        assert_eq!(parse_device_id(" td 1a2b 3c4d 5e6f 7a8b "), Some(id));
        assert_eq!(
            parse_device_id("1A2B3C4D5E6F7A8B"),
            None,
            "a host name made of hex digits stays a host name"
        );
        assert_eq!(parse_device_id("my-pc"), None);
        assert_eq!(parse_device_id("tdhost"), None);
    }

    #[test]
    fn a_host_must_match_the_device_id_asked_for() {
        let identity = HostIdentity::load_or_create(&temp_dir("device-id-check")).unwrap();
        assert!(verify_device_id(identity.device_id(), &identity.fingerprint()).is_ok());
        let err = verify_device_id(DeviceId([0; 8]), &identity.fingerprint()).unwrap_err();
        assert!(err.to_string().contains("TD-0000-0000-0000-0000"), "{err}");
    }

    #[tokio::test]
    async fn dialer_direct_route_still_resolves_default_port() {
        let dialer = Dialer::new("127.0.0.1", &Route::Direct, None, |_| {})
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
                    } else {
                        // As a host from before passwords: it cannot read the
                        // message, and the session ends.
                        conn.close(0u32.into(), b"");
                    }
                    conn.closed().await;
                });
            }
        });
    }

    /// A host with the saved password `key`: welcomes a viewer that proves
    /// it, and counts the password hellos it got.
    fn password_host(endpoint: quinn::Endpoint, key: password::Key) -> Arc<AtomicUsize> {
        let hellos = Arc::new(AtomicUsize::new(0));
        let counted = hellos.clone();
        tokio::spawn(async move {
            while let Some(incoming) = net::accept_validated(&endpoint).await {
                let counted = counted.clone();
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    let Ok((mut send, mut recv)) = conn.accept_bi().await else {
                        return;
                    };
                    let Ok(Some(ClientMessage::PasswordHello { start, .. })) =
                        protocol::read_message::<_, ClientMessage>(&mut recv).await
                    else {
                        return;
                    };
                    counted.fetch_add(1, Ordering::SeqCst);
                    let binding = auth::session_binding(&conn).unwrap();
                    let (host, answer, proof) =
                        password::Host::answer(&key, binding, &start).unwrap();
                    let message = ServerMessage::PasswordAnswer { answer, proof };
                    let _ = protocol::write_message(&mut send, &message).await;
                    let reply = protocol::read_message::<_, ClientMessage>(&mut recv).await;
                    let reply = match reply {
                        Ok(Some(ClientMessage::PasswordProof { proof }))
                            if host.accepts(&proof) =>
                        {
                            ServerMessage::Welcome {
                                host_name: "own-pc".into(),
                                width: 640,
                                height: 480,
                                audio: false,
                            }
                        }
                        _ => ServerMessage::Rejected {
                            reason: RejectReason::BadPassword,
                        },
                    };
                    let _ = protocol::write_message(&mut send, &reply).await;
                    conn.closed().await;
                });
            }
        });
        hellos
    }

    /// A host this viewer has met takes the saved password; a wrong one is
    /// refused; one never met gets no password at all; one from before
    /// passwords is named as such.
    #[tokio::test]
    async fn a_saved_password_opens_a_host_met_before() {
        let dir = temp_dir("password");
        let identity = HostIdentity::load_or_create(&dir).unwrap();
        let fingerprint = identity.fingerprint();
        let key = password::derive_key("correct horse battery", &fingerprint).unwrap();
        let (socket, _) = SharedSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let host = socket.local_addr().unwrap();
        let hellos = password_host(net::server_endpoint_on(socket, &identity).unwrap(), key);
        let connect = |code: &str, dir: PathBuf| {
            let code = code.to_string();
            async move {
                let dialer = Dialer::new(&host.to_string(), &Route::Direct, None, |_| {})
                    .await
                    .unwrap()
                    .with_config_dir(dir);
                let options = ConnectOptions {
                    code,
                    ..options(host, Route::Direct)
                };
                dialer.connect(&options).await
            }
        };

        // Never met: no password goes out.
        let why = connect("correct horse battery", dir.clone())
            .await
            .err()
            .unwrap();
        assert!(
            format!("{why:#}").contains("connect once with its access code"),
            "{why:#}"
        );
        assert_eq!(hellos.load(Ordering::SeqCst), 0);

        // Met before.
        KnownHosts::load(&dir)
            .unwrap()
            .pin(&host.to_string(), &fingerprint)
            .unwrap();
        let session = connect("correct horse battery", dir.clone()).await.unwrap();
        assert_eq!(session.host_name, "own-pc");
        session.conn.close(0u32.into(), b"done");

        let why = connect("battery horse correct", dir.clone())
            .await
            .err()
            .unwrap();
        assert!(format!("{why:#}").contains("wrong password"), "{why:#}");
        assert_eq!(hellos.load(Ordering::SeqCst), 2);

        // A host from before passwords.
        let old_dir = temp_dir("password-old-host");
        let old_identity = HostIdentity::load_or_create(&old_dir).unwrap();
        let (socket, _) = SharedSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let old = socket.local_addr().unwrap();
        fake_host(net::server_endpoint_on(socket, &old_identity).unwrap());
        KnownHosts::load(&old_dir)
            .unwrap()
            .pin(&old.to_string(), &old_identity.fingerprint())
            .unwrap();
        let dialer = Dialer::new(&old.to_string(), &Route::Direct, None, |_| {})
            .await
            .unwrap()
            .with_config_dir(old_dir);
        let options = ConnectOptions {
            code: "correct horse battery".into(),
            ..options(old, Route::Direct)
        };
        let why = dialer.connect(&options).await.err().unwrap();
        assert!(format!("{why:#}").contains("without passwords"), "{why:#}");
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
            Dialer::new(&host_addr.to_string(), &route, None, |_| {}),
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

    /// A real rendezvous service on this machine, named by
    /// `TIDEDESK_TEST_SERVICE=ip:port`; its second port must be the next one
    /// up. The tests that need one are ignored until it is set:
    /// `cargo test -p tidedesk-view -- --ignored viewer_dialer_connects_by_device_id`.
    /// The service is not part of this repository; its own CI runs them.
    fn external_service() -> SocketAddr {
        let service: SocketAddr = std::env::var("TIDEDESK_TEST_SERVICE")
            .expect("TIDEDESK_TEST_SERVICE=ip:port names a rendezvous service on this machine")
            .parse()
            .expect("TIDEDESK_TEST_SERVICE is an ip:port");
        // The tests compare addresses the service reports with loopback ones.
        assert!(
            service.ip().is_loopback(),
            "TIDEDESK_TEST_SERVICE must be on this machine (127.0.0.1:port), not {service}"
        );
        service
    }

    #[tokio::test]
    #[ignore = "needs a rendezvous service on this machine: TIDEDESK_TEST_SERVICE=ip:port"]
    async fn viewer_dialer_connects_by_device_id() {
        use tidedesk_core::nat::AgentStatus;
        use tidedesk_core::nat::signal::RendezvousStatus;

        let service = external_service();
        let dir = temp_dir("dialer-device-id");
        let identity = HostIdentity::load_or_create(&dir).unwrap();
        let (host_socket, host_tap) = SharedSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let host_addr = host_socket.local_addr().unwrap();
        fake_host(net::server_endpoint_on(host_socket.clone(), &identity).unwrap());
        let host = Agent::spawn(host_socket, host_tap).unwrap();
        host.start_rendezvous(service.to_string(), identity.rendezvous_credentials());
        let registered =
            |s: &AgentStatus| matches!(s.rendezvous, RendezvousStatus::Registered { .. });
        tokio::time::timeout(Duration::from_secs(10), host.status().wait_for(registered))
            .await
            .expect("the host registers")
            .unwrap();

        let id = identity.device_id().to_string();
        let route = Route::Rendezvous {
            service: service.to_string(),
        };
        let dialer = Dialer::new(&id, &route, None, |_| {})
            .await
            .unwrap()
            .with_config_dir(dir);
        let probe = dialer.probe().await.unwrap();
        assert_eq!(
            probe.status,
            PinStatus::Trusted,
            "the ID vouches for the host"
        );
        assert_eq!(probe.address, id);

        let session = dialer.connect(&options(host_addr, route)).await.unwrap();
        assert_eq!(session.route.kind, RouteKind::Rendezvous);
        assert_eq!(session.route.peer, host_addr);
        assert_eq!(session.fingerprint, identity.fingerprint());
        session.conn.close(0u32.into(), b"done");
        let known = KnownHosts::load(&temp_dir_path("dialer-device-id")).unwrap();
        assert!(
            known.addresses().any(|a| a == id),
            "remembered under the ID, for the recent list"
        );

        let unknown = Dialer::new(
            "TD-0000-0000-0000-0001",
            &Route::Rendezvous {
                service: service.to_string(),
            },
            None,
            |_| {},
        )
        .await;
        let err = unknown.err().expect("nobody has that ID").to_string();
        assert!(err.contains("not online"), "{err}");
    }

    #[test]
    fn directed_broadcast_covers_the_subnet() {
        let broadcast = |ip: [u8; 4], prefix| directed_broadcast(Ipv4Addr::from(ip), prefix);
        assert_eq!(
            broadcast([192, 168, 1, 20], 24),
            Some(Ipv4Addr::new(192, 168, 1, 255))
        );
        assert_eq!(
            broadcast([172, 16, 5, 9], 20),
            Some(Ipv4Addr::new(172, 16, 15, 255))
        );
        assert_eq!(
            broadcast([10, 1, 2, 3], 8),
            Some(Ipv4Addr::new(10, 255, 255, 255))
        );
        // Point-to-point links have no broadcast address; /0 would be everything.
        for prefix in [0, 31, 32] {
            assert_eq!(broadcast([192, 168, 1, 20], prefix), None, "/{prefix}");
        }

        let targets = lan_targets();
        assert_eq!(
            targets.first(),
            Some(&SocketAddr::from((Ipv4Addr::BROADCAST, DEFAULT_PORT)))
        );
        assert!(targets.iter().all(|t| t.port() == DEFAULT_PORT));
        let mut unique = targets.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), targets.len(), "{targets:?}");
    }

    /// A host on loopback that answers local-network queries for `answers_for`
    /// while presenting `identity`'s certificate, for as long as the returned
    /// agent lives.
    fn host_on_this_network(
        identity: &HostIdentity,
        answers_for: DeviceId,
    ) -> (Arc<Agent>, SocketAddr) {
        let (socket, tap) = SharedSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = socket.local_addr().unwrap();
        fake_host(net::server_endpoint_on(socket.clone(), identity).unwrap());
        let agent = Agent::spawn(socket, tap).unwrap();
        agent.start_lan_discovery(answers_for);
        (agent, addr)
    }

    /// No IPv4 address: the service cannot even be looked up, as when offline.
    const OFFLINE_SERVICE: &str = "[::1]:47900";

    #[tokio::test]
    async fn dialer_finds_a_host_on_this_network_without_the_service() {
        let dir = temp_dir("dialer-lan");
        let identity = HostIdentity::load_or_create(&dir).unwrap();
        let (_host, host_addr) = host_on_this_network(&identity, identity.device_id());
        let id = identity.device_id().to_string();

        let statuses = Arc::new(Mutex::new(Vec::new()));
        let progress = {
            let statuses = statuses.clone();
            move |step| {
                if let Progress::Status(text) = step {
                    statuses.lock().unwrap().push(text);
                }
            }
        };
        let dialer = Dialer::by_device_id(&id, OFFLINE_SERVICE, None, vec![host_addr], progress)
            .await
            .unwrap()
            .with_config_dir(dir.clone());
        let found = RouteInfo {
            kind: RouteKind::LocalNetwork,
            peer: host_addr,
            observed_self: None,
        };
        assert_eq!(dialer.route, found);
        assert_eq!(
            found.to_string(),
            format!("by device ID on this network, direct to {host_addr}")
        );
        let statuses = statuses.lock().unwrap().clone();
        assert!(
            statuses
                .iter()
                .any(|s| s.contains("Found") && s.contains(&id)),
            "{statuses:?}"
        );

        let probe = dialer.probe().await.unwrap();
        assert_eq!(probe.status, PinStatus::Trusted, "the ID vouches for it");
        let route = Route::Rendezvous {
            service: OFFLINE_SERVICE.into(),
        };
        let session = dialer.connect(&options(host_addr, route)).await.unwrap();
        assert_eq!(session.route.kind, RouteKind::LocalNetwork);
        assert_eq!(session.fingerprint, identity.fingerprint());
        session.conn.close(0u32.into(), b"done");
        let known = KnownHosts::load(&dir).unwrap();
        assert!(
            known.addresses().any(|a| a == id),
            "remembered under the ID, for the recent list"
        );
    }

    #[tokio::test]
    async fn a_spoofed_lan_answer_cannot_impersonate_the_host() {
        let victim = HostIdentity::load_or_create(&temp_dir("lan-victim")).unwrap();
        let dir = temp_dir("lan-impostor");
        let impostor = HostIdentity::load_or_create(&dir).unwrap();
        let (_impostor, addr) = host_on_this_network(&impostor, victim.device_id());
        let id = victim.device_id().to_string();

        // Checked before it counts as found, so the service's way stays open;
        // here the service is unreachable too, and both are named.
        let refused = Dialer::by_device_id(&id, OFFLINE_SERVICE, None, vec![addr], |_| {})
            .await
            .err()
            .expect("the impostor is not taken for the host")
            .to_string();
        assert!(
            refused.contains(&format!(
                "answered for {id} on this network ({addr}) is not it"
            )),
            "{refused}"
        );
        assert!(refused.contains("connection service"), "{refused}");
    }

    #[tokio::test]
    async fn dialer_names_both_failures_when_nobody_answers() {
        // Bound, so the queries go somewhere, but nobody answers them.
        let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let lan = vec![silent.local_addr().unwrap()];
        let id = "TD-0000-0000-0000-0001";
        let err = Dialer::by_device_id(id, OFFLINE_SERVICE, None, lan, |_| {})
            .await
            .err()
            .expect("nobody has that ID")
            .to_string();
        assert!(
            err.contains(&format!("{id} was not found on this network")),
            "{err}"
        );
        assert!(err.contains("connection service"), "{err}");
        assert!(!err.contains("rendezvous"), "{err}");
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
        let dialer = Dialer::new(&host_addr.to_string(), &route, None, progress)
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

    #[tokio::test]
    async fn local_addresses_are_tried_at_once_and_checked_against_the_id() {
        let host = HostIdentity::load_or_create(&temp_dir("local-host")).unwrap();
        let impostor = HostIdentity::load_or_create(&temp_dir("local-impostor")).unwrap();
        let id = host.device_id();
        let (_host_agent, host_addr) = host_on_this_network(&host, id);
        let (_impostor_agent, impostor_addr) = host_on_this_network(&impostor, id);
        let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let nobody = silent.local_addr().unwrap();
        let (socket, _) = SharedSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = net::client_endpoint_on(socket).unwrap();

        let found = at_local_addresses(&endpoint, id, &[nobody, impostor_addr, host_addr])
            .await
            .unwrap();
        assert_eq!(found, host_addr);

        let err = at_local_addresses(&endpoint, id, &[impostor_addr, nobody])
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&format!("{impostor_addr} is another computer")),
            "{err}"
        );
        assert!(err.contains(&format!("{nobody} did not answer")), "{err}");
    }

    #[test]
    fn sealed_local_addresses_count_only_on_the_same_network_with_the_right_code() {
        let id = DeviceId([1; 8]);
        let local: std::net::SocketAddrV4 = "192.168.1.20:47800".parse().unwrap();
        let introduction = |reflexive: &str| Introduction {
            session: [5; 8],
            peer: "203.0.113.5:40000".parse().unwrap(),
            reflexive: reflexive.parse().unwrap(),
            nat: NatKind::Unknown,
            candidates: candidates::seal("K7QM-3XPA-WZ", id, &[local]),
        };
        let same = introduction("203.0.113.5:51000");
        assert_eq!(
            local_addresses_to_try(&same, id, Some("k7qm 3xpa wz")),
            [SocketAddr::V4(local)]
        );
        assert!(
            local_addresses_to_try(&same, id, Some("K7QM-3XPA-WY")).is_empty(),
            "a wrong code opens nothing"
        );
        assert!(local_addresses_to_try(&same, id, None).is_empty());
        let elsewhere = introduction("198.51.100.7:51000");
        assert!(
            local_addresses_to_try(&elsewhere, id, Some("K7QM-3XPA-WZ")).is_empty(),
            "another network: its local addresses are out of reach"
        );

        let advice = same_network_no_reply(id, same.peer.ip(), &[SocketAddr::V4(local)]);
        assert!(advice.contains("192.168.1.20:47800"), "{advice}");
        assert!(advice.contains("did not answer either"), "{advice}");
        let plain = same_network_no_reply(id, same.peer.ip(), &[]);
        assert!(!plain.contains("either"), "{plain}");
        for text in [advice, plain] {
            assert!(text.contains("same internet address"), "{text}");
            assert!(!text.contains("rendezvous"), "{text}");
        }
    }

    #[tokio::test]
    #[ignore = "needs a rendezvous service on this machine: TIDEDESK_TEST_SERVICE=ip:port"]
    async fn viewer_dialer_uses_sealed_local_addresses_on_the_same_network() {
        use tidedesk_core::nat::AgentStatus;
        use tidedesk_core::nat::signal::{Credentials, RendezvousStatus};

        let service = external_service();
        let dir = temp_dir("dialer-local-address");
        let identity = HostIdentity::load_or_create(&dir).unwrap();
        // The host answers QUIC on one socket and registers from another, so
        // the address the service introduces leads nowhere: only the sealed
        // local address reaches the host.
        let (host_socket, _) = SharedSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let host_addr = host_socket.local_addr().unwrap();
        fake_host(net::server_endpoint_on(host_socket, &identity).unwrap());
        let (registrar, registrar_tap) =
            SharedSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let registrar_addr = registrar.local_addr().unwrap();
        // quinn reads the socket; without an endpoint nothing reaches the agent.
        let _registrar_endpoint = net::client_endpoint_on(registrar.clone()).unwrap();
        let host_agent = Agent::spawn(registrar, registrar_tap).unwrap();
        let code = "K7QM-3XPA-WZ";
        let SocketAddr::V4(local) = host_addr else {
            unreachable!("bound to IPv4 loopback")
        };
        let credentials = Arc::new(Credentials {
            candidates: candidates::seal(code, identity.device_id(), &[local]),
            ..(*identity.rendezvous_credentials()).clone()
        });
        host_agent.start_rendezvous(service.to_string(), credentials);
        let registered =
            |s: &AgentStatus| matches!(s.rendezvous, RendezvousStatus::Registered { .. });
        tokio::time::timeout(
            Duration::from_secs(10),
            host_agent.status().wait_for(registered),
        )
        .await
        .expect("the host registers")
        .unwrap();

        let id = identity.device_id().to_string();
        let route = Route::Rendezvous {
            service: service.to_string(),
        };
        let dialer = Dialer::new(&id, &route, Some(code), |_| {})
            .await
            .unwrap()
            .with_config_dir(dir);
        let found = RouteInfo {
            kind: RouteKind::LocalNetwork,
            peer: host_addr,
            observed_self: None,
        };
        assert_eq!(dialer.route, found);
        let session = dialer
            .connect(&options(host_addr, route.clone()))
            .await
            .unwrap();
        assert_eq!(session.fingerprint, identity.fingerprint());
        session.conn.close(0u32.into(), b"done");

        // A wrong code opens nothing: the punched path to the registering
        // socket is used, as before sealed addresses.
        let wrong = Dialer::new(&id, &route, Some("K7QM-3XPA-WY"), |_| {})
            .await
            .unwrap();
        assert_eq!(wrong.route.kind, RouteKind::Rendezvous);
        assert_eq!(wrong.route.peer, registrar_addr);
    }
}
