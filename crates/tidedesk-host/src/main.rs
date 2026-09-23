//! `tidedesk-host`: shares this machine's screen and audio.

// Release builds are GUI apps with no console window; console output still
// reaches a terminal the host was started from (see `platform::attach_console`).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod audio;
mod capture;
mod config;
mod gui;
mod icon;
mod input;
mod internet;
mod platform;
mod session;
mod tray;
mod video;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tidedesk_core::identity::HostIdentity;
use tidedesk_core::nat::stun::STUN_REFRESH;
use tidedesk_core::nat::{Agent, PublicStatus, SharedSocket};
use tidedesk_core::{auth, net, paths, stats};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Share this computer's screen and sound with a TideDesk viewer."
)]
struct Args {
    /// Run without a window, printing status to the console.
    #[arg(long)]
    headless: bool,

    /// Start with the window hidden in the tray.
    #[arg(long)]
    tray: bool,

    // The options below override the saved settings for this run only.
    /// Address and UDP port to listen on [default: 0.0.0.0 and the saved port].
    #[arg(long)]
    listen: Option<SocketAddr>,

    /// Which display to share (see --list-displays).
    #[arg(long)]
    display: Option<usize>,

    /// Maximum frames per second.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=120))]
    fps: Option<u32>,

    /// Target video bitrate in kbit/s.
    #[arg(long, value_parser = clap::value_parser!(u32).range(250..=100_000))]
    bitrate: Option<u32>,

    /// Do not share system audio.
    #[arg(long)]
    no_audio: bool,

    /// Log frame rate, bitrate and encode time every two seconds.
    #[arg(long)]
    stats: bool,

    /// Replace the saved access code with a new random one.
    #[arg(long)]
    new_code: bool,

    /// Print the available displays and exit.
    #[arg(long)]
    list_displays: bool,

    /// Reach this host through a relay server (not available yet).
    #[arg(long, value_name = "URL")]
    relay: Option<String>,
}

const RELAY_NOTICE: &str = "Relay connections are not implemented yet.\n\
For now, reach this host over the internet through a VPN such as Tailscale or WireGuard, \
or forward its UDP port on your router. See docs/internet-access.md.";

/// Reads the saved access code, creating one if missing or `regenerate` is set.
pub fn load_code(regenerate: bool) -> Result<String> {
    let path = paths::config_dir()?.join("access-code.txt");
    if !regenerate && let Ok(code) = std::fs::read_to_string(&path) {
        let code = code.trim().to_string();
        if auth::normalize_code(&code).len() == auth::CODE_LEN {
            return Ok(code);
        }
    }
    let code = auth::generate_code();
    std::fs::write(&path, &code).context("saving access code")?;
    Ok(code)
}

fn main() {
    platform::attach_console();
    tracing_subscriber::fmt().with_target(false).init();
    let args = Args::parse();
    let gui = !args.headless && !args.list_displays && args.relay.is_none();
    if let Err(e) = run(args) {
        if gui {
            platform::error_box("TideDesk Host", &format!("{e:#}"));
        } else {
            eprintln!("error: {e:#}");
        }
        std::process::exit(1);
    }
}

fn run(args: Args) -> Result<()> {
    if args.relay.is_some() {
        eprintln!("{RELAY_NOTICE}");
        std::process::exit(2);
    }

    platform::enable_dpi_awareness();

    if args.list_displays {
        for d in capture::list_displays()? {
            println!(
                "{}: {} {}x{} at ({}, {}){}",
                d.index,
                d.name,
                d.rect.width,
                d.rect.height,
                d.rect.left,
                d.rect.top,
                if d.primary { " [primary]" } else { "" }
            );
        }
        return Ok(());
    }

    let config = config::HostConfig::load();
    let listen = args
        .listen
        .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], config.port)));
    let identity = HostIdentity::load_or_create(&paths::config_dir()?)?;
    let host_name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "tidedesk-host".into());
    let state = Arc::new(session::HostState {
        host_name: host_name.clone(),
        code: Mutex::new(load_code(args.new_code)?),
        video: Mutex::new(video::VideoSettings {
            display: args.display.unwrap_or(config.display),
            fps: args.fps.unwrap_or(config.fps),
            bitrate_bps: args.bitrate.unwrap_or(config.bitrate_kbps) * 1000,
            stats: args.stats,
        }),
        audio: AtomicBool::new(config.share_audio && !args.no_audio),
        clipboard: AtomicBool::new(config.allow_clipboard),
        mouse: AtomicBool::new(config.allow_mouse),
        accepting: AtomicBool::new(true),
        throttle: Mutex::new(auth::Throttle::default()),
        busy: AtomicBool::new(false),
        viewer: Mutex::new(None),
        expected_viewer: Mutex::new(None),
        on_change: Mutex::new(None),
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let (endpoint, agent) = {
        let _guard = runtime.enter();
        let (socket, side_channel) =
            SharedSocket::bind(listen).with_context(|| format!("listening on {listen}"))?;
        let agent = Agent::spawn(socket.clone(), side_channel)?;
        (net::server_endpoint_on(socket, &identity)?, agent)
    };
    if config.discover_public_address {
        agent.start_refresh(config.effective_stun_servers(), STUN_REFRESH);
    }
    let mut agent_status = agent.status();
    let repaint = state.clone();
    runtime.spawn(async move {
        while agent_status.changed().await.is_ok() {
            repaint.changed();
        }
    });
    runtime.spawn(accept_loop(endpoint, state.clone()));

    if !args.headless {
        return gui::run(gui::HostInfo {
            state,
            agent,
            runtime: runtime.handle().clone(),
            start_hidden: args.tray || config.start_in_tray,
            config,
            fingerprint: identity.fingerprint(),
            port: listen.port(),
        });
    }

    // A first answer usually takes a fraction of a second.
    let internet = runtime.block_on(async {
        let mut status = agent.status();
        let looked_up = status.wait_for(|s| s.public != PublicStatus::Discovering);
        let _ = tokio::time::timeout(Duration::from_secs(5), looked_up).await;
        agent.public()
    });
    let code = state.code.lock().unwrap().clone();
    println!();
    println!(
        "  TideDesk host \"{host_name}\" is listening on UDP {}",
        listen
    );
    println!("  Access code:  {code}");
    println!("  Fingerprint:  {}", identity.fingerprint());
    println!("  Internet address: {internet}");
    println!();
    println!("  Connect with: tidedesk-view <this-pc-address> --code {code}");
    println!();
    runtime.block_on(std::future::pending::<()>());
    Ok(())
}

async fn accept_loop(endpoint: quinn::Endpoint, state: Arc<session::HostState>) {
    while let Some(incoming) = net::accept_validated(&endpoint).await {
        let state = state.clone();
        tokio::spawn(async move {
            let remote = incoming.remote_address();
            let result = async {
                let conn = incoming.await?;
                if state.video.lock().unwrap().stats {
                    tokio::spawn(stats::log_path(conn.clone(), "viewer path (direct)"));
                }
                session::run(conn, state).await
            }
            .await;
            if let Err(e) = result {
                tracing::warn!("session with {remote} ended: {e:#}");
            }
        });
    }
}
