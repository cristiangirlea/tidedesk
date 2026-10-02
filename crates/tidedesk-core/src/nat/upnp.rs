//! UPnP IGD port mapping: the method most home routers speak, asked after
//! PCP and NAT-PMP (see [`super::portmap`]). An SSDP search finds the
//! router, its description names its WAN connection service, and SOAP over
//! HTTP asks that service to forward the port.
//!
//! Only the router itself is believed: answers to the search from any other
//! address are ignored, and the description and the service must be at the
//! router's own address, so another device on the network cannot point
//! TideDesk anywhere else.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use super::portmap::{MapError, Method};

/// Where SSDP searches go.
pub const SSDP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);

/// What a port mapping is called in the router's list.
const DESCRIPTION: &str = "TideDesk";

/// An `http://` address at a literal IPv4 address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HttpUrl {
    pub host: SocketAddrV4,
    pub path: String,
}

/// Where to ask the router: its WAN connection service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Gateway {
    pub control: HttpUrl,
    /// For example `urn:schemas-upnp-org:service:WANIPConnection:1`.
    pub service: String,
}

/// An SSDP search for devices of type `target`.
fn search_request(target: &str) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: {SSDP}\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {target}\r\n\r\n"
    )
}

/// A header's value, by its name in any case.
fn header<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

/// Where an internet gateway's description is, from its answer to a search.
fn parse_search_answer(data: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(data).ok()?;
    if !text.starts_with("HTTP/1.1 200") {
        return None;
    }
    let target = header(text, "ST")?;
    if !target.contains(":device:InternetGatewayDevice:") {
        return None;
    }
    Some(header(text, "LOCATION")?.to_string())
}

/// `http://a.b.c.d[:port]/path`; names and other schemes are refused.
fn parse_url(text: &str) -> Option<HttpUrl> {
    let rest = text.trim().strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let host = match authority.split_once(':') {
        Some((ip, port)) => SocketAddrV4::new(ip.parse().ok()?, port.parse().ok()?),
        None => SocketAddrV4::new(authority.parse().ok()?, 80),
    };
    Some(HttpUrl {
        host,
        path: path.to_string(),
    })
}

/// The text inside the first `<name>…</name>` in `text`.
fn element<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&format!("</{name}>"))? + start;
    Some(text[start..end].trim())
}

/// The router's WAN connection service, from its description at
/// `location`: IP before PPP, the newer version first. A service anywhere
/// but the description's own address is refused.
fn find_service(description: &str, location: &HttpUrl) -> Option<Gateway> {
    const WANTED: [&str; 3] = [
        "urn:schemas-upnp-org:service:WANIPConnection:2",
        "urn:schemas-upnp-org:service:WANIPConnection:1",
        "urn:schemas-upnp-org:service:WANPPPConnection:1",
    ];
    let services: Vec<(&str, &str)> = description
        .split("<service>")
        .skip(1)
        .filter_map(|block| {
            let block = block.split("</service>").next()?;
            Some((
                element(block, "serviceType")?,
                element(block, "controlURL")?,
            ))
        })
        .collect();
    let base = element(description, "URLBase")
        .and_then(parse_url)
        .unwrap_or_else(|| location.clone());
    WANTED.iter().find_map(|wanted| {
        let (service, control) = services.iter().find(|(s, _)| s == wanted)?;
        let control = if control.starts_with("http://") {
            parse_url(control)?
        } else {
            let path = if control.starts_with('/') {
                control.to_string()
            } else {
                format!("/{control}")
            };
            HttpUrl {
                host: base.host,
                path,
            }
        };
        (control.host.ip() == location.host.ip()).then(|| Gateway {
            control,
            service: service.to_string(),
        })
    })
}

/// A SOAP call of `action` with `arguments`, as the HTTP request to send.
fn soap_request(gateway: &Gateway, action: &str, arguments: &[(&str, String)]) -> Vec<u8> {
    let service = &gateway.service;
    let arguments: String = arguments
        .iter()
        .map(|(name, value)| format!("<{name}>{value}</{name}>"))
        .collect();
    let body = format!(
        "<?xml version=\"1.0\"?>\r\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>\
         <u:{action} xmlns:u=\"{service}\">{arguments}</u:{action}></s:Body></s:Envelope>\r\n"
    );
    format!(
        "POST {path} HTTP/1.1\r\nHOST: {host}\r\nCONTENT-TYPE: text/xml; charset=\"utf-8\"\r\n\
         SOAPACTION: \"{service}#{action}\"\r\nCONTENT-LENGTH: {length}\r\nCONNECTION: close\r\n\r\n{body}",
        path = gateway.control.path,
        host = gateway.control.host,
        length = body.len(),
    )
    .into_bytes()
}

