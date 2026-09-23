//! A minimal STUN client (RFC 5389 Binding only).
//!
//! A STUN server replies with the address it saw a request come from: for a
//! computer behind a router, that is the router's public address and the port
//! it mapped for this socket. Asking two servers also tells how the router
//! maps: the same public port for both means other computers can be reached
//! from it by hole punching; different ports ("symmetric NAT") mean they
//! cannot.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};

use ring::rand::{SecureRandom, SystemRandom};

use super::STUN_MAGIC_COOKIE;

/// Public servers asked by default. Each sees only this computer's public
/// address and the tiny request.
pub const DEFAULT_STUN_SERVERS: &[&str] = &["stun.l.google.com:19302", "stun.cloudflare.com:3478"];

/// How often to ask again, which also keeps the router's mapping alive:
/// cheap routers drop idle UDP mappings after as little as 30 seconds.
pub const STUN_REFRESH: Duration = Duration::from_secs(25);

/// Port used when a configured server has none.
pub const DEFAULT_STUN_PORT: u16 = 3478;

/// When to (re)send a request, measured from the start of discovery.
const RETRANSMIT_AT: [Duration; 4] = [
    Duration::ZERO,
    Duration::from_millis(500),
    Duration::from_millis(1500),
    Duration::from_millis(3500),
];

/// When to give up on servers that have not answered.
const GIVE_UP_AFTER: Duration = Duration::from_millis(7500);

const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const BINDING_ERROR: u16 = 0x0111;
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_ERROR_CODE: u16 = 0x0009;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;

pub type TransactionId = [u8; 12];

/// Whether a datagram has the shape of a STUN message.
pub fn is_stun(datagram: &[u8]) -> bool {
    datagram.len() >= 20 && datagram[0] & 0xC0 == 0 && datagram[4..8] == STUN_MAGIC_COOKIE
}

pub fn encode_binding_request(id: &TransactionId) -> [u8; 20] {
    let mut request = [0u8; 20];
    request[..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    request[4..8].copy_from_slice(&STUN_MAGIC_COOKIE);
    request[8..].copy_from_slice(id);
    request
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StunError {
    Malformed,
    ErrorResponse { code: u16, reason: String },
    NoAddress,
}

impl fmt::Display for StunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => write!(f, "malformed STUN response"),
            Self::ErrorResponse { code, reason } => write!(f, "STUN error {code} {reason}"),
            Self::NoAddress => write!(f, "STUN response without an address"),
        }
    }
}

impl std::error::Error for StunError {}

/// Decodes a Binding success response into its transaction ID and the
/// address the server saw (XOR-MAPPED-ADDRESS, else MAPPED-ADDRESS).
pub fn decode_binding_response(datagram: &[u8]) -> Result<(TransactionId, SocketAddr), StunError> {
    if !is_stun(datagram) {
        return Err(StunError::Malformed);
    }
    let kind = u16::from_be_bytes([datagram[0], datagram[1]]);
    let length = u16::from_be_bytes([datagram[2], datagram[3]]) as usize;
    if !length.is_multiple_of(4) || 20 + length > datagram.len() {
        return Err(StunError::Malformed);
    }
    if kind != BINDING_SUCCESS && kind != BINDING_ERROR {
        return Err(StunError::Malformed);
    }
    let id: TransactionId = datagram[8..20].try_into().expect("12 bytes");

    let (mut xor_mapped, mut mapped, mut error) = (None, None, None);
    let mut attributes = &datagram[20..20 + length];
    while attributes.len() >= 4 {
        let kind = u16::from_be_bytes([attributes[0], attributes[1]]);
        let len = u16::from_be_bytes([attributes[2], attributes[3]]) as usize;
        let Some(value) = attributes.get(4..4 + len) else {
            return Err(StunError::Malformed);
        };
        match kind {
            ATTR_XOR_MAPPED_ADDRESS => xor_mapped = parse_address(value, Some(&id)),
            ATTR_MAPPED_ADDRESS => mapped = parse_address(value, None),
            ATTR_ERROR_CODE if len >= 4 => {
                let code = u16::from(value[2] & 0x07) * 100 + u16::from(value[3]);
                error = Some((code, String::from_utf8_lossy(&value[4..]).into_owned()));
            }
            _ => {} // comprehension-optional or irrelevant: skip
        }
        let padded = (4 + len.next_multiple_of(4)).min(attributes.len());
        attributes = &attributes[padded..];
    }

    if kind == BINDING_ERROR {
        let (code, reason) = error.unwrap_or_default();
        return Err(StunError::ErrorResponse { code, reason });
    }
    xor_mapped
        .or(mapped)
        .map(|addr| (id, addr))
        .ok_or(StunError::NoAddress)
}

