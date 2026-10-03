//! Sending a problem report to TideDesk, when the person chooses to (see
//! [`crate::problems`]). The report goes over QUIC to TideDesk's report
//! service, which must show the certificate this build pins; it keeps the
//! report and answers with a reference the person can quote. An organisation
//! can send reports to its own service instead, or nowhere (see
//! [`crate::policy::Reports`]).

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::policy::Reports;
use tidedesk_rendezvous_proto::report::{
    ALPN, Answer, DEFAULT_PORT, MAX_ANSWER_BYTES, MAX_TEXT, Report, decode, encode,
};

/// Where reports go.
pub const SERVER: &str = "report.tidedesk.app";

/// Fingerprints of the report service's certificates: the one in use, and
/// a spare kept offline to move to if the first is ever lost. Reports go to
/// no other.
pub const SERVER_FINGERPRINTS: &[&str] = &[
    "4914 22E1 31C4 A220 7FFB 9906 D9EA DE9F 0365 5C2C 5B73 0F49 274D 2D76 133A 0C91",
    "71A9 8ACF BCBB 29B8 3F3D 884F 0A96 C615 01F4 A828 E43B 7308 DE80 1E3F 8243 F0DC",
];

/// How long sending may take in all.
const TIMEOUT: Duration = Duration::from_secs(20);

/// How long one of the service's addresses may take, so that one that
/// cannot be reached leaves time for the next.
const TIMEOUT_EACH: Duration = Duration::from_secs(8);

/// Sends `text` where the organisation's settings say, TideDesk by
/// default, and returns the reference it was kept under. Blocks: call it off
/// the UI thread.
pub fn send(text: &str) -> Result<String> {
    send_as(&crate::policy::current().reports, text)
}

/// Sends `text` where `reports` says.
pub fn send_as(reports: &Reports, text: &str) -> Result<String> {
    let (server, pins): (&str, Vec<&str>) = match reports {
        Reports::TideDesk => (SERVER, SERVER_FINGERPRINTS.to_vec()),
        Reports::Own {
            server,
            fingerprint,
        } => (server, vec![fingerprint]),
        Reports::Off => bail!("your organisation does not allow sending reports"),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the network")?;
    runtime.block_on(async {
        tokio::time::timeout(TIMEOUT, async {
            let target = crate::net::with_default_port(server, DEFAULT_PORT);
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host(&target)
                .await
                .with_context(|| format!("could not find {server}"))?
                .collect();
            let mut last = None;
            for address in addresses {
                let attempt = tokio::time::timeout(
                    TIMEOUT_EACH,
                    send_to(address, host(&target), &pins, text),
                );
                match attempt
                    .await
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("no answer from {address}")))
                {
                    Ok(reference) => return Ok(reference),
                    // The service answered: another address would answer the same.
                    Err(e) if e.is::<Answered>() => return Err(e),
                    Err(e) => last = Some(e),
                }
            }
            Err(last.unwrap_or_else(|| anyhow::anyhow!("{server} has no address")))
        })
        .await
        .map_err(|_| anyhow::anyhow!("the report service did not answer in time"))?
    })
}

/// The host in `host:port` or `[v6]:port`.
fn host(target: &str) -> &str {
    let host = target.rsplit_once(':').map_or(target, |(host, _)| host);
    host.trim_start_matches('[').trim_end_matches(']')
}

/// Sends `text` to the service named `name` at `address`, which must show a
/// certificate with one of `fingerprints`.
pub async fn send_to(
    address: SocketAddr,
    name: &str,
    fingerprints: &[&str],
    text: &str,
) -> Result<String> {
    let report = Report {
        version: crate::problems::VERSION.to_string(),
        text: cut(text),
    };
    if report.text.trim().is_empty() {
        bail!("the report is empty");
    }
    if !report.fits() {
        bail!("the report is longer than the report service takes");
    }
    let bind: SocketAddr = if address.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let mut endpoint = quinn::Endpoint::client(bind).context("opening a socket")?;
    endpoint.set_default_client_config(crate::net::service_client_config(ALPN, fingerprints)?);
    let conn = endpoint
        .connect(address, name)?
        .await
        .context("could not reach the report service")?;
    let (mut send, mut receive) = conn
        .open_bi()
        .await
        .context("could not reach the report service")?;
    send.write_all(&encode(&report))
        .await
        .context("sending the report")?;
    send.finish().context("sending the report")?;
    let answer = receive
        .read_to_end(MAX_ANSWER_BYTES)
        .await
        .context("waiting for the report service's answer")?;
    conn.close(0u32.into(), b"");
    endpoint.wait_idle().await;
    match decode::<Answer>(&answer) {
        Some(Answer::Received(reference)) => Ok(reference),
        Some(Answer::TooMany) => Err(Answered(
            "the report service has had many reports from this network lately; try again later",
        )
        .into()),
        Some(Answer::Refused) => {
            Err(Answered("the report service could not take this report").into())
        }
        None => bail!("the report service's answer was not understood"),
    }
}