/// An HTTP response's status and body: its length from Content-Length or
/// chunks, or else all that follows the head.
fn parse_http_response(bytes: &[u8]) -> Option<(u16, String)> {
    let text = String::from_utf8_lossy(bytes);
    let (head, rest) = text.split_once("\r\n\r\n")?;
    let status: u16 = head
        .strip_prefix("HTTP/1.")?
        .get(2..)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let chunked = header(head, "Transfer-Encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"));
    let body = if chunked {
        let mut body = String::new();
        let mut rest = rest;
        loop {
            let (size, after) = rest.split_once("\r\n")?;
            let size = usize::from_str_radix(size.trim(), 16).ok()?;
            if size == 0 {
                break;
            }
            body.push_str(after.get(..size)?);
            rest = after.get(size..)?.strip_prefix("\r\n")?;
        }
        body
    } else {
        match header(head, "Content-Length").and_then(|n| n.parse::<usize>().ok()) {
            Some(length) => rest.get(..length)?.to_string(),
            None => rest.to_string(),
        }
    };
    Some((status, body))
}

/// An output argument of a SOAP answer.
fn soap_value(body: &str, name: &str) -> Option<String> {
    element(body, name).map(str::to_string)
}

/// The UPnP error code of a SOAP fault.
fn soap_error(body: &str) -> Option<u16> {
    element(body, "errorCode")?.parse().ok()
}

/// Most bytes read from a router: a description or a SOAP answer is far
/// smaller.
const MAX_ANSWER: u64 = 64 * 1024;

/// Sends an HTTP `request` to `host` and reads the answer, within `wait`.
async fn exchange(host: SocketAddrV4, request: &[u8], wait: Duration) -> Option<(u16, String)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let talk = async {
        let mut stream = tokio::net::TcpStream::connect(host).await.ok()?;
        stream.write_all(request).await.ok()?;
        let mut answer = Vec::new();
        (&mut stream)
            .take(MAX_ANSWER)
            .read_to_end(&mut answer)
            .await
            .ok()?;
        parse_http_response(&answer)
    };
    tokio::time::timeout(wait, talk).await.ok().flatten()
}

/// The internet gateway that `router` describes, found by searching at
/// `search_to`; only answers from `router` itself count.
pub(crate) async fn discover(
    router: Ipv4Addr,
    search_to: SocketAddr,
    wait: Duration,
) -> Option<Gateway> {
    let socket = tokio::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .await
        .ok()?;
    for version in [2, 1] {
        let target = format!("urn:schemas-upnp-org:device:InternetGatewayDevice:{version}");
        socket
            .send_to(search_request(&target).as_bytes(), search_to)
            .await
            .ok()?;
    }
    let deadline = tokio::time::Instant::now() + wait;
    let mut buffer = [0u8; 2048];
    loop {
        let (n, from) = match tokio::time::timeout_at(deadline, socket.recv_from(&mut buffer)).await
        {
            Ok(Ok(answer)) => answer,
            // Windows reports a closed port as an error on the next receive.
            Ok(Err(_)) => continue,
            Err(_) => return None,
        };
        if from.ip() != std::net::IpAddr::V4(router) {
            continue;
        }
        let Some(location) = parse_search_answer(&buffer[..n]).and_then(|l| parse_url(&l)) else {
            continue;
        };
        if *location.host.ip() != router {
            continue;
        }
        let request = format!(
            "GET {} HTTP/1.1\r\nHOST: {}\r\nCONNECTION: close\r\n\r\n",
            location.path, location.host
        );
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if let Some((200, description)) = exchange(location.host, request.as_bytes(), left).await
            && let Some(gateway) = find_service(&description, &location)
        {
            return Some(gateway);
        }
    }
}

/// Calls `action` with `arguments`: the answer's body, or the router's
/// error code (its HTTP status when it gave no UPnP one).
async fn call(
    gateway: &Gateway,
    action: &str,
    arguments: &[(&str, String)],
    wait: Duration,
) -> Result<String, MapError> {
    let request = soap_request(gateway, action, arguments);
    match exchange(gateway.control.host, &request, wait).await {
        Some((200, body)) => Ok(body),
        Some((status, body)) => Err(MapError::Refused {
            method: Method::Upnp,
            code: soap_error(&body).unwrap_or(status),
        }),
        None => Err(MapError::NoAnswer),
    }
}