/// Parses a (XOR-)MAPPED-ADDRESS value. `xor_id` is the transaction ID for
/// the XOR form, whose port and address are masked with the cookie (and, for
/// IPv6, the transaction ID).
fn parse_address(value: &[u8], xor_id: Option<&TransactionId>) -> Option<SocketAddr> {
    let family = *value.get(1)?;
    let mut port = u16::from_be_bytes([*value.get(2)?, *value.get(3)?]);
    let mut mask = [0u8; 16];
    if let Some(id) = xor_id {
        mask[..4].copy_from_slice(&STUN_MAGIC_COOKIE);
        mask[4..].copy_from_slice(id);
        port ^= u16::from_be_bytes([mask[0], mask[1]]);
    }
    let unmask = |bytes: &[u8]| {
        bytes
            .iter()
            .zip(mask)
            .map(|(b, m)| b ^ m)
            .collect::<Vec<_>>()
    };
    let ip: IpAddr = match family {
        0x01 => {
            let octets: [u8; 4] = unmask(value.get(4..8)?).try_into().ok()?;
            Ipv4Addr::from(octets).into()
        }
        0x02 => {
            let octets: [u8; 16] = unmask(value.get(4..20)?).try_into().ok()?;
            Ipv6Addr::from(octets).into()
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// How the router in front of this computer maps outgoing traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NatKind {
    /// Only one server answered, so the mapping could not be compared.
    Unknown,
    /// Every server saw the same public address and port: hole punching can work.
    EndpointIndependent,
    /// Servers saw different ports: a new mapping per destination, so the
    /// address shown to one peer is useless to another. Punching cannot work.
    Symmetric,
}

impl fmt::Display for NatKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unknown => "unknown",
            Self::EndpointIndependent => "endpoint-independent",
            Self::Symmetric => "symmetric",
        })
    }
}

/// This computer's address as seen from the internet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicEndpoint {
    pub addr: SocketAddr,
    pub nat: NatKind,
    /// Name of the server whose answer supplied `addr`.
    pub via: String,
}

/// One round of asking every configured server, with retransmissions.
///
/// Pure state machine: the caller sends what [`Discovery::poll`] returns,
/// feeds replies to [`Discovery::on_datagram`] and sleeps until
/// [`Discovery::next_deadline`]. Time is passed in, so tests control it.
pub struct Discovery {
    started: Instant,
    servers: Vec<Query>,
    /// Index of the server that answered first; its answer is the address.
    first: Option<usize>,
    expired: bool,
}

struct Query {
    name: String,
    addr: SocketAddr,
    id: TransactionId,
    /// How many of the RETRANSMIT_AT slots have been used.
    sent: usize,
    answer: Option<SocketAddr>,
    failed: Option<String>,
}

impl Query {
    fn pending(&self) -> bool {
        self.answer.is_none() && self.failed.is_none()
    }
}

