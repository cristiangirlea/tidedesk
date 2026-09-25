//! The TideDesk host: shares this machine's screen and audio. Runs as
//! `tidedesk host …` inside the one program, or as `tidedesk-host.exe`.

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

use std::ffi::OsString;
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser};
use tidedesk_core::identity::HostIdentity;
use tidedesk_core::nat::signal::RendezvousStatus;
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

    /// Rendezvous service to register this host's device ID with, so viewers
    /// on other networks can connect by ID [default: the one in Settings,
    /// else TideDesk's own].
    #[arg(long, value_name = "HOST:PORT", conflicts_with = "no_rendezvous")]
    rendezvous: Option<String>,

    /// Do not register the device ID with any rendezvous service.
    #[arg(long)]
    no_rendezvous: bool,

    /// Log frame rate, bitrate and encode time every two seconds.
    #[arg(long)]
    stats: bool,

    /// Replace the saved access code with a new random one.
    #[arg(long)]
    new_code: bool,

    /// Print the available displays and exit.
    #[arg(long)]
    list_displays: bool,

    /// Not supported: TideDesk never relays sessions. Prints how to connect directly.
    #[arg(long, value_name = "URL")]
    relay: Option<String>,
}

const RELAY_NOTICE: &str = "TideDesk never relays sessions: every connection runs directly \
between the two computers.\n\
To reach this host from another network, open a path to the viewer under \"Viewer on another \
network\" in the host window, or use a VPN such as Tailscale. See docs/internet-access.md.";

/// The rendezvous service to register with: none with `--no-rendezvous`,
/// else the `--rendezvous` argument (empty meaning none), else the setting.
fn rendezvous_choice(
    argument: Option<&str>,
    off: bool,
    configured: Option<&str>,
) -> Option<String> {
    if off {
        return None;
    }
    match argument.map(str::trim) {
        Some(service) => Some(service).filter(|s| !s.is_empty()),
        None => configured,
    }
    .map(str::to_string)
}

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

pub use platform::attach_console;

static SELF_PREFIX: OnceLock<&'static [&'static str]> = OnceLock::new();

/// The words that start this program's own command line when it launches
/// itself: `["host"]` inside `tidedesk.exe`, none as `tidedesk-host.exe`.
pub fn self_prefix() -> &'static [&'static str] {
    SELF_PREFIX.get().copied().unwrap_or(&[])
}

/// Runs the host with a command line (`program` names it in usage text).
/// Exits the process on failure.
pub fn main(program: &str, argv: Vec<OsString>, self_prefix: &'static [&'static str]) {
    let _ = SELF_PREFIX.set(self_prefix);
    platform::attach_console();
    tracing_subscriber::fmt().with_target(false).init();
    let matches = Args::command().bin_name(program).get_matches_from(argv);
    let args = Args::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
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
    let rendezvous = rendezvous_choice(
        args.rendezvous.as_deref(),
        args.no_rendezvous,
        config.rendezvous_service(),
    );
    if let Some(service) = rendezvous {
        agent.start_rendezvous(service, identity.rendezvous_credentials());
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
            identity: identity.rendezvous_credentials(),
            runtime: runtime.handle().clone(),
            start_hidden: args.tray || config.start_in_tray,
            config,
            fingerprint: identity.fingerprint(),
            port: listen.port(),
        });
    }

    // A first answer usually takes a fraction of a second.
    let (internet, rendezvous) = runtime.block_on(async {
        let mut status = agent.status();
        let settled = status.wait_for(|s| {
            s.public != PublicStatus::Discovering && s.rendezvous != RendezvousStatus::Connecting
        });
        let _ = tokio::time::timeout(Duration::from_secs(5), settled).await;
        (agent.public(), agent.rendezvous())
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
    println!("  Device ID:    {}", identity.device_id());
    println!("  Rendezvous:   {rendezvous}");
    println!();
    println!("  Connect with: tidedesk view <this-pc-address> --code {code}");
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

#[cfg(test)]
mod tests {
    use super::rendezvous_choice;

    #[test]
    fn the_command_line_decides_the_service_before_the_setting() {
        let saved = Some("rv.saved:47900");
        assert_eq!(rendezvous_choice(None, false, saved).as_deref(), saved);
        assert_eq!(rendezvous_choice(None, false, None), None);
        assert_eq!(
            rendezvous_choice(Some(" rv.arg "), false, saved).as_deref(),
            Some("rv.arg")
        );
        assert_eq!(rendezvous_choice(Some(""), false, saved), None);
        assert_eq!(rendezvous_choice(None, true, saved), None);
    }
}
