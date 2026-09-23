//! `tidedesk-rendezvous`: introduces TideDesk viewers to hosts by device ID.
//! See the library for what the service does and does not do.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tidedesk_rendezvous::{Config, Server, serve};
use tokio::net::UdpSocket;

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
