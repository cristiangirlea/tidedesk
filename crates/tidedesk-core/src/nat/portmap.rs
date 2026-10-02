//! Asking the home router to forward this computer's QUIC port, so viewers
//! on other networks reach it even where hole punching fails. TideDesk never
//! relays: a port the router opened is the next-best direct path.
//!
//! PCP (RFC 6887) is asked first, then NAT-PMP (RFC 6886), its predecessor,
//! which many routers still speak; both on the router's UDP port
//! [`ROUTER_PORT`]. (UPnP is a separate piece.) A mapping lasts [`LIFETIME`],
//! is renewed at half of it while sharing runs and removed when sharing
//! stops, so a computer that crashes leaves at most one lifetime behind.
//!
//! The port asked for outside is the one used inside. On the many routers
//! that keep port numbers, the address the connection service already sees
//! is then the opened one, and any viewer that knows it gets in, even one
//! behind a router that defeats punching.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use tokio::net::UdpSocket;

use super::upnp::{self, Gateway};

/// The router's port for PCP and NAT-PMP.
pub const ROUTER_PORT: u16 = 5351;

/// How long a mapping is asked for: RFC 6886's recommendation.
pub const LIFETIME: Duration = Duration::from_secs(2 * 60 * 60);

/// How long to wait before asking again after the router did not open the
/// port: it may be switched on later, or the computer may move networks.
const RETRY: Duration = Duration::from_secs(5 * 60);

/// The first wait for an answer; each try waits twice the one before.
const FIRST_WAIT: Duration = Duration::from_millis(250);

/// How long to wait for a router, and where to search for UPnP ones.
#[derive(Debug, Clone, Copy)]
pub(crate) struct How {
    pub first_wait: Duration,
    pub ssdp: SocketAddr,
}

impl How {
    /// UPnP's search answers and HTTP exchanges take longer than one
    /// datagram's round trip.
    fn search_wait(self) -> Duration {
        self.first_wait * 8
    }

    fn http_wait(self) -> Duration {
        self.first_wait * 12
    }
}

const REAL: How = How {
    first_wait: FIRST_WAIT,
    ssdp: SocketAddr::V4(upnp::SSDP),
};
const TRIES: u32 = 3;

/// IANA protocol number of UDP, which QUIC runs over.
const UDP: u8 = 17;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Pcp,
    NatPmp,
    Upnp,
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pcp => "PCP",
            Self::NatPmp => "NAT-PMP",
            Self::Upnp => "UPnP",
        })
    }
}

/// A port the router forwards to this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    pub method: Method,
    /// The router's own internet address and the port it forwards.
    pub external: SocketAddrV4,
    pub internal_port: u16,
    /// How long the router keeps it without a renewal.
    pub lifetime: Duration,
    /// PCP's nonce: renewing or removing the mapping needs the same one.
    nonce: [u8; 12],
    /// Where a UPnP router is asked.
    gateway: Option<Gateway>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapError {
    NoRouter,
    /// None of PCP, NAT-PMP and UPnP answered.
    NoAnswer,
    /// The router answered with an error code.
    Refused {
        method: Method,
        code: u16,
    },
    Network(String),
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRouter => f.write_str("no router found on this network"),
            Self::NoAnswer => f.write_str(
                "no answer from the router (it may not support PCP, NAT-PMP or UPnP, or has \
                 them turned off)",
            ),
            Self::Refused { method, code } => {
                write!(
                    f,
                    "the router refused ({method}: {})",
                    meaning(*method, *code)
                )
            }
            Self::Network(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for MapError {}

/// What a router's error code means.
fn meaning(method: Method, code: u16) -> &'static str {
    match (method, code) {
        (Method::NatPmp, 2) | (Method::Pcp, 2) => "not allowed by its settings",
        (Method::NatPmp, 3) | (Method::Pcp, 7) => "its own internet connection is down",
        (Method::NatPmp, 4) | (Method::Pcp, 8) => "it has no ports left",
        (Method::Pcp, 11) => "it cannot give this computer an address outside",
        (Method::Pcp, 12) => "this computer's address does not match what it sees",
        (Method::Upnp, 718) => "the port is already forwarded to another computer",
        (Method::Upnp, 606) => "not allowed by its settings",
        _ => "unsupported request",
    }
}

/// Whether the router forwards the port, as the host's settings show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingStatus {
    /// Not asked: turned off.
    Off,
    Asking,
    /// The router forwards `external` to this computer.
    Open {
        method: Method,
        external: SocketAddrV4,
    },
    /// The router opened the port, but its own address, `outer`, is not on
    /// the internet: another router, or the internet provider's, sits in
    /// front of it, so viewers outside still cannot reach the port.
    BehindAnotherRouter {
        method: Method,
        outer: Ipv4Addr,
    },
    /// Not opened; the reason says why.
    Unavailable(String),
}