impl Discovery {
    /// Servers with the same address are asked once: answers from one
    /// destination always agree, so they could not reveal a symmetric NAT.
    pub fn new(mut servers: Vec<(String, SocketAddr)>, now: Instant) -> Self {
        let mut seen = Vec::new();
        servers.retain(|(_, addr)| {
            let new = !seen.contains(addr);
            seen.push(*addr);
            new
        });
        let rng = SystemRandom::new();
        let servers = servers
            .into_iter()
            .map(|(name, addr)| {
                let mut id = [0u8; 12];
                rng.fill(&mut id).expect("system RNG failed");
                Query {
                    name,
                    addr,
                    id,
                    sent: 0,
                    answer: None,
                    failed: None,
                }
            })
            .collect();
        Self {
            started: now,
            servers,
            first: None,
            expired: false,
        }
    }

    /// Requests due at `now`, as (server address, datagram) pairs.
    pub fn poll(&mut self, now: Instant) -> Vec<(SocketAddr, [u8; 20])> {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed >= GIVE_UP_AFTER {
            self.expired = true;
            return Vec::new();
        }
        let due = RETRANSMIT_AT.iter().filter(|at| **at <= elapsed).count();
        let mut requests = Vec::new();
        for query in self.servers.iter_mut().filter(|q| q.pending()) {
            // One request even if several slots passed since the last poll.
            if due > query.sent {
                query.sent = due;
                requests.push((query.addr, encode_binding_request(&query.id)));
            }
        }
        requests
    }

    /// Handles a datagram from the socket. Returns the updated public
    /// endpoint whenever a server's answer is accepted. Replies are matched
    /// by their random transaction ID, not by the sender's address.
    pub fn on_datagram(&mut self, datagram: &[u8], now: Instant) -> Option<PublicEndpoint> {
        if !is_stun(datagram) || now.saturating_duration_since(self.started) >= GIVE_UP_AFTER {
            return None;
        }
        let id = &datagram[8..20];
        let index = self
            .servers
            .iter()
            .position(|q| q.id == id && q.pending())?;
        match decode_binding_response(datagram) {
            Ok((_, addr)) => {
                self.servers[index].answer = Some(addr);
                self.first.get_or_insert(index);
                self.current()
            }
            Err(e) => {
                self.servers[index].failed = Some(e.to_string());
                None
            }
        }
    }

    /// When [`Discovery::poll`] next has work, or `None` once finished.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.expired || self.finished() {
            return None;
        }
        let next = self
            .servers
            .iter()
            .filter(|q| q.pending())
            .filter_map(|q| RETRANSMIT_AT.get(q.sent))
            .min()
            .copied()
            .unwrap_or(GIVE_UP_AFTER);
        Some(self.started + next)
    }

    /// `None` while still waiting; the result once every server answered or
    /// the time is up.
    pub fn outcome(&self, now: Instant) -> Option<Result<PublicEndpoint, String>> {
        let timed_out = now.saturating_duration_since(self.started) >= GIVE_UP_AFTER;
        if !timed_out && !self.finished() {
            return None;
        }
        Some(self.current().ok_or_else(|| {
            if self.servers.is_empty() {
                return "no STUN server could be resolved".to_string();
            }
            let servers: Vec<String> = self
                .servers
                .iter()
                .map(|q| match &q.failed {
                    Some(error) => format!("{} ({error})", q.name),
                    None => q.name.clone(),
                })
                .collect();
            format!("no reply from {}", servers.join(", "))
        }))
    }

    fn finished(&self) -> bool {
        self.servers.iter().all(|q| !q.pending())
    }

    fn current(&self) -> Option<PublicEndpoint> {
        let first = &self.servers[self.first?];
        let answers: Vec<SocketAddr> = self.servers.iter().filter_map(|q| q.answer).collect();
        let nat = match answers.as_slice() {
            [] | [_] => NatKind::Unknown,
            [a, rest @ ..] if rest.iter().all(|b| b == a) => NatKind::EndpointIndependent,
            _ => NatKind::Symmetric,
        };
        Some(PublicEndpoint {
            addr: first.answer?,
            nat,
            via: first.name.clone(),
        })
    }
}