/// Forwards UDP `port` to `client` for `lease` seconds; the router's
/// internet address and port, and the lease it gave (0: until removed).
pub(crate) async fn add(
    gateway: &Gateway,
    client: Ipv4Addr,
    port: u16,
    lease: u32,
    wait: Duration,
) -> Result<(SocketAddrV4, u32), MapError> {
    let body = call(gateway, "GetExternalIPAddress", &[], wait).await?;
    let outer: Ipv4Addr = soap_value(&body, "NewExternalIPAddress")
        .and_then(|ip| ip.parse().ok())
        .ok_or(MapError::NoAnswer)?;
    let arguments = |lease: u32| {
        [
            ("NewRemoteHost", String::new()),
            ("NewExternalPort", port.to_string()),
            ("NewProtocol", "UDP".to_string()),
            ("NewInternalPort", port.to_string()),
            ("NewInternalClient", client.to_string()),
            ("NewEnabled", "1".to_string()),
            ("NewPortMappingDescription", DESCRIPTION.to_string()),
            ("NewLeaseDuration", lease.to_string()),
        ]
    };
    let external = SocketAddrV4::new(outer, port);
    match call(gateway, "AddPortMapping", &arguments(lease), wait).await {
        Ok(_) => Ok((external, lease)),
        // OnlyPermanentLeasesSupported: kept until removed instead.
        Err(MapError::Refused { code: 725, .. }) => {
            call(gateway, "AddPortMapping", &arguments(0), wait).await?;
            Ok((external, 0))
        }
        Err(e) => Err(e),
    }
}

/// Removes the forwarding of UDP `port`.
pub(crate) async fn delete(gateway: &Gateway, port: u16, wait: Duration) -> Result<(), MapError> {
    let arguments = [
        ("NewRemoteHost", String::new()),
        ("NewExternalPort", port.to_string()),
        ("NewProtocol", "UDP".to_string()),
    ];
    call(gateway, "DeletePortMapping", &arguments, wait)
        .await
        .map(|_| ())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A header's value, for the fake router in `portmap`'s tests.
    pub(in crate::nat) fn header_value<'a>(text: &'a str, name: &str) -> Option<&'a str> {
        header(text, name)
    }

    #[tokio::test]
    async fn answers_from_anywhere_but_the_router_are_ignored() {
        let (search_at, _) = super::super::portmap::tests::fake_upnp_router(false).await;
        // The fake answers from 127.0.0.1; a router elsewhere is not it.
        let elsewhere = Ipv4Addr::new(127, 0, 0, 2);
        assert_eq!(
            discover(elsewhere, search_at, Duration::from_millis(300)).await,
            None
        );
        assert!(
            discover(Ipv4Addr::LOCALHOST, search_at, Duration::from_millis(300))
                .await
                .is_some()
        );
    }

    #[test]
    fn a_search_asks_for_internet_gateways_and_reads_where_they_are() {
        let request = search_request("urn:schemas-upnp-org:device:InternetGatewayDevice:1");
        assert!(request.starts_with("M-SEARCH * HTTP/1.1\r\n"));
        assert!(request.contains("\r\nHOST: 239.255.255.250:1900\r\n"));
        assert!(request.contains("\r\nMAN: \"ssdp:discover\"\r\n"));
        assert!(
            request.contains("\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n")
        );
        assert!(request.ends_with("\r\n\r\n"));

        let answer = b"HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\n\
            ST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\
            Location: http://192.168.1.1:5000/rootDesc.xml\r\nSERVER: miniupnpd\r\n\r\n";
        assert_eq!(
            parse_search_answer(answer).as_deref(),
            Some("http://192.168.1.1:5000/rootDesc.xml")
        );
        let printer = b"HTTP/1.1 200 OK\r\nST: urn:schemas-upnp-org:device:Printer:1\r\n\
            LOCATION: http://192.168.1.9/desc.xml\r\n\r\n";
        assert_eq!(parse_search_answer(printer), None, "not a gateway");
        assert_eq!(parse_search_answer(b"NOTIFY * HTTP/1.1\r\n\r\n"), None);
    }

    #[test]
    fn only_plain_http_at_an_ipv4_address_is_followed() {
        assert_eq!(
            parse_url("http://192.168.1.1:5000/rootDesc.xml"),
            Some(HttpUrl {
                host: "192.168.1.1:5000".parse().unwrap(),
                path: "/rootDesc.xml".into()
            })
        );
        assert_eq!(
            parse_url("http://10.0.0.138/"),
            Some(HttpUrl {
                host: "10.0.0.138:80".parse().unwrap(),
                path: "/".into()
            })
        );
        assert_eq!(parse_url("https://192.168.1.1/x"), None);
        assert_eq!(parse_url("http://router.local/x"), None, "no names");
    }

    const DESCRIPTION_XML: &str = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