impl fmt::Display for MappingStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("The router is not asked to open a port."),
            Self::Asking => f.write_str("Asking the router to open TideDesk's port…"),
            Self::Open { method, external } => {
                write!(
                    f,
                    "The router forwards {external} to this computer ({method})."
                )
            }
            Self::BehindAnotherRouter { method, outer } => write!(
                f,
                "The router opened the port ({method}), but its address {outer} is not on \
                 the internet: another router, or the internet provider's, sits in front of it."
            ),
            Self::Unavailable(reason) => write!(f, "The router did not open the port: {reason}."),
        }
    }
}

/// This network's router (the default gateway), when there is one.
pub fn router() -> Option<Ipv4Addr> {
    platform_router()
}

#[cfg(windows)]
fn platform_router() -> Option<Ipv4Addr> {
    use windows::Win32::NetworkManagement::IpHelper::{GetBestRoute, MIB_IPFORWARDROW};
    // Any internet address: the route towards it goes through the router.
    let destination = u32::from_ne_bytes([8, 8, 8, 8]);
    let mut row = MIB_IPFORWARDROW::default();
    // SAFETY: GetBestRoute fills the row it is given.
    let error = unsafe { GetBestRoute(destination, None, &mut row) };
    let next_hop = Ipv4Addr::from(row.dwForwardNextHop.to_ne_bytes());
    (error == 0 && !next_hop.is_unspecified()).then_some(next_hop)
}

#[cfg(target_os = "linux")]
fn platform_router() -> Option<Ipv4Addr> {
    parse_proc_route(&std::fs::read_to_string("/proc/net/route").ok()?)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn platform_router() -> Option<Ipv4Addr> {
    None
}

/// The default gateway in Linux's `/proc/net/route`: the line for
/// destination 0 with the gateway flag, its address in host byte order.
#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
fn parse_proc_route(text: &str) -> Option<Ipv4Addr> {
    const RTF_GATEWAY: u32 = 0x2;
    text.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (destination, gateway, flags) = (fields.get(1)?, fields.get(2)?, fields.get(3)?);
        let flags = u32::from_str_radix(flags, 16).ok()?;
        if *destination != "00000000" || flags & RTF_GATEWAY == 0 {
            return None;
        }
        let gateway = u32::from_str_radix(gateway, 16).ok()?;
        Some(Ipv4Addr::from(gateway.to_le_bytes()))
    })
}

// NAT-PMP (RFC 6886): version 0; answers carry the opcode plus 128.

fn natpmp_address_request() -> [u8; 2] {
    [0, 0]
}

