//! Sending a problem report to TideDesk, when the person chooses to (see
//! [`crate::problems`]). The report goes over QUIC to TideDesk's report
//! service, which must show the certificate this build pins; it keeps the
//! report and answers with a reference the person can quote.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tidedesk_rendezvous_proto::report::{
    ALPN, Answer, DEFAULT_PORT, MAX_ANSWER_BYTES, MAX_TEXT, Report, decode, encode,
};

/// Where reports go.
pub const SERVER: &str = "report.tidedesk.app";

/// Fingerprint of the report service's certificate. Reports go to no other.
pub const SERVER_FINGERPRINT: &str =
    "4914 22E1 31C4 A220 7FFB 9906 D9EA DE9F 0365 5C2C 5B73 0F49 274D 2D76 133A 0C91";

/// How long sending may take in all.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Sends `text` to TideDesk and returns the reference it was kept under.
/// Blocks: call it off the UI thread.
pub fn send(text: &str) -> Result<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the network")?;
    runtime.block_on(async {
        tokio::time::timeout(TIMEOUT, async {
            let target = crate::net::with_default_port(SERVER, DEFAULT_PORT);
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host(&target)
                .await
                .with_context(|| format!("could not find {SERVER}"))?
                .collect();
            let mut last = None;
            for address in addresses {
                match send_to(address, SERVER_FINGERPRINT, text).await {
                    Ok(reference) => return Ok(reference),
                    // TideDesk answered: another address would answer the same.
                    Err(e) if e.is::<Answered>() => return Err(e),
                    Err(e) => last = Some(e),
                }
            }
            Err(last.unwrap_or_else(|| anyhow::anyhow!("{SERVER} has no address")))
        })
        .await
        .map_err(|_| anyhow::anyhow!("TideDesk did not answer in time"))?
    })
}

/// Sends `text` to the service at `address`, which must show the certificate
/// with `fingerprint`.
pub async fn send_to(address: SocketAddr, fingerprint: &str, text: &str) -> Result<String> {
    let report = Report {
        version: crate::problems::VERSION.to_string(),
        text: cut(text),
    };
    if !report.fits() {
        bail!("the report is empty");
    }
    let bind: SocketAddr = if address.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let mut endpoint = quinn::Endpoint::client(bind).context("opening a socket")?;
    endpoint.set_default_client_config(crate::net::service_client_config(ALPN, fingerprint)?);
    let conn = endpoint
        .connect(address, SERVER)?
        .await
        .context("could not reach TideDesk")?;
    let (mut send, mut receive) = conn.open_bi().await.context("could not reach TideDesk")?;
    send.write_all(&encode(&report))
        .await
        .context("sending the report")?;
    send.finish().context("sending the report")?;
    let answer = receive
        .read_to_end(MAX_ANSWER_BYTES)
        .await
        .context("waiting for TideDesk's answer")?;
    conn.close(0u32.into(), b"");
    endpoint.wait_idle().await;
    match decode::<Answer>(&answer) {
        Some(Answer::Received(reference)) => Ok(reference),
        Some(Answer::TooMany) => Err(Answered(
            "TideDesk has had many reports from this network lately. Try again later, or send \
             it by email.",
        )
        .into()),
        Some(Answer::Refused) => Err(Answered("TideDesk could not take this report.").into()),
        None => bail!("TideDesk's answer was not understood"),
    }
}

/// TideDesk's service answered, and did not keep the report.
#[derive(Debug)]
struct Answered(&'static str);

impl std::fmt::Display for Answered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Answered {}

/// `text`, cut to what the service takes.
fn cut(text: &str) -> String {
    if text.chars().count() <= MAX_TEXT {
        return text.to_string();
    }
    let note = "\n(Cut short.)";
    let mut out: String = text.chars().take(MAX_TEXT - note.len()).collect();
    out.push_str(note);
    out
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use quinn::crypto::rustls::QuicServerConfig;

    use super::*;

    /// A report service on the loopback that answers every report with
    /// `answer`, and keeps what it received.
    struct Service {
        address: SocketAddr,
        fingerprint: String,
        received: Arc<Mutex<Vec<Report>>>,
        _endpoint: quinn::Endpoint,
    }

    fn service(answer: Answer) -> Service {
        let generated = rcgen::generate_simple_self_signed(vec![SERVER.to_string()]).unwrap();
        let cert = generated.cert.der().clone();
        let key =
            rustls::pki_types::PrivateKeyDer::Pkcs8(generated.signing_key.serialize_der().into());
        let fingerprint = crate::identity::fingerprint(&cert);
        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let config =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls).unwrap()));
        let endpoint = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let (accepting, kept) = (endpoint.clone(), received.clone());
        tokio::spawn(async move {
            while let Some(incoming) = accepting.accept().await {
                let (answer, kept) = (answer.clone(), kept.clone());
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    let (mut send, mut receive) = conn.accept_bi().await.unwrap();
                    let bytes = receive
                        .read_to_end(tidedesk_rendezvous_proto::report::MAX_REPORT_BYTES)
                        .await
                        .unwrap();
                    kept.lock().unwrap().push(decode(&bytes).unwrap());
                    send.write_all(&encode(&answer)).await.unwrap();
                    send.finish().unwrap();
                    let _ = conn.closed().await;
                });
            }
        });
        Service {
            address: endpoint.local_addr().unwrap(),
            fingerprint,
            received,
            _endpoint: endpoint,
        }
    }

    #[tokio::test]
    async fn a_sent_report_arrives_whole_and_comes_back_with_its_reference() {
        let service = service(Answer::Received("R-1234".into()));
        let text = "TideDesk problem report\nWhat: Sharing could not start";
        let reference = send_to(service.address, &service.fingerprint, text)
            .await
            .unwrap();
        assert_eq!(reference, "R-1234");
        let received = service.received.lock().unwrap().clone();
        assert_eq!(
            received,
            [Report {
                version: crate::problems::VERSION.to_string(),
                text: text.to_string()
            }]
        );
    }

    #[tokio::test]
    async fn nothing_is_sent_to_a_server_with_another_certificate() {
        let service = service(Answer::Received("R-1234".into()));
        let error = send_to(service.address, SERVER_FINGERPRINT, "a problem")
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("could not reach TideDesk"),
            "{error:#}"
        );
        assert!(service.received.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_busy_service_says_so_in_words() {
        let service = service(Answer::TooMany);
        let error = send_to(service.address, &service.fingerprint, "a problem")
            .await
            .unwrap_err();
        assert!(error.is::<Answered>());
        assert!(error.to_string().contains("Try again later"), "{error}");
    }

    #[tokio::test]
    async fn an_empty_report_is_not_sent() {
        let service = service(Answer::Received("R-1234".into()));
        assert!(
            send_to(service.address, &service.fingerprint, "  \n")
                .await
                .is_err()
        );
        assert!(service.received.lock().unwrap().is_empty());
    }

    #[test]
    fn a_long_report_is_cut_to_what_the_service_takes() {
        let long = "a".repeat(MAX_TEXT + 10);
        let sent = cut(&long);
        assert_eq!(sent.chars().count(), MAX_TEXT);
        assert!(sent.ends_with("(Cut short.)"));
        assert_eq!(cut("short"), "short");
    }

    #[test]
    fn the_pinned_fingerprint_is_a_whole_sha256() {
        assert_eq!(
            crate::identity::normalize_fingerprint(SERVER_FINGERPRINT).len(),
            64
        );
    }
}