<device><deviceType>urn:schemas-upnp-org:device:InternetGatewayDevice:1</deviceType>
<deviceList><device><deviceType>urn:schemas-upnp-org:device:WANDevice:1</deviceType>
<serviceList><service>
<serviceType>urn:schemas-upnp-org:service:WANCommonInterfaceConfig:1</serviceType>
<controlURL>/ctl/CmnIfCfg</controlURL></service></serviceList>
<deviceList><device><deviceType>urn:schemas-upnp-org:device:WANConnectionDevice:1</deviceType>
<serviceList><service>
<serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
<serviceId>urn:upnp-org:serviceId:WANIPConn1</serviceId>
<controlURL> /ctl/IPConn </controlURL>
<eventSubURL>/evt/IPConn</eventSubURL>
<SCPDURL>/WANIPCn.xml</SCPDURL>
</service></serviceList></device></deviceList>
</device></deviceList></device></root>"#;

    fn location() -> HttpUrl {
        parse_url("http://192.168.1.1:5000/rootDesc.xml").unwrap()
    }

    #[test]
    fn the_description_names_the_wan_connection_service() {
        assert_eq!(
            find_service(DESCRIPTION_XML, &location()),
            Some(Gateway {
                control: parse_url("http://192.168.1.1:5000/ctl/IPConn").unwrap(),
                service: "urn:schemas-upnp-org:service:WANIPConnection:1".into()
            })
        );
        // A PPP connection, its control address given in full.
        let ppp = DESCRIPTION_XML
            .replace("WANIPConnection:1", "WANPPPConnection:1")
            .replace(" /ctl/IPConn ", "http://192.168.1.1:5000/ctl/PPPConn");
        assert_eq!(
            find_service(&ppp, &location()).map(|g| g.control.path),
            Some("/ctl/PPPConn".into())
        );
        // A service elsewhere than the router is not followed.
        let elsewhere = DESCRIPTION_XML.replace(" /ctl/IPConn ", "http://192.168.1.77/ctl");
        assert_eq!(find_service(&elsewhere, &location()), None);
        assert_eq!(find_service("<root></root>", &location()), None);
    }

    #[test]
    fn soap_requests_and_answers() {
        let gateway = find_service(DESCRIPTION_XML, &location()).unwrap();
        let request = soap_request(
            &gateway,
            "AddPortMapping",
            &[
                ("NewExternalPort", "47800".into()),
                ("NewProtocol", "UDP".into()),
            ],
        );
        let request = String::from_utf8(request).unwrap();
        let (head, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("POST /ctl/IPConn HTTP/1.1\r\n"));
        assert!(head.contains("\r\nHOST: 192.168.1.1:5000"));
        assert!(head.contains(
            "\r\nSOAPACTION: \"urn:schemas-upnp-org:service:WANIPConnection:1#AddPortMapping\""
        ));
        assert!(head.contains(&format!("\r\nCONTENT-LENGTH: {}", body.len())));
        assert!(body.contains(
            "<u:AddPortMapping xmlns:u=\"urn:schemas-upnp-org:service:WANIPConnection:1\">\
             <NewExternalPort>47800</NewExternalPort><NewProtocol>UDP</NewProtocol>\
             </u:AddPortMapping>"
        ));

        let ok = b"HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nContent-Length: 108\r\n\r\n\
            <s:Envelope><s:Body><u:GetExternalIPAddressResponse><NewExternalIPAddress>\
            203.0.113.7</NewExternalIPAddress>";
        let (status, body) = parse_http_response(ok).unwrap();
        assert_eq!(status, 200);
        assert_eq!(
            soap_value(&body, "NewExternalIPAddress").as_deref(),
            Some("203.0.113.7")
        );

        let chunked = b"HTTP/1.1 500 Internal Server Error\r\nTransfer-Encoding: chunked\r\n\r\n\
            19\r\n<UPnPError><errorCode>718\r\n18\r\n</errorCode></UPnPError>\r\n0\r\n\r\n";
        let (status, body) = parse_http_response(chunked).unwrap();
        assert_eq!(status, 500);
        assert_eq!(soap_error(&body), Some(718));
        assert_eq!(parse_http_response(b"garbage"), None);
    }
}