/// Resolves configured `host[:port]` names to IPv4 addresses (TideDesk's
/// internet paths are IPv4). Unresolvable entries are logged and skipped.
pub async fn resolve_servers(servers: &[String]) -> Vec<(String, SocketAddr)> {
    // Look all names up at once, so a slow resolver costs one delay, not one per server.
    let lookups: Vec<_> = servers
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|name| tokio::spawn(resolve_one(name)))
        .collect();
    let mut resolved = Vec::new();
    for lookup in lookups {
        if let Ok(Some(server)) = lookup.await {
            resolved.push(server);
        }
    }
    resolved
}

async fn resolve_one(name: String) -> Option<(String, SocketAddr)> {
    let has_port = name.parse::<SocketAddr>().is_ok()
        || name
            .rsplit_once(':')
            .is_some_and(|(host, port)| !host.contains(':') && port.parse::<u16>().is_ok());
    let target = if has_port {
        name.clone()
    } else {
        format!("{name}:{DEFAULT_STUN_PORT}")
    };
    match tokio::net::lookup_host(&target).await {
        Ok(addrs) => match addrs.into_iter().find(SocketAddr::is_ipv4) {
            Some(addr) => Some((name, addr)),
            None => {
                tracing::warn!("STUN server {name} has no IPv4 address");
                None
            }
        },
        Err(e) => {
            tracing::warn!("cannot resolve STUN server {name}: {e}");
            None
        }
    }
}

/// Builds the reply a STUN server would send; for tests of code that talks
/// to a (fake) server.
#[cfg(test)]
pub(crate) fn encode_binding_response(id: &TransactionId, mapped: SocketAddr) -> Vec<u8> {
    let mut value = vec![0u8, 0];
    let port = mapped.port() ^ u16::from_be_bytes([STUN_MAGIC_COOKIE[0], STUN_MAGIC_COOKIE[1]]);
    let mut mask = STUN_MAGIC_COOKIE.to_vec();
    mask.extend_from_slice(id);
    match mapped.ip() {
        IpAddr::V4(ip) => {
            value[1] = 0x01;
            value.extend_from_slice(&port.to_be_bytes());
            value.extend(ip.octets().iter().zip(&mask).map(|(a, m)| a ^ m));
        }
        IpAddr::V6(ip) => {
            value[1] = 0x02;
            value.extend_from_slice(&port.to_be_bytes());
            value.extend(ip.octets().iter().zip(&mask).map(|(a, m)| a ^ m));
        }
    }
    message(BINDING_SUCCESS, id, &[(ATTR_XOR_MAPPED_ADDRESS, value)])
}

