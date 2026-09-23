//! `tidedesk-rendezvous`: introduces TideDesk viewers to hosts by device ID.
//!
//! Hosts register the internet address their router gives them; a viewer
//! asks for a host by ID and both are told each other's address, then punch
//! a direct path. No session data ever passes through this service. State is
//! kept in memory only.

mod server;

use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use tokio::net::UdpSocket;

use server::{Config, Port, Server};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Introduce TideDesk viewers to hosts by device ID. Never relays sessions."
)]
struct Args {
    /// Address and UDP port hosts and viewers use.
    #[arg(long, default_value = "0.0.0.0:47900")]
    listen: SocketAddr,

    /// Second UDP port, which lets hosts detect a symmetric NAT.
    #[arg(long, default_value = "0.0.0.0:47901")]
    alt_listen: SocketAddr,

    /// Seconds a registration lasts without a refresh.
    #[arg(long, default_value_t = 75, value_parser = clap::value_parser!(u64).range(30..=3600))]
    ttl: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let args = Args::parse();
    let main = UdpSocket::bind(args.listen)
        .await
        .with_context(|| format!("listening on {}", args.listen))?;
    let alt = UdpSocket::bind(args.alt_listen)
        .await
        .with_context(|| format!("listening on {}", args.alt_listen))?;
    tracing::info!(
        "TideDesk rendezvous listening on UDP {} and {}",
        args.listen,
        args.alt_listen
    );
    let config = Config {
        ttl: Duration::from_secs(args.ttl),
        ..Config::default()
    };
    serve(main, alt, Server::new(config)).await
}

/// Answers datagrams on both ports until an error stops the sockets.
async fn serve(main: UdpSocket, alt: UdpSocket, mut server: Server) -> Result<()> {
    let mut main_buf = vec![0u8; 2048];
    let mut alt_buf = vec![0u8; 2048];
    let every = |period| tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    let mut sweep = every(Duration::from_secs(10));
    let mut report = every(Duration::from_secs(300));
    loop {
        tokio::select! {
            received = main.recv_from(&mut main_buf) => {
                if let Some((len, from)) = usable(received) {
                    let now = Instant::now();
                    for (to, reply) in server.handle(from, Port::Main, &main_buf[..len], now) {
                        let _ = main.send_to(&reply, to).await;
                    }
                }
            }
            received = alt.recv_from(&mut alt_buf) => {
                if let Some((len, from)) = usable(received) {
                    let now = Instant::now();
                    for (to, reply) in server.handle(from, Port::Alt, &alt_buf[..len], now) {
                        let _ = alt.send_to(&reply, to).await;
                    }
                }
            }
            _ = sweep.tick() => server.sweep(Instant::now()),
            _ = report.tick() => {
                // Counts only: device IDs and addresses are never logged.
                let c = server.counters();
                tracing::info!(
                    "{} hosts registered; since start: {} registrations, {} lookups \
                     ({} not found), {} rejected, {} rate-limited",
                    server.hosts(),
                    c.registered,
                    c.lookups,
                    c.not_found,
                    c.rejected,
                    c.rate_limited
                );
            }
        }
    }
}

/// A received datagram worth handling. Receive errors are transient on UDP
/// (Windows, for one, reports an earlier send's "port unreachable" here), so
/// they are logged and skipped.
fn usable(received: io::Result<(usize, SocketAddr)>) -> Option<(usize, SocketAddr)> {
    received
        .inspect_err(|e| tracing::debug!("receive failed: {e}"))
        .ok()
}

#[cfg(test)]
mod tests {
    use tidedesk_rendezvous_proto::{FromServer, ToServer, decode, encode};

    use super::*;

    #[tokio::test]
    async fn server_binary_loop_answers_on_loopback() {
        let main = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let alt = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (main_addr, alt_addr) = (main.local_addr().unwrap(), alt.local_addr().unwrap());
        tokio::spawn(serve(main, alt, Server::new(Config::default())));

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let me = client.local_addr().unwrap();
        for server in [main_addr, alt_addr] {
            let hello = encode(&ToServer::Hello { nonce: [7; 8] });
            client.send_to(&hello, server).await.unwrap();
            let mut buf = [0u8; 1500];
            let (len, from) =
                tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buf))
                    .await
                    .expect("the service answers")
                    .unwrap();
            assert_eq!(from, server, "answered from the port that was asked");
            let reply = decode::<FromServer>(&buf[..len]).unwrap();
            assert!(matches!(reply, FromServer::Challenge { reflexive, .. } if reflexive == me));
        }
    }
}
