//! QUIC endpoint configuration for both sides.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

use crate::identity::{self, HostIdentity};
use crate::nat::SharedSocket;

pub const ALPN: &[u8] = b"tidedesk/1";

fn transport() -> Arc<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    t.keep_alive_interval(Some(Duration::from_secs(5)));
    t.max_idle_timeout(Some(Duration::from_secs(20).try_into().unwrap()));
    // BBR tracks the path's real capacity instead of filling queues until loss,
    // which is what keeps interactive video latency low.
    t.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
    // Cap data queued but not yet acknowledged. Quinn's default (~10 MB) would
    // let seconds of video pile up on a slow link; with a small window the host
    // feels backpressure within a frame or two and skips capture instead.
    t.send_window(512 * 1024);
    t.datagram_receive_buffer_size(Some(256 * 1024));
    t.datagram_send_buffer_size(256 * 1024);
    Arc::new(t)
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// TideDesk endpoints do not "grease" the QUIC fixed bit. Not advertising
/// it means peers always set that bit towards us, which keeps QUIC packets
/// distinguishable from the NAT side channel on a [`SharedSocket`].
pub(crate) fn endpoint_config() -> quinn::EndpointConfig {
    let mut config = quinn::EndpointConfig::default();
    config.grease_quic_bit(false);
    config
}

pub fn server_config(id: &HostIdentity) -> Result<quinn::ServerConfig> {
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![id.cert.clone()], id.key.clone_key())
        .context("loading host certificate")?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
    cfg.transport_config(transport());
    Ok(cfg)
}

pub fn client_config() -> Result<quinn::ClientConfig> {
    let mut tls = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(FingerprintVerifier(provider())))
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut cfg = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls)?));
    cfg.transport_config(transport());
    Ok(cfg)
}

/// Host endpoint on a socket shared with the NAT side channel.
pub fn server_endpoint_on(socket: Arc<SharedSocket>, id: &HostIdentity) -> Result<quinn::Endpoint> {
    endpoint_on(socket, Some(server_config(id)?))
}

pub fn client_endpoint() -> Result<quinn::Endpoint> {
    let bind: SocketAddr = "[::]:0".parse().unwrap();
    let mut ep = quinn::Endpoint::client(bind)
        .or_else(|_| quinn::Endpoint::client("0.0.0.0:0".parse().unwrap()))
        .context("opening client socket")?;
    ep.set_default_client_config(client_config()?);
    Ok(ep)
}

/// Viewer endpoint on a socket shared with the NAT side channel.
pub fn client_endpoint_on(socket: Arc<SharedSocket>) -> Result<quinn::Endpoint> {
    let mut ep = endpoint_on(socket, None)?;
    ep.set_default_client_config(client_config()?);
    Ok(ep)
}

/// `host`, `host:port`, `v4:port`, `[v6]:port` or a bare IPv6 address, with
/// `default_port` added when none is given, ready for DNS lookup.
pub fn with_default_port(host: &str, default_port: u16) -> String {
    let has_port = host.parse::<SocketAddr>().is_ok()
        || host
            .rsplit_once(':')
            .is_some_and(|(h, p)| !h.contains(':') && p.parse::<u16>().is_ok());
    if has_port {
        host.to_string()
    } else if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{default_port}") // bare IPv6
    } else {
        format!("{host}:{default_port}")
    }
}

/// The next connection attempt whose source address is proven: an
/// unproven one is answered with a QUIC Retry (one extra round trip) and
/// comes back proven, while a spoofed source never does. Keeps a host that
/// is reachable from the internet from doing handshake work for, or sending
/// handshake data to, addresses that did not ask.
pub async fn accept_validated(endpoint: &quinn::Endpoint) -> Option<quinn::Incoming> {
    loop {
        let incoming = endpoint.accept().await?;
        if incoming.remote_address_validated() {
            return Some(incoming);
        }
        // quinn guarantees a retry is allowed for an unvalidated address.
        let _ = incoming.retry();
    }
}

fn endpoint_on(
    socket: Arc<SharedSocket>,
    server: Option<quinn::ServerConfig>,
) -> Result<quinn::Endpoint> {
    quinn::Endpoint::new_with_abstract_socket(
        endpoint_config(),
        server,
        socket,
        Arc::new(quinn::TokioRuntime),
    )
    .context("starting the QUIC endpoint")
}

/// Fingerprint of the certificate the host presented on `conn`.
pub fn peer_fingerprint(conn: &quinn::Connection) -> Option<String> {
    let certs = conn
        .peer_identity()?
        .downcast::<Vec<CertificateDer<'static>>>()
        .ok()?;
    Some(identity::fingerprint(certs.first()?))
}

/// Accepts any certificate *chain* but still verifies the handshake signature,
/// proving the host holds the certificate's private key. Whether that
/// certificate is the right one is decided afterwards by fingerprint pinning
/// (see [`crate::identity::KnownHosts`]), before any secret is sent.
#[derive(Debug)]
struct FingerprintVerifier(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::test_identity;

    #[test]
    fn default_port_is_added_only_when_missing() {
        assert_eq!(with_default_port("my-pc", 47800), "my-pc:47800");
        assert_eq!(with_default_port("my-pc:5000", 47800), "my-pc:5000");
        assert_eq!(with_default_port("192.0.2.1", 3478), "192.0.2.1:3478");
        assert_eq!(
            with_default_port("192.0.2.1:19302", 3478),
            "192.0.2.1:19302"
        );
        assert_eq!(
            with_default_port("2001:db8::1", 47800),
            "[2001:db8::1]:47800"
        );
        assert_eq!(
            with_default_port("[2001:db8::1]:9", 47800),
            "[2001:db8::1]:9"
        );
    }

    #[tokio::test]
    async fn unvalidated_addresses_are_retried_before_the_handshake() {
        let identity = test_identity("net-retry");
        let loopback: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (host_socket, _) = SharedSocket::bind(loopback).unwrap();
        let (viewer_socket, _) = SharedSocket::bind(loopback).unwrap();
        let host_addr = host_socket.local_addr().unwrap();
        let host = server_endpoint_on(host_socket, &identity).unwrap();
        let viewer = client_endpoint_on(viewer_socket).unwrap();

        let server = tokio::spawn(async move {
            let incoming = accept_validated(&host).await.unwrap();
            assert!(incoming.remote_address_validated());
            let conn = incoming.await.unwrap();
            conn.closed().await;
        });
        let conn = tokio::time::timeout(
            Duration::from_secs(10),
            viewer.connect(host_addr, "tidedesk-host").unwrap(),
        )
        .await
        .expect("the handshake completes after the retry")
        .unwrap();
        assert_eq!(peer_fingerprint(&conn), Some(identity.fingerprint()));
        conn.close(0u32.into(), b"done");
        server.await.unwrap();
    }
}