#[cfg(test)]
fn message(kind: u16, id: &TransactionId, attributes: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (t, v) in attributes {
        body.extend_from_slice(&t.to_be_bytes());
        body.extend_from_slice(&(v.len() as u16).to_be_bytes());
        body.extend_from_slice(v);
        body.resize(body.len().next_multiple_of(4), 0x20);
    }
    let mut m = kind.to_be_bytes().to_vec();
    m.extend_from_slice(&(body.len() as u16).to_be_bytes());
    m.extend_from_slice(&STUN_MAGIC_COOKIE);
    m.extend_from_slice(id);
    m.extend(body);
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Transaction ID of the RFC 5769 sample responses.
    const RFC5769_ID: TransactionId = [
        0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6, 0x86, 0xfa, 0x87, 0xdf, 0xae,
    ];
    const SOFTWARE: u16 = 0x8022;

    #[test]
    fn binding_request_has_rfc5389_header() {
        let id = [7u8; 12];
        let request = encode_binding_request(&id);
        assert_eq!(request[..2], BINDING_REQUEST.to_be_bytes());
        assert_eq!(request[2..4], [0, 0]); // no attributes
        assert_eq!(request[4..8], STUN_MAGIC_COOKIE);
        assert_eq!(request[8..], id);
        assert!(is_stun(&request));
    }

    #[test]
    fn xor_mapped_address_decodes_ipv4() {
        // RFC 5769 section 2.2: 192.0.2.1 port 32853.
        let value = vec![0x00, 0x01, 0xa1, 0x47, 0xe1, 0x12, 0xa6, 0x43];
        let m = message(
            BINDING_SUCCESS,
            &RFC5769_ID,
            &[(ATTR_XOR_MAPPED_ADDRESS, value)],
        );
        let (id, addr) = decode_binding_response(&m).unwrap();
        assert_eq!(id, RFC5769_ID);
        assert_eq!(addr, "192.0.2.1:32853".parse().unwrap());
    }

    #[test]
    fn xor_mapped_address_decodes_ipv6() {
        // RFC 5769 section 2.3: 2001:db8:1234:5678:11:2233:4455:6677 port 32853.
        let value = vec![
            0x00, 0x02, 0xa1, 0x47, 0x01, 0x13, 0xa9, 0xfa, 0xa5, 0xd3, 0xf1, 0x79, 0xbc, 0x25,
            0xf4, 0xb5, 0xbe, 0xd2, 0xb9, 0xd9,
        ];
        let m = message(
            BINDING_SUCCESS,
            &RFC5769_ID,
            &[(ATTR_XOR_MAPPED_ADDRESS, value)],
        );
        let (_, addr) = decode_binding_response(&m).unwrap();
        assert_eq!(
            addr,
            "[2001:db8:1234:5678:11:2233:4455:6677]:32853"
                .parse()
                .unwrap()
        );
    }

    #[test]
    fn mapped_address_is_used_when_xor_is_absent() {
        let value = vec![0x00, 0x01, 0x13, 0x88, 198, 51, 100, 9]; // 198.51.100.9:5000
        let m = message(
            BINDING_SUCCESS,
            &RFC5769_ID,
            &[(ATTR_MAPPED_ADDRESS, value)],
        );
        let (_, addr) = decode_binding_response(&m).unwrap();
        assert_eq!(addr, "198.51.100.9:5000".parse().unwrap());
    }

    #[test]
    fn error_response_is_reported() {
        let mut value = vec![0, 0, 4, 20];
        value.extend_from_slice(b"Unknown Attribute");
        let m = message(BINDING_ERROR, &RFC5769_ID, &[(ATTR_ERROR_CODE, value)]);
        assert_eq!(
            decode_binding_response(&m),
            Err(StunError::ErrorResponse {
                code: 420,
                reason: "Unknown Attribute".into()
            })
        );
        // Truncated or foreign data is rejected, not misread.
        assert_eq!(decode_binding_response(&m[..19]), Err(StunError::Malformed));
        let request = encode_binding_request(&RFC5769_ID);
        assert_eq!(decode_binding_response(&request), Err(StunError::Malformed));
        let empty = message(BINDING_SUCCESS, &RFC5769_ID, &[]);
        assert_eq!(decode_binding_response(&empty), Err(StunError::NoAddress));
    }

    #[test]
    fn unknown_attributes_are_skipped() {
        // SOFTWARE with an odd length (padded), then the address, then noise.
        let software = b"test vector".to_vec();
        let xor = vec![0x00, 0x01, 0xa1, 0x47, 0xe1, 0x12, 0xa6, 0x43];
        let m = message(
            BINDING_SUCCESS,
            &RFC5769_ID,
            &[
                (SOFTWARE, software),
                (ATTR_XOR_MAPPED_ADDRESS, xor),
                (0x8028, vec![1, 2, 3, 4]),
            ],
        );
        let (_, addr) = decode_binding_response(&m).unwrap();
        assert_eq!(addr, "192.0.2.1:32853".parse().unwrap());
    }

    fn server(name: &str, addr: &str) -> (String, SocketAddr) {
        (name.to_string(), addr.parse().unwrap())
    }

    fn id_of(request: &[u8; 20]) -> TransactionId {
        request[8..].try_into().unwrap()
    }

    #[test]
    fn discovery_retransmits_with_backoff_then_gives_up() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut d = Discovery::new(vec![server("stun.example", "192.0.2.10:3478")], t0);

        let first = d.poll(t0);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, "192.0.2.10:3478".parse().unwrap());
        assert_eq!(d.next_deadline(), Some(ms(500)));
        assert!(d.poll(ms(200)).is_empty());
        for at in [500, 1500, 3500] {
            let again = d.poll(ms(at));
            assert_eq!(again.len(), 1, "resend at {at} ms");
            assert_eq!(id_of(&again[0].1), id_of(&first[0].1), "same transaction");
        }
        assert_eq!(d.next_deadline(), Some(ms(7500)));
        assert!(d.poll(ms(5000)).is_empty());
        assert_eq!(d.outcome(ms(7499)), None);

        let result = d.outcome(ms(7500)).expect("finished");
        let err = result.unwrap_err();
        assert!(err.contains("stun.example"), "{err}");
        assert!(d.poll(ms(9000)).is_empty());
        assert_eq!(d.next_deadline(), None);
    }

    fn answer_all(d: &mut Discovery, now: Instant, mapped: &[&str]) -> Option<PublicEndpoint> {
        let requests = d.poll(now);
        let mut last = None;
        for ((_, request), public) in requests.iter().zip(mapped) {
            let reply = encode_binding_response(&id_of(request), public.parse().unwrap());
            last = d.on_datagram(&reply, now).or(last);
        }
        last
    }

    #[test]
    fn two_servers_with_different_ports_mean_symmetric_nat() {
        let t0 = Instant::now();
        let mut d = Discovery::new(
            vec![
                server("a", "192.0.2.10:3478"),
                server("b", "198.51.100.20:3478"),
            ],
            t0,
        );
        let latest = answer_all(&mut d, t0, &["203.0.113.5:40000", "203.0.113.5:40001"]).unwrap();
        assert_eq!(latest.nat, NatKind::Symmetric);
        let done = d.outcome(t0).expect("all servers answered").unwrap();
        assert_eq!(done.nat, NatKind::Symmetric);
        assert_eq!(d.next_deadline(), None);
    }

    #[test]
    fn two_servers_with_equal_ports_mean_endpoint_independent() {
        let t0 = Instant::now();
        let mut d = Discovery::new(
            vec![
                server("a", "192.0.2.10:3478"),
                server("b", "198.51.100.20:3478"),
            ],
            t0,
        );
        answer_all(&mut d, t0, &["203.0.113.5:40000", "203.0.113.5:40000"]);
        let done = d.outcome(t0).unwrap().unwrap();
        assert_eq!(done.addr, "203.0.113.5:40000".parse().unwrap());
        assert_eq!(done.nat, NatKind::EndpointIndependent);
        assert_eq!(done.via, "a");
    }

    #[test]
    fn single_server_leaves_nat_unknown() {
        let t0 = Instant::now();
        let mut d = Discovery::new(vec![server("only", "192.0.2.10:3478")], t0);
        let first = answer_all(&mut d, t0, &["203.0.113.5:40000"]).unwrap();
        assert_eq!(first.nat, NatKind::Unknown);
        assert_eq!(d.outcome(t0).unwrap().unwrap().via, "only");

        // With two servers, the first answer is usable right away and the
        // outcome waits for the second (or the deadline).
        let mut d = Discovery::new(
            vec![
                server("a", "192.0.2.10:3478"),
                server("b", "198.51.100.20:3478"),
            ],
            t0,
        );
        let requests = d.poll(t0);
        let reply =
            encode_binding_response(&id_of(&requests[0].1), "203.0.113.5:40000".parse().unwrap());
        assert_eq!(d.on_datagram(&reply, t0).unwrap().nat, NatKind::Unknown);
        assert_eq!(d.outcome(t0), None);
        let late = d.outcome(t0 + GIVE_UP_AFTER).unwrap().unwrap();
        assert_eq!((late.nat, late.via.as_str()), (NatKind::Unknown, "a"));
        // Unrelated or replayed datagrams change nothing.
        assert_eq!(d.on_datagram(&reply, t0), None);
        assert_eq!(d.on_datagram(b"not stun", t0), None);
    }

    #[test]
    fn a_server_error_ends_its_query_and_is_reported() {
        let t0 = Instant::now();
        let mut d = Discovery::new(vec![server("strict", "192.0.2.10:3478")], t0);
        let requests = d.poll(t0);
        let mut value = vec![0, 0, 4, 1];
        value.extend_from_slice(b"Unauthorized");
        let reply = message(
            BINDING_ERROR,
            &id_of(&requests[0].1),
            &[(ATTR_ERROR_CODE, value)],
        );
        assert_eq!(d.on_datagram(&reply, t0), None);

        assert!(
            d.poll(t0 + Duration::from_millis(600)).is_empty(),
            "no resend after an error"
        );
        let err = d
            .outcome(t0)
            .expect("no server left to wait for")
            .unwrap_err();
        assert!(
            err.contains("strict (STUN error 401 Unauthorized)"),
            "{err}"
        );
    }

    #[test]
    fn duplicate_servers_are_asked_once() {
        // Two names for one address cannot tell a symmetric NAT apart.
        let t0 = Instant::now();
        let mut d = Discovery::new(
            vec![
                server("a", "192.0.2.10:3478"),
                server("alias-of-a", "192.0.2.10:3478"),
            ],
            t0,
        );
        let requests = d.poll(t0);
        assert_eq!(requests.len(), 1);
        let reply =
            encode_binding_response(&id_of(&requests[0].1), "203.0.113.5:40000".parse().unwrap());
        let public = d.on_datagram(&reply, t0).unwrap();
        assert_eq!((public.nat, public.via.as_str()), (NatKind::Unknown, "a"));
    }

    /// Asks the real default servers. Needs internet access, so it only runs
    /// on request: `cargo test -p tidedesk-core -- --ignored live_stun`.
    #[tokio::test]
    #[ignore]
    async fn live_stun_servers_report_this_computers_public_address() {
        let names: Vec<String> = DEFAULT_STUN_SERVERS.iter().map(|s| s.to_string()).collect();
        let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let mut d = Discovery::new(resolve_servers(&names).await, Instant::now());
        let mut buf = [0u8; 1500];
        loop {
            for (to, request) in d.poll(Instant::now()) {
                socket.send_to(&request, to).await.unwrap();
            }
            if let Some(result) = d.outcome(Instant::now()) {
                let public = result.unwrap();
                println!(
                    "public address {} via {} (NAT: {})",
                    public.addr, public.via, public.nat
                );
                assert!(public.addr.is_ipv4());
                return;
            }
            let wait = d.next_deadline().map_or(Duration::from_millis(100), |t| {
                t.saturating_duration_since(Instant::now())
            });
            if let Ok(Ok((n, _))) = tokio::time::timeout(wait, socket.recv_from(&mut buf)).await {
                d.on_datagram(&buf[..n], Instant::now());
            }
        }
    }

    #[tokio::test]
    async fn resolve_servers_keeps_ipv4_and_adds_the_default_port() {
        let servers = ["127.0.0.1:19302", "127.0.0.2", "[::1]:3478"].map(String::from);
        let resolved = resolve_servers(&servers).await;
        assert_eq!(
            resolved,
            vec![
                server("127.0.0.1:19302", "127.0.0.1:19302"),
                server("127.0.0.2", "127.0.0.2:3478")
            ]
        );
    }
}
