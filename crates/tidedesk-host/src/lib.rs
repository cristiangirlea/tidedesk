//! The TideDesk host: shares this machine's screen and audio. Runs as the
//! one window's Share tab, or alone as `tidedesk host …`.

mod audio;
mod capture;
pub mod config;
pub mod gui;
mod icon;
mod input;
mod internet;
mod platform;
pub mod session;
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

pub use gui::{HostApp, HostInfo};
pub use platform::{attach_console, error_box};

/// The window icon, for the one program's window.
pub fn window_icon() -> Arc<egui::IconData> {
    icon::egui_icon()
}

/// What sharing needs to start. The command line fills it in; the one
/// program uses the saved settings.
#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    pub listen: Option<SocketAddr>,
    pub display: Option<usize>,
    pub fps: Option<u32>,
    pub bitrate: Option<u32>,
    pub no_audio: bool,
    pub rendezvous: Option<String>,
    pub no_rendezvous: bool,
    pub stats: bool,
    pub new_code: bool,
    /// Start with the window hidden in the tray.
    pub tray: bool,
}

impl From<&Args> for StartOptions {
    fn from(args: &Args) -> Self {
        Self {
            listen: args.listen,
            display: args.display,
            fps: args.fps,
            bitrate: args.bitrate,
            no_audio: args.no_audio,
            rendezvous: args.rendezvous.clone(),
            no_rendezvous: args.no_rendezvous,
            stats: args.stats,
            new_code: args.new_code,
            tray: args.tray,
        }
    }
}

/// Sharing that has started: viewers can connect. `runtime` runs it and must
/// live as long as sharing should; a window or the console shows `info`.
pub struct Started {
    pub info: HostInfo,
    pub runtime: tokio::runtime::Runtime,
    pub device_id: String,
    pub listen: SocketAddr,
}

/// Starts sharing this computer: loads the settings and identity, binds the
/// socket, starts address discovery and rendezvous registration, and accepts
/// viewers. Nothing is shown yet.
pub fn start(options: &StartOptions) -> Result<Started> {
    platform::enable_dpi_awareness();
    // Earlier releases started tidedesk-host.exe at sign-in, no longer shipped.
    match platform::migrate_autostart() {
        Ok(true) => tracing::info!("the start-up entry now starts this program"),
        Ok(false) => {}
        Err(e) => tracing::warn!("could not move the start-up entry to this program: {e:#}"),
    }
    let config = config::HostConfig::load();
    let listen = options
        .listen
        .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], config.port)));
    let identity = HostIdentity::load_or_create(&paths::config_dir()?)?;
    let host_name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "tidedesk-host".into());
    let state = Arc::new(session::HostState {
        host_name,
        code: Mutex::new(load_code(options.new_code)?),
        video: Mutex::new(video::VideoSettings {
            display: options.display.unwrap_or(config.display),
            fps: options.fps.unwrap_or(config.fps),
            bitrate_bps: options.bitrate.unwrap_or(config.bitrate_kbps) * 1000,
            stats: options.stats,
        }),
        audio: AtomicBool::new(config.share_audio && !options.no_audio),
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
        options.rendezvous.as_deref(),
        options.no_rendezvous,
        config.rendezvous_service(),
    );
    if let Some(service) = rendezvous {
        agent.start_rendezvous(service, identity.rendezvous_credentials());
    }
    // Needs no service: viewers on this network ask the network itself.
    if config.lan_discovery {
        agent.start_lan_discovery(identity.device_id());
    }
    let mut agent_status = agent.status();
    let repaint = state.clone();
    runtime.spawn(async move {
        while agent_status.changed().await.is_ok() {
            repaint.changed();
        }
    });
    runtime.spawn(accept_loop(endpoint, state.clone()));

    let info = gui::HostInfo {
        state,
        agent,
        identity: identity.rendezvous_credentials(),
        runtime: runtime.handle().clone(),
        start_hidden: options.tray || config.start_in_tray,
        config,
        fingerprint: identity.fingerprint(),
        port: listen.port(),
    };
    Ok(Started {
        info,
        runtime,
        device_id: identity.device_id().to_string(),
        listen,
    })
}

static SELF_PREFIX: OnceLock<&'static [&'static str]> = OnceLock::new();

/// The words that start this program's own command line when it launches
/// itself: `["host"]` as `tidedesk host`, none in the one window.
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

    let Started {
        info,
        runtime,
        device_id,
        listen,
    } = start(&StartOptions::from(&args))?;
    if !args.headless {
        let result = gui::run(info);
        drop(runtime);
        return result;
    }

    let (state, agent) = (info.state.clone(), info.agent.clone());
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
        "  TideDesk host \"{}\" is listening on UDP {listen}",
        state.host_name
    );
    println!("  Access code:  {code}");
    println!("  Fingerprint:  {}", info.fingerprint);
    println!("  Internet address: {internet}");
    println!("  Device ID:    {device_id}");
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