/// Maps UDP `internal` to `external` outside (0: the router chooses) for
/// `lifetime` seconds; a lifetime of 0 removes the mapping.
fn natpmp_map_request(internal: u16, external: u16, lifetime: u32) -> [u8; 12] {
    let mut request = [0u8; 12];
    request[1] = 1;
    request[4..6].copy_from_slice(&internal.to_be_bytes());
    request[6..8].copy_from_slice(&external.to_be_bytes());
    request[8..12].copy_from_slice(&lifetime.to_be_bytes());
    request
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NatPmpAnswer {
    Address(Ipv4Addr),
    Mapped {
        internal: u16,
        external: u16,
        lifetime: u32,
    },
}

/// The answer to the request with opcode `op`: the router's error code when
/// it refused; `None` for anything else.
fn parse_natpmp(data: &[u8], op: u8) -> Option<Result<NatPmpAnswer, u16>> {
    if data.len() < 4 || data[0] != 0 || data[1] != 128 + op {
        return None;
    }
    let code = u16::from_be_bytes([data[2], data[3]]);
    if code != 0 {
        return Some(Err(code));
    }
    match op {
        0 if data.len() >= 12 => Some(Ok(NatPmpAnswer::Address(Ipv4Addr::new(
            data[8], data[9], data[10], data[11],
        )))),
        1 if data.len() >= 16 => Some(Ok(NatPmpAnswer::Mapped {
            internal: u16::from_be_bytes([data[8], data[9]]),
            external: u16::from_be_bytes([data[10], data[11]]),
            lifetime: u32::from_be_bytes([data[12], data[13], data[14], data[15]]),
        })),
        _ => None,
    }
}

// PCP (RFC 6887): version 2, a 24-byte header and a 36-byte MAP opcode.

/// Asks for UDP `internal` to be forwarded from `suggested` outside (an
/// unspecified address or port leaves the choice to the router) for
/// `lifetime` seconds, from `client`, this computer's address towards the
/// router. A lifetime of 0 removes the mapping with the same `nonce`.
fn pcp_map_request(
    nonce: &[u8; 12],
    client: Ipv4Addr,
    internal: u16,
    suggested: SocketAddrV4,
    lifetime: u32,
) -> [u8; 60] {
    let mut request = [0u8; 60];
    request[0] = 2;
    request[1] = 1;
    request[4..8].copy_from_slice(&lifetime.to_be_bytes());
    request[8..24].copy_from_slice(&client.to_ipv6_mapped().octets());
    request[24..36].copy_from_slice(nonce);
    request[36] = UDP;
    request[40..42].copy_from_slice(&internal.to_be_bytes());
    request[42..44].copy_from_slice(&suggested.port().to_be_bytes());
    request[44..60].copy_from_slice(&suggested.ip().to_ipv6_mapped().octets());
    request
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PcpAnswer {
    Mapped {
        external: SocketAddrV4,
        lifetime: u32,
    },
    Error(u16),
    /// A NAT-PMP router: it answers a PCP request with version 0.
    UnsupportedVersion,
}

/// The answer to a MAP request with `nonce` for `internal`; `None` for
/// anything else.
fn parse_pcp(data: &[u8], nonce: &[u8; 12], internal: u16) -> Option<PcpAnswer> {
    if data.len() >= 2 && data[0] == 0 && data[1] >= 128 {
        return Some(PcpAnswer::UnsupportedVersion);
    }
    if data.len() < 60 || data[0] != 2 || data[1] != 0x81 {
        return None;
    }
    if &data[24..36] != nonce
        || data[36] != UDP
        || u16::from_be_bytes([data[40], data[41]]) != internal
    {
        return None;
    }
    let code = u16::from(data[3]);
    if code != 0 {
        return Some(PcpAnswer::Error(code));
    }
    let ip: [u8; 16] = data[44..60].try_into().ok()?;
    let ip = std::net::Ipv6Addr::from(ip).to_ipv4_mapped()?;
    Some(PcpAnswer::Mapped {
        external: SocketAddrV4::new(ip, u16::from_be_bytes([data[42], data[43]])),
        lifetime: u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
    })
}

/// Sends `request` until `answer` accepts what comes back, waiting
/// `first_wait`, then twice as long, [`TRIES`] times in all. `None` when
/// nothing acceptable came, or the router said nothing listens there.
async fn ask<T>(
    socket: &UdpSocket,
    request: &[u8],
    first_wait: Duration,
    mut answer: impl FnMut(&[u8]) -> Option<T>,
) -> Option<T> {
    let mut wait = first_wait;
    let mut buffer = [0u8; 1100];
    for _ in 0..TRIES {
        socket.send(request).await.ok()?;
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            match tokio::time::timeout_at(deadline, socket.recv(&mut buffer)).await {
                Ok(Ok(n)) => {
                    if let Some(found) = answer(&buffer[..n]) {
                        return Some(found);
                    }
                }
                // Windows reports a closed port on the router as an error.
                Ok(Err(_)) => return None,
                Err(_) => break,
            }
        }
        wait *= 2;
    }
    None
}

/// A socket towards `router`, and this computer's address on that path.
async fn towards(router: SocketAddr) -> Result<(UdpSocket, Ipv4Addr), MapError> {
    let network = |e: std::io::Error| MapError::Network(format!("asking the router: {e}"));
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .await
        .map_err(network)?;
    socket.connect(router).await.map_err(network)?;
    match socket.local_addr().map_err(network)? {
        SocketAddr::V4(local) => Ok((socket, *local.ip())),
        SocketAddr::V6(_) => Err(MapError::Network("no IPv4 path to the router".into())),
    }
}

/// Asks `router` to forward UDP `internal_port` for `lifetime`: PCP first,
/// then NAT-PMP. Passing the `earlier` mapping renews it.
pub async fn map(
    router: SocketAddr,
    internal_port: u16,
    lifetime: Duration,
    earlier: Option<&Mapping>,
) -> Result<Mapping, MapError> {
    map_with(router, internal_port, lifetime, earlier, REAL).await
}

async fn map_with(
    router: SocketAddr,
    internal_port: u16,
    lifetime: Duration,
    earlier: Option<&Mapping>,
    how: How,
) -> Result<Mapping, MapError> {
    let (socket, client) = towards(router).await?;
    let seconds = u32::try_from(lifetime.as_secs()).unwrap_or(u32::MAX);
    if let Some(gateway) = earlier.and_then(|m| m.gateway.clone()) {
        return upnp_map(gateway, client, internal_port, lifetime, how).await;
    }
    if earlier.is_none_or(|m| m.method == Method::Pcp) {
        let nonce = earlier.map_or_else(super::random_bytes, |m| m.nonce);
        let suggested = earlier.map_or(
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, internal_port),
            |m| m.external,
        );
        let request = pcp_map_request(&nonce, client, internal_port, suggested, seconds);
        match ask(&socket, &request, how.first_wait, |d| {
            parse_pcp(d, &nonce, internal_port)
        })
        .await
        {
            Some(PcpAnswer::Mapped { external, lifetime }) => {
                return Ok(Mapping {
                    method: Method::Pcp,
                    external,
                    internal_port,
                    lifetime: Duration::from_secs(lifetime.into()),
                    nonce,
                    gateway: None,
                });
            }
            Some(PcpAnswer::Error(code)) => {
                return Err(MapError::Refused {
                    method: Method::Pcp,
                    code,
                });
            }
            Some(PcpAnswer::UnsupportedVersion) | None => {}
        }
    }
    match natpmp_map(&socket, internal_port, seconds, earlier, how).await {
        Err(MapError::NoAnswer) if earlier.is_none() => {}
        done => return done,
    }
    let SocketAddr::V4(router) = router else {
        return Err(MapError::NoAnswer);
    };
    match upnp::discover(*router.ip(), how.ssdp, how.search_wait()).await {
        Some(gateway) => upnp_map(gateway, client, internal_port, lifetime, how).await,
        None => Err(MapError::NoAnswer),
    }
}

