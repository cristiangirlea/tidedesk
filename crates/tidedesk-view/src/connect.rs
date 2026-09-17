//! Connecting to a host: resolve, QUIC handshake, fingerprint pinning, auth.

use std::net::SocketAddr;

use anyhow::{Context, Result, bail};
use tidedesk_core::identity::{KnownHosts, PinStatus, normalize_fingerprint};
use tidedesk_core::protocol::{self, ClientMessage, PROTOCOL_VERSION, ServerMessage};
use tidedesk_core::{DEFAULT_PORT, auth, net, paths};

pub struct Session {
    pub endpoint: quinn::Endpoint,
    pub conn: quinn::Connection,
    pub send: quinn::SendStream,
    pub recv: quinn::RecvStream,
    pub host_name: String,
    pub width: u32,
    pub height: u32,
    pub audio: bool,
}

pub struct ConnectOptions {
    pub host: String,
    pub code: String,
    pub want_audio: bool,
    pub expected_fingerprint: Option<String>,
    pub accept_new_fingerprint: bool,
}

/// `host`, `host:port`, `[v6]:port` → `(display form, socket address)`.
async fn resolve(host: &str) -> Result<(String, SocketAddr)> {
    let with_port = if host.parse::<SocketAddr>().is_ok()
        || (host
            .rsplit_once(':')
            .is_some_and(|(h, p)| !h.contains(':') && p.parse::<u16>().is_ok()))
    {
        host.to_string()
    } else if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{DEFAULT_PORT}") // bare IPv6
    } else {
        format!("{host}:{DEFAULT_PORT}")
    };
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
    /// Normalised `host:port`, the key used for pinning.
    pub address: String,
    pub fingerprint: String,
    pub status: PinStatus,
}

/// Completes only the encrypted handshake to learn the host's fingerprint and
/// how it compares with the pinned one, so a UI can ask before connecting.
pub async fn probe(host: &str) -> Result<Probe> {
    let (address, addr) = resolve(host).await?;
    let endpoint = net::client_endpoint()?;
    let conn = endpoint
        .connect(addr, "tidedesk-host")?
        .await
        .with_context(|| format!("could not reach a TideDesk host at {addr}"))?;
    let fingerprint = net::peer_fingerprint(&conn).context("host presented no certificate")?;
    conn.close(0u32.into(), b"probe");
    let status = KnownHosts::load(&paths::config_dir()?)?.check(&address, &fingerprint);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(300), endpoint.wait_idle()).await;
    Ok(Probe {
        address,
        fingerprint,
        status,
    })
}

pub async fn connect(opts: &ConnectOptions) -> Result<Session> {
    let (display, addr) = resolve(&opts.host).await?;
    let endpoint = net::client_endpoint()?;
    let conn = endpoint
        .connect(addr, "tidedesk-host")?
        .await
        .with_context(|| format!("could not reach a TideDesk host at {addr}"))?;

    // Verify who we are talking to *before* using the access code.
    let fp = net::peer_fingerprint(&conn).context("host presented no certificate")?;
    let mut known = KnownHosts::load(&paths::config_dir()?)?;
    match known.check(&display, &fp) {
        PinStatus::Trusted => {}
        PinStatus::Unknown => match &opts.expected_fingerprint {
            Some(expected) if normalize_fingerprint(expected) != normalize_fingerprint(&fp) => {
                bail!("host fingerprint {fp} does not match the one you supplied");
            }
            Some(_) => known.pin(&display, &fp)?,
            None => {
                eprintln!(
                    "First connection to {display}.\n  Host fingerprint: {fp}\n  \
                     It should match the fingerprint shown in the host's window. Remembering it."
                );
                known.pin(&display, &fp)?;
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
            known.pin(&display, &fp)?;
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
            endpoint,
            conn,
            send,
            recv,
            host_name,
            width,
            height,
            audio,
        }),
        Some(ServerMessage::Rejected { reason }) => bail!("host refused the connection: {reason}"),
        Some(_) => bail!("host sent session data before Welcome"),
        None => bail!("host closed the connection during the handshake"),
    }
}
