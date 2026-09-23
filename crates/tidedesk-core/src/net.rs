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

/// Host endpoint on its own socket. Must be called inside a tokio runtime.
pub fn server_endpoint(listen: SocketAddr, id: &HostIdentity) -> Result<quinn::Endpoint> {
    let (socket, _side_channel) =
        SharedSocket::bind(listen).with_context(|| format!("listening on {listen}"))?;
    server_endpoint_on(socket, id)
}

/// Host endpoint on a socket shared with the NAT side channel.
pub fn server_endpoint_on(socket: Arc<SharedSocket>, id: &HostIdentity) -> Result<quinn::Endpoint> {
    quinn::Endpoint::new_with_abstract_socket(
        endpoint_config(),
        Some(server_config(id)?),
        socket,
        Arc::new(quinn::TokioRuntime),
    )
    .context("starting the QUIC endpoint")
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
    let mut ep = quinn::Endpoint::new_with_abstract_socket(
        endpoint_config(),
        None,
        socket,
        Arc::new(quinn::TokioRuntime),
    )
    .context("starting the QUIC endpoint")?;
    ep.set_default_client_config(client_config()?);
    Ok(ep)
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