/// Asks a UPnP router at `gateway` to forward `internal_port` to `client`.
async fn upnp_map(
    gateway: Gateway,
    client: Ipv4Addr,
    internal_port: u16,
    lifetime: Duration,
    how: How,
) -> Result<Mapping, MapError> {
    let seconds = u32::try_from(lifetime.as_secs()).unwrap_or(u32::MAX);
    let (external, lease) =
        upnp::add(&gateway, client, internal_port, seconds, how.http_wait()).await?;
    Ok(Mapping {
        method: Method::Upnp,
        external,
        internal_port,
        // A router that keeps mappings until removed is renewed as often.
        lifetime: if lease == 0 {
            lifetime
        } else {
            Duration::from_secs(lease.into())
        },
        nonce: [0; 12],
        gateway: Some(gateway),
    })
}

/// NAT-PMP: the router's own address first, then the mapping.
async fn natpmp_map(
    socket: &UdpSocket,
    internal_port: u16,
    seconds: u32,
    earlier: Option<&Mapping>,
    how: How,
) -> Result<Mapping, MapError> {
    let refused = |code| MapError::Refused {
        method: Method::NatPmp,
        code,
    };
    let outer = match ask(socket, &natpmp_address_request(), how.first_wait, |d| {
        parse_natpmp(d, 0)
    })
    .await
    {
        Some(Ok(NatPmpAnswer::Address(ip))) => ip,
        Some(Err(code)) => return Err(refused(code)),
        Some(Ok(_)) | None => return Err(MapError::NoAnswer),
    };
    let suggested = earlier.map_or(internal_port, |m| m.external.port());
    let request = natpmp_map_request(internal_port, suggested, seconds);
    match ask(socket, &request, how.first_wait, |d| parse_natpmp(d, 1)).await {
        Some(Ok(NatPmpAnswer::Mapped {
            internal,
            external,
            lifetime,
        })) if internal == internal_port => Ok(Mapping {
            method: Method::NatPmp,
            external: SocketAddrV4::new(outer, external),
            internal_port,
            lifetime: Duration::from_secs(lifetime.into()),
            nonce: [0; 12],
            gateway: None,
        }),
        Some(Err(code)) => Err(refused(code)),
        _ => Err(MapError::NoAnswer),
    }
}

/// Asks `router` to remove `mapping`.
pub async fn unmap(router: SocketAddr, mapping: &Mapping) -> Result<(), MapError> {
    unmap_with(router, mapping, REAL).await
}

async fn unmap_with(router: SocketAddr, mapping: &Mapping, how: How) -> Result<(), MapError> {
    let port = mapping.internal_port;
    if let Some(gateway) = &mapping.gateway {
        return upnp::delete(gateway, port, how.http_wait()).await;
    }
    let (socket, client) = towards(router).await?;
    let answered = match mapping.method {
        Method::Pcp => {
            let request = pcp_map_request(&mapping.nonce, client, port, mapping.external, 0);
            match ask(&socket, &request, how.first_wait, |d| {
                parse_pcp(d, &mapping.nonce, port)
            })
            .await
            {
                Some(PcpAnswer::Mapped { .. }) => Ok(()),
                Some(PcpAnswer::Error(code)) => Err(code),
                _ => return Err(MapError::NoAnswer),
            }
        }
        Method::NatPmp => {
            let request = natpmp_map_request(port, 0, 0);
            match ask(&socket, &request, how.first_wait, |d| parse_natpmp(d, 1)).await {
                Some(Ok(_)) => Ok(()),
                Some(Err(code)) => Err(code),
                None => return Err(MapError::NoAnswer),
            }
        }
        Method::Upnp => return Err(MapError::NoAnswer),
    };
    answered.map_err(|code| MapError::Refused {
        method: mapping.method,
        code,
    })
}

/// The status a mapping gives.
fn status_of(mapping: &Mapping) -> MappingStatus {
    let outer = *mapping.external.ip();
    if super::check_public(SocketAddr::V4(mapping.external)).is_ok() {
        MappingStatus::Open {
            method: mapping.method,
            external: mapping.external,
        }
    } else {
        MappingStatus::BehindAnotherRouter {
            method: mapping.method,
            outer,
        }
    }
}