/// The report service answered, and did not keep the report.
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
        let reference = send_to(service.address, SERVER, &[&service.fingerprint], text)
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
        let error = send_to(service.address, SERVER, SERVER_FINGERPRINTS, "a problem")
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("could not reach the report service"),
            "{error:#}"
        );
        assert!(service.received.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_busy_service_says_so_in_words() {
        let service = service(Answer::TooMany);
        let error = send_to(
            service.address,
            SERVER,
            &["0000", &service.fingerprint],
            "a problem",
        )
        .await
        .unwrap_err();
        assert!(error.is::<Answered>());
        assert!(error.to_string().contains("try again later"), "{error}");
    }

    #[tokio::test]
    async fn an_empty_report_is_not_sent() {
        let service = service(Answer::Received("R-1234".into()));
        assert!(
            send_to(service.address, SERVER, &[&service.fingerprint], "  \n")
                .await
                .is_err()
        );
        assert!(service.received.lock().unwrap().is_empty());
    }

    #[test]
    fn an_organisations_own_service_gets_the_report() {
        // `send_as` runs its own runtime, so the service runs on another.
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let service = runtime.block_on(async { service(Answer::Received("ORG-1".into())) });
        let own = Reports::Own {
            server: service.address.to_string(),
            fingerprint: service.fingerprint.clone(),
        };
        assert_eq!(send_as(&own, "What: a problem").unwrap(), "ORG-1");
        assert_eq!(service.received.lock().unwrap().len(), 1);
    }

    #[test]
    fn nothing_is_sent_when_the_organisation_turned_reports_off() {
        let error = send_as(&Reports::Off, "What: a problem").unwrap_err();
        assert!(error.to_string().contains("organisation"), "{error}");
    }

    #[test]
    fn the_host_is_taken_from_the_address() {
        assert_eq!(host("report.tidedesk.app:47902"), "report.tidedesk.app");
        assert_eq!(host("127.0.0.1:47902"), "127.0.0.1");
        assert_eq!(host("[::1]:47902"), "::1");
    }

    #[test]
    fn a_long_report_is_cut_to_what_the_service_takes() {
        let long = "a".repeat(MAX_TEXT + 10);
        let sent = cut(&long);
        assert_eq!(sent.chars().count(), MAX_TEXT);
        assert!(sent.ends_with("(Cut short.)"));
        assert_eq!(cut("short"), "short");
    }

    /// A real report service on this machine, named by
    /// `TIDEDESK_TEST_REPORTS=ip:port`, with the certificate whose fingerprint
    /// is `TIDEDESK_TEST_REPORTS_FINGERPRINT`. The service is not part of this
    /// repository; its own CI runs this.
    #[tokio::test]
    #[ignore = "needs a report service on this machine: TIDEDESK_TEST_REPORTS=ip:port"]
    async fn a_report_reaches_a_real_service() {
        let service: SocketAddr = std::env::var("TIDEDESK_TEST_REPORTS")
            .expect("TIDEDESK_TEST_REPORTS=ip:port names a report service on this machine")
            .parse()
            .expect("TIDEDESK_TEST_REPORTS is an ip:port");
        let fingerprint = std::env::var("TIDEDESK_TEST_REPORTS_FINGERPRINT")
            .expect("TIDEDESK_TEST_REPORTS_FINGERPRINT is the service certificate's SHA-256");
        let reference = send_to(service, SERVER, &[&fingerprint], "What: a contract test")
            .await
            .unwrap();
        assert!(reference.starts_with("R-"), "{reference}");
    }

    #[test]
    fn the_pinned_fingerprints_are_whole_sha256s() {
        for fingerprint in SERVER_FINGERPRINTS {
            assert_eq!(
                crate::identity::normalize_fingerprint(fingerprint).len(),
                64
            );
        }
    }
}