/// Keeps `internal_port` forwarded until `stop` changes or closes: asks the
/// router `find` names, renews at half the lifetime, asks again
/// [`RETRY`] after a failure, and removes the mapping at the end. Every
/// change goes to `report`.
pub(crate) async fn keep(
    internal_port: u16,
    find: impl Fn() -> Option<SocketAddr>,
    report: impl Fn(MappingStatus),
    mut stop: tokio::sync::watch::Receiver<()>,
    how: How,
) {
    let mut current: Option<(SocketAddr, Mapping)> = None;
    loop {
        let wait = match find() {
            None => {
                current = None;
                report(MappingStatus::Unavailable(MapError::NoRouter.to_string()));
                RETRY
            }
            Some(router) => {
                if current.is_none() {
                    report(MappingStatus::Asking);
                }
                // A mapping on another router (the computer moved) is left
                // to expire.
                let earlier = current
                    .as_ref()
                    .filter(|(at, _)| *at == router)
                    .map(|(_, m)| m);
                match map_with(router, internal_port, LIFETIME, earlier, how).await {
                    Ok(mapping) => {
                        report(status_of(&mapping));
                        let renew = (mapping.lifetime / 2).max(Duration::from_secs(30));
                        current = Some((router, mapping));
                        renew
                    }
                    Err(e) => {
                        current = None;
                        report(MappingStatus::Unavailable(e.to_string()));
                        RETRY
                    }
                }
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = stop.changed() => break,
        }
    }
    if let Some((router, mapping)) = current {
        // Best effort: the router forgets it after its lifetime anyway.
        let _ = unmap_with(router, &mapping, how).await;
    }
    report(MappingStatus::Off);
}

/// [`keep`] with the real router and timings.
pub(crate) async fn keep_on_this_network(
    internal_port: u16,
    report: impl Fn(MappingStatus),
    stop: tokio::sync::watch::Receiver<()>,
) {
    let find = || router().map(|ip| SocketAddr::from((ip, ROUTER_PORT)));
    keep(internal_port, find, report, stop, REAL).await;
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Searches go to a closed port: no test ever searches the real network.
    const SHORT: How = How {
        first_wait: Duration::from_millis(20),
        ssdp: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9)),
    };

    #[test]
    fn nat_pmp_requests_and_answers_follow_rfc_6886() {
        assert_eq!(natpmp_address_request(), [0, 0]);
        // 47800 = 0xBAB8, 7200 s = 0x1C20.
        assert_eq!(
            natpmp_map_request(47800, 47800, 7200),
            [0, 1, 0, 0, 0xBA, 0xB8, 0xBA, 0xB8, 0, 0, 0x1C, 0x20]
        );
        let address = [0, 128, 0, 0, 0, 0, 0, 9, 203, 0, 113, 7];
        assert_eq!(
            parse_natpmp(&address, 0),
            Some(Ok(NatPmpAnswer::Address(Ipv4Addr::new(203, 0, 113, 7))))
        );
        let mapped = [
            0, 129, 0, 0, 0, 0, 0, 9, 0xBA, 0xB8, 0xBA, 0xB9, 0, 0, 0x1C, 0x20,
        ];
        assert_eq!(
            parse_natpmp(&mapped, 1),
            Some(Ok(NatPmpAnswer::Mapped {
                internal: 47800,
                external: 47801,
                lifetime: 7200
            }))
        );
        assert_eq!(parse_natpmp(&[0, 129, 0, 2], 1), Some(Err(2)), "refused");
        assert_eq!(
            parse_natpmp(&mapped, 0),
            None,
            "an answer to another request"
        );
        assert_eq!(parse_natpmp(&address[..8], 0), None, "too short");
    }

    #[test]
    fn pcp_requests_and_answers_follow_rfc_6887() {
        let nonce = [7u8; 12];
        let client = Ipv4Addr::new(192, 168, 1, 50);
        let any = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 47800);
        let request = pcp_map_request(&nonce, client, 47800, any, 7200);
        assert_eq!(request[..4], [2, 1, 0, 0], "version 2, MAP");
        assert_eq!(request[4..8], 7200u32.to_be_bytes());
        assert_eq!(request[8..24], client.to_ipv6_mapped().octets());
        assert_eq!(request[24..36], nonce);
        assert_eq!(request[36], 17, "UDP");
        assert_eq!(request[40..44], [0xBA, 0xB8, 0xBA, 0xB8]);
        assert_eq!(
            request[44..60],
            Ipv4Addr::UNSPECIFIED.to_ipv6_mapped().octets()
        );

        let mut answer = request;
        answer[1] = 0x81;
        answer[42..44].copy_from_slice(&47801u16.to_be_bytes());
        answer[44..60].copy_from_slice(&Ipv4Addr::new(203, 0, 113, 7).to_ipv6_mapped().octets());
        assert_eq!(
            parse_pcp(&answer, &nonce, 47800),
            Some(PcpAnswer::Mapped {
                external: "203.0.113.7:47801".parse().unwrap(),
                lifetime: 7200
            })
        );
        assert_eq!(parse_pcp(&answer, &[8u8; 12], 47800), None, "another nonce");
        assert_eq!(parse_pcp(&answer, &nonce, 47801), None, "another port");
        answer[3] = 8;
        assert_eq!(parse_pcp(&answer, &nonce, 47800), Some(PcpAnswer::Error(8)));
        // What a NAT-PMP router answers to PCP (seen on a real one).
        assert_eq!(
            parse_pcp(&[0, 129, 0, 1], &nonce, 47800),
            Some(PcpAnswer::UnsupportedVersion)
        );
    }

    #[test]
    fn linux_names_the_default_gateway_in_its_route_table() {
        let table = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\n\
                     eth0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\n\
                     eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\n";
        assert_eq!(parse_proc_route(table), Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(parse_proc_route("Iface\tDestination\n"), None);
    }

    /// How the fake router behaves.
    #[derive(Clone, Copy)]
    enum Kind {
        /// Like the real one these were checked against: NAT-PMP only.
        NatPmp(Ipv4Addr),
        Pcp(Ipv4Addr),
        /// Answers NAT-PMP with "not authorized".
        Refuses,
        Silent,
    }

    pub(super) type Seen = Arc<Mutex<Vec<Vec<u8>>>>;

    /// A router on the loopback that answers as `kind` and keeps what it
    /// was asked.
    async fn fake_router(kind: Kind) -> (SocketAddr, Seen) {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = socket.local_addr().unwrap();
        let seen = Seen::default();
        let log = seen.clone();
        tokio::spawn(async move {
            let mut buffer = [0u8; 1100];
            while let Ok((n, from)) = socket.recv_from(&mut buffer).await {
                let request = buffer[..n].to_vec();
                log.lock().unwrap().push(request.clone());
                if let Some(answer) = answer(kind, &request) {
                    let _ = socket.send_to(&answer, from).await;
                }
            }
        });
        (address, seen)
    }

    fn answer(kind: Kind, request: &[u8]) -> Option<Vec<u8>> {
        let outer = match kind {
            Kind::Silent => return None,
            Kind::Refuses => {
                let mut answer = vec![0, 128 + request[1], 0, 2];
                answer.resize(16, 0);
                return Some(answer);
            }
            Kind::NatPmp(outer) | Kind::Pcp(outer) => outer,
        };
        match (kind, request[0], request.get(1)) {
            (Kind::Pcp(_), 2, _) => {
                let mut answer = request.to_vec();
                answer[1] = 0x81;
                answer[8..24].fill(0);
                if answer[42..44] == [0, 0] {
                    answer[42..44].copy_from_slice(&request[40..42]);
                }
                answer[44..60].copy_from_slice(&outer.to_ipv6_mapped().octets());
                Some(answer)
            }
            (_, 2, _) => Some(vec![0, 129, 0, 1]),
            (_, 0, Some(0)) => {
                let mut answer = vec![0, 128, 0, 0, 0, 0, 0, 1];
                answer.extend_from_slice(&outer.octets());
                Some(answer)
            }
            (_, 0, Some(1)) => {
                let mut answer = vec![0, 129, 0, 0, 0, 0, 0, 1];
                answer.extend_from_slice(&request[4..6]);
                let asked = &request[6..8];
                let lifetime = &request[8..12];
                answer.extend_from_slice(if asked == [0, 0] && lifetime != [0; 4] {
                    &request[4..6]
                } else {
                    asked
                });
                answer.extend_from_slice(lifetime);
                Some(answer)
            }
            _ => None,
        }
    }

    const OUTER: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 7);

    /// A UPnP router on the loopback: answers searches on UDP, serves its
    /// description and its SOAP control over HTTP, and keeps each SOAP
    /// call it got. With `permanent_only`, it refuses leases as some do.
    pub(in crate::nat) async fn fake_upnp_router(permanent_only: bool) -> (SocketAddr, Seen) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_port = http.local_addr().unwrap().port();
        let search = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let search_at = search.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buffer = [0u8; 2048];
            while let Ok((_, from)) = search.recv_from(&mut buffer).await {
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\
                     LOCATION: http://127.0.0.1:{http_port}/rootDesc.xml\r\n\r\n"
                );
                let _ = search.send_to(answer.as_bytes(), from).await;
            }
        });
        let seen = Seen::default();
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = http.accept().await {
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                // The head, then as much body as it announces.
                loop {
                    let n = stream.read(&mut buffer).await.unwrap_or(0);
                    request.extend_from_slice(&buffer[..n]);
                    let text = String::from_utf8_lossy(&request).to_string();
                    let Some((head, body)) = text.split_once("\r\n\r\n") else {
                        if n == 0 {
                            break;
                        }
                        continue;
                    };
                    let length = upnp::tests::header_value(head, "CONTENT-LENGTH")
                        .and_then(|l| l.parse::<usize>().ok())
                        .unwrap_or(0);
                    if body.len() >= length || n == 0 {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&request).to_string();
                let (status, body) = if text.starts_with("GET ") {
                    (200, DESCRIPTION.to_string())
                } else {
                    let action = text
                        .split('#')
                        .nth(1)
                        .and_then(|a| a.split('"').next())
                        .unwrap_or("");
                    log.lock().unwrap().push(text.clone().into_bytes());
                    match action {
                        "GetExternalIPAddress" => (
                            200,
                            format!("<NewExternalIPAddress>{OUTER}</NewExternalIPAddress>"),
                        ),
                        "AddPortMapping"
                            if permanent_only
                                && !text.contains("<NewLeaseDuration>0</NewLeaseDuration>") =>
                        {
                            (
                                500,
                                "<UPnPError><errorCode>725</errorCode></UPnPError>".into(),
                            )
                        }
                        _ => (200, format!("<u:{action}Response/>")),
                    }
                };
                let answer = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(answer.as_bytes()).await;
            }
        });
        (search_at, seen)
    }

    const DESCRIPTION: &str = "<root><device><serviceList><service>\
        <serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>\
        <controlURL>/ctl/IPConn</controlURL></service></serviceList></device></root>";

    /// The SOAP actions a fake UPnP router was asked, in order.
    fn actions(seen: &Seen) -> Vec<String> {
        seen.lock()
            .unwrap()
            .iter()
            .map(|r| {
                let text = String::from_utf8_lossy(r).to_string();
                text.split('#')
                    .nth(1)
                    .unwrap()
                    .split('"')
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    #[tokio::test]
    async fn a_upnp_router_is_asked_when_pcp_and_nat_pmp_say_nothing() {
        let (router, _) = fake_router(Kind::Silent).await;
        let (ssdp, seen) = fake_upnp_router(false).await;
        let how = How { ssdp, ..SHORT };
        let mapping = map_with(router, 47800, LIFETIME, None, how).await.unwrap();
        assert_eq!(mapping.method, Method::Upnp);
        assert_eq!(mapping.external, SocketAddrV4::new(OUTER, 47800));
        assert_eq!(mapping.lifetime, LIFETIME);
        let add = seen.lock().unwrap()[1].clone();
        let add = String::from_utf8(add).unwrap();
        assert!(add.contains("<NewProtocol>UDP</NewProtocol>"));
        assert!(add.contains("<NewInternalClient>127.0.0.1</NewInternalClient>"));
        assert!(add.contains("<NewLeaseDuration>7200</NewLeaseDuration>"));

        let renewed = map_with(router, 47800, LIFETIME, Some(&mapping), how)
            .await
            .unwrap();
        assert_eq!(renewed, mapping);
        unmap_with(router, &mapping, how).await.unwrap();
        assert_eq!(
            actions(&seen),
            [
                "GetExternalIPAddress",
                "AddPortMapping",
                "GetExternalIPAddress",
                "AddPortMapping",
                "DeletePortMapping"
            ]
        );
    }

    #[tokio::test]
    async fn a_upnp_router_without_leases_keeps_the_mapping_until_removed() {
        let (router, _) = fake_router(Kind::Silent).await;
        let (ssdp, seen) = fake_upnp_router(true).await;
        let how = How { ssdp, ..SHORT };
        let mapping = map_with(router, 47800, LIFETIME, None, how).await.unwrap();
        assert_eq!(mapping.method, Method::Upnp);
        assert_eq!(
            mapping.lifetime, LIFETIME,
            "renewed as often as a leased one"
        );
        let calls = seen.lock().unwrap().clone();
        let last = String::from_utf8(calls.last().unwrap().clone()).unwrap();
        assert!(last.contains("<NewLeaseDuration>0</NewLeaseDuration>"));
    }

    #[tokio::test]
    async fn a_nat_pmp_router_opens_renews_and_closes_the_port() {
        let (router, seen) = fake_router(Kind::NatPmp(OUTER)).await;
        let mapping = map_with(router, 47800, LIFETIME, None, SHORT)
            .await
            .unwrap();
        assert_eq!(mapping.method, Method::NatPmp);
        assert_eq!(mapping.external, SocketAddrV4::new(OUTER, 47800));
        assert_eq!(mapping.lifetime, LIFETIME);
        // PCP first, then NAT-PMP's address and mapping requests.
        let firsts: Vec<u8> = seen.lock().unwrap().iter().map(|r| r[0]).collect();
        assert_eq!(firsts, [2, 0, 0]);

        let renewed = map_with(router, 47800, LIFETIME, Some(&mapping), SHORT)
            .await
            .unwrap();
        assert_eq!(renewed, mapping);
        let pcp_asks = seen.lock().unwrap().iter().filter(|r| r[0] == 2).count();
        assert_eq!(pcp_asks, 1, "a renewal asks the router the way that worked");

        unmap_with(router, &mapping, SHORT).await.unwrap();
        let last = seen.lock().unwrap().last().unwrap().clone();
        assert_eq!(last, natpmp_map_request(47800, 0, 0));
    }

    #[tokio::test]
    async fn a_pcp_router_is_asked_first_and_renewed_with_the_same_nonce() {
        let (router, seen) = fake_router(Kind::Pcp(OUTER)).await;
        let mapping = map_with(router, 47800, LIFETIME, None, SHORT)
            .await
            .unwrap();
        assert_eq!(mapping.method, Method::Pcp);
        assert_eq!(mapping.external, SocketAddrV4::new(OUTER, 47800));
        map_with(router, 47800, LIFETIME, Some(&mapping), SHORT)
            .await
            .unwrap();
        unmap_with(router, &mapping, SHORT).await.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3, "no NAT-PMP when PCP answers");
        assert!(seen.iter().all(|r| r[24..36] == mapping.nonce));
        assert_eq!(seen[2][4..8], [0; 4], "removed with a lifetime of 0");
    }

    #[tokio::test]
    async fn a_silent_or_refusing_router_says_so() {
        let (silent, _) = fake_router(Kind::Silent).await;
        assert_eq!(
            map_with(silent, 47800, LIFETIME, None, SHORT).await,
            Err(MapError::NoAnswer)
        );
        let (refusing, _) = fake_router(Kind::Refuses).await;
        let refused = map_with(refusing, 47800, LIFETIME, None, SHORT).await;
        assert_eq!(
            refused,
            Err(MapError::Refused {
                method: Method::NatPmp,
                code: 2
            })
        );
        assert_eq!(
            refused.unwrap_err().to_string(),
            "the router refused (NAT-PMP: not allowed by its settings)"
        );
    }

    async fn keep_until(
        kind: Kind,
        wanted: fn(&MappingStatus) -> bool,
    ) -> (Vec<MappingStatus>, Seen) {
        let (router, seen) = fake_router(kind).await;
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let (stop, stopped) = tokio::sync::watch::channel(());
        let log = statuses.clone();
        let task = tokio::spawn(keep(
            47800,
            move || Some(router),
            move |s| log.lock().unwrap().push(s),
            stopped,
            SHORT,
        ));
        for _ in 0..200 {
            if statuses.lock().unwrap().last().is_some_and(wanted) {
                break;
            }
            tokio::time::sleep(SHORT.first_wait).await;
        }
        stop.send(()).unwrap();
        task.await.unwrap();
        let statuses = statuses.lock().unwrap().clone();
        (statuses, seen)
    }

    #[tokio::test]
    async fn sharing_keeps_the_port_open_and_closes_it_at_the_end() {
        let (statuses, seen) = keep_until(Kind::NatPmp(OUTER), |s| {
            matches!(s, MappingStatus::Open { .. })
        })
        .await;
        assert_eq!(
            statuses,
            [
                MappingStatus::Asking,
                MappingStatus::Open {
                    method: Method::NatPmp,
                    external: SocketAddrV4::new(OUTER, 47800)
                },
                MappingStatus::Off
            ]
        );
        let last = seen.lock().unwrap().last().unwrap().clone();
        assert_eq!(
            last,
            natpmp_map_request(47800, 0, 0),
            "removed when stopped"
        );
    }

    #[tokio::test]
    async fn a_router_behind_another_router_is_named() {
        let carrier = Ipv4Addr::new(100, 64, 12, 34);
        let (statuses, _) = keep_until(Kind::NatPmp(carrier), |s| {
            matches!(s, MappingStatus::BehindAnotherRouter { .. })
        })
        .await;
        assert_eq!(
            statuses[1],
            MappingStatus::BehindAnotherRouter {
                method: Method::NatPmp,
                outer: carrier
            }
        );
        let (statuses, _) =
            keep_until(Kind::Silent, |s| matches!(s, MappingStatus::Unavailable(_))).await;
        assert!(
            statuses[1]
                .to_string()
                .starts_with("The router did not open the port: no answer")
        );
    }

    /// Opens a port on this network's real router for a minute and closes
    /// it again; prints nothing that names the network.
    #[tokio::test]
    #[ignore = "asks the real router"]
    async fn this_networks_router_opens_and_closes_a_port() {
        let router = SocketAddr::from((router().expect("a router"), ROUTER_PORT));
        let throwaway = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        let port = throwaway.local_addr().unwrap().port();
        let mapping = map(router, port, Duration::from_secs(60), None)
            .await
            .expect("the router opens the port");
        println!(
            "{} opened the port, {} outside; the router's address is {}",
            mapping.method,
            if mapping.external.port() == port {
                "the same port"
            } else {
                "another port"
            },
            if matches!(status_of(&mapping), MappingStatus::Open { .. }) {
                "on the internet"
            } else {
                "not on the internet"
            }
        );
        unmap(router, &mapping).await.expect("the router closes it");
    }
}
