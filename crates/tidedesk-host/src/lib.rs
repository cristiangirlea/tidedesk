//! The TideDesk host: shares this machine's screen and audio. Runs as the
//! one window's Share tab, or alone as `tidedesk host …`.

mod audio;
mod capture;
pub mod codes;
pub mod config;
pub mod gui;
mod icon;
mod input;
mod internet;
pub mod limits;
mod platform;
pub mod saved_password;
pub mod session;
pub mod session_log;
mod tray;
pub mod trusted;
mod video;

use std::ffi::OsString;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser};
use tidedesk_core::identity::HostIdentity;
use tidedesk_core::nat::signal::{Credentials, RendezvousStatus};
use tidedesk_core::nat::stun::STUN_REFRESH;
use tidedesk_core::nat::{Agent, PublicStatus, SharedSocket, candidates};
use tidedesk_core::policy::{self, Choice, Policy, Services};
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

    /// Target video bitrate in kbit/s [default: set by the screen's size,
    /// 4000-20000, unless Settings chooses one].
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

    /// Set the password for viewers that have met this host before, read
    /// from standard input, and exit.
    #[arg(long, conflicts_with = "remove_password")]
    set_password: bool,

    /// Remove the saved password and exit.
    #[arg(long)]
    remove_password: bool,

    /// Trust the viewer with this fingerprint (`tidedesk view --my-fingerprint`
    /// shows it): it comes back without the access code. Then exit.
    #[arg(long, value_name = "FINGERPRINT")]
    trust_viewer: Option<String>,

    /// Stop trusting the viewer with this fingerprint, and exit.
    #[arg(long, value_name = "FINGERPRINT")]
    untrust_viewer: Option<String>,

    /// List the trusted viewers and exit.
    #[arg(long)]
    trusted_viewers: bool,

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
/// else the `--rendezvous` argument (empty meaning none), else the setting;
/// and then what an administrator allows (see [`tidedesk_core::policy`]),
/// which wins over all three.
fn rendezvous_choice(
    argument: Option<&str>,
    off: bool,
    configured: Option<&str>,
    allowed: &Services,
) -> Option<String> {
    let chosen = if off {
        None
    } else {
        match argument.map(str::trim) {
            Some(service) => Some(service).filter(|s| !s.is_empty()),
            None => configured,
        }
    };
    let choice = allowed.choose(chosen);
    if let Choice::Overruled(service) = &choice {
        tracing::info!(
            "connection service {chosen:?} not allowed by this computer's administrator: using \
             {service:?}"
        );
    }
    choice.service().map(str::to_string)
}

/// Whether the host asks the router to open its port: the setting (or the
/// administrator's), and only while it registers with a service.
pub(crate) fn maps_port(config: &config::HostConfig, managed: &Policy) -> bool {
    Policy::bool_or(managed.port_mapping, config.port_mapping)
        && managed
            .services
            .choose(config.rendezvous_service())
            .service()
            .is_some()
}

/// Whether the person at this computer may stop sharing or quit: unless the
/// administrator said no (`AllowStopSharing = 0`). A session in progress
/// shows in the host's window, and in the tray icon's tooltip; it can be
/// ended from the window or with the tray's "Disconnect viewer".
pub fn may_stop_sharing(managed: &Policy) -> bool {
    Policy::bool_or(managed.stop_sharing, true)
}

/// The service to register with now, as [`rendezvous_choice`] with the
/// administrator's settings in force.
pub(crate) fn rendezvous_now(
    argument: Option<&str>,
    off: bool,
    configured: Option<&str>,
) -> Option<String> {
    rendezvous_choice(argument, off, configured, &policy::current().services)
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

/// What this host registers with a connection service: its credentials and
/// its local addresses sealed with the access `code`, so a viewer on the
/// same network that knows the code reaches it directly when the router
/// cannot loop a path back (see `tidedesk_core::nat::candidates`). The
/// addresses are those the window lists, real adapters first.
pub(crate) fn registration_credentials(
    identity: &Credentials,
    code: &str,
    port: u16,
) -> Arc<Credentials> {
    let addresses: Vec<SocketAddrV4> = gui::local_addresses(port)
        .iter()
        .map(|address| SocketAddrV4::new(address.ip, port))
        .collect();
    Arc::new(Credentials {
        candidates: candidates::seal(code, identity.device_id, &addresses),
        ..identity.clone()
    })
}

pub use gui::{HistoryView, HostApp, HostInfo, SettingsGroup};
pub use platform::{attach_console, error_box, open_link};

/// The screen's usable area in pixels, when Windows says it.
pub fn work_area() -> Option<(f32, f32)> {
    platform::work_area()
}

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
    /// No window shows a new code: keep the one there is.
    pub headless: bool,
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
            headless: args.headless,
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
    let code = load_code(options.new_code)?;
    let state = Arc::new(session::HostState {
        host_name,
        codes: Mutex::new(codes::Codes::new(code.clone())),
        password: Mutex::new(saved_password::load()),
        trusted: Mutex::new(trusted::TrustedViewers::load()),
        new_code_after_session: AtomicBool::new(config.new_code_after_session && !options.headless),
        registration: Mutex::new(None),
        register: Mutex::new(None),
        code_note: Mutex::new(None),
        video: Mutex::new(video::VideoSettings {
            display: options.display.unwrap_or(config.display),
            fps: options.fps.unwrap_or(config.fps),
            bitrate_bps: options
                .bitrate
                .map(|kbps| kbps * 1000)
                .or(config.bitrate_bps()),
            stats: options.stats,
        }),
        audio: AtomicBool::new(config.share_audio && !options.no_audio),
        clipboard: AtomicBool::new(config.allow_clipboard),
        mouse: AtomicBool::new(config.allow_mouse),
        files: AtomicBool::new(config.allow_files),
        files_note: Mutex::new(None),
        outgoing: Mutex::new(None),
        chat: Mutex::new(Vec::new()),
        chat_out: Mutex::new(None),
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
    let (endpoint, agent, listen) = {
        let _guard = runtime.enter();
        let (socket, side_channel) =
            SharedSocket::bind(listen).with_context(|| format!("listening on {listen}"))?;
        // With port 0 asked for, the port the system chose.
        let bound = socket.local_addr().unwrap_or(listen);
        let agent = Agent::spawn(socket.clone(), side_channel)?;
        (net::server_endpoint_on(socket, &identity)?, agent, bound)
    };
    if config.discover_public_address {
        agent.start_refresh(config.effective_stun_servers(), STUN_REFRESH);
    }
    let credentials = identity.rendezvous_credentials();
    let rendezvous = rendezvous_now(
        options.rendezvous.as_deref(),
        options.no_rendezvous,
        config.rendezvous_service(),
    );
    if let Some(service) = &rendezvous {
        let sealed = registration_credentials(&credentials, &code, listen.port());
        agent.start_rendezvous(service.clone(), sealed);
    }
    // Local addresses are sealed with the code: a new code registers again.
    *state.registration.lock().unwrap() = rendezvous.clone();
    {
        let (agent, credentials, port) = (agent.clone(), credentials.clone(), listen.port());
        *state.register.lock().unwrap() = Some(Box::new(move |service: &str, code: &str| {
            let sealed = registration_credentials(&credentials, code, port);
            agent.start_rendezvous(service.to_string(), sealed);
        }));
    }
    if maps_port(&config, &policy::current()) {
        agent.start_port_mapping(listen.port());
    }
    // Needs no service: viewers on this network ask the network itself.
    if Policy::bool_or(policy::current().lan_discovery, config.lan_discovery) {
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
        identity: credentials,
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

    if args.trust_viewer.is_some() || args.untrust_viewer.is_some() || args.trusted_viewers {
        if let Err(e) = trusted_from_console(&args) {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if args.set_password || args.remove_password {
        if let Err(e) = password_from_console(args.set_password) {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
        return Ok(());
    }
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
    let code = state.codes.lock().unwrap().current().to_string();
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
    println!("  Terms of use: {}", tidedesk_core::TERMS_URL);
    println!();
    runtime.block_on(std::future::pending::<()>());
    Ok(())
}

/// `--trust-viewer`, `--untrust-viewer` and `--trusted-viewers`: for a host
/// without a window.
fn trusted_from_console(args: &Args) -> Result<()> {
    let mut trusted = trusted::TrustedViewers::load();
    if let Some(fingerprint) = &args.trust_viewer {
        if tidedesk_core::identity::normalize_fingerprint(fingerprint).len() != 64 {
            bail!("a viewer's fingerprint has 64 hexadecimal digits");
        }
        trusted.add(fingerprint, "added on the command line", &trusted::today());
        trusted.save()?;
        println!("Trusted. Restart a running host to apply it.");
    }
    if let Some(fingerprint) = &args.untrust_viewer {
        if !trusted.remove(fingerprint) {
            bail!("no trusted viewer has that fingerprint");
        }
        trusted.save()?;
        println!("No longer trusted. Restart a running host to apply it.");
    }
    if args.trusted_viewers {
        for viewer in trusted.list() {
            println!(
                "{}  {}  since {}",
                viewer.fingerprint, viewer.name, viewer.since
            );
        }
        if trusted.list().is_empty() {
            println!("No trusted viewers.");
        }
    }
    Ok(())
}

/// `--set-password` and `--remove-password`: for a host without a window.
/// The password is read from standard input, twice when typed.
fn password_from_console(set: bool) -> Result<()> {
    if !set {
        saved_password::remove()?;
        println!("The saved password is removed; viewers need the access code.");
        return Ok(());
    }
    use std::io::IsTerminal;
    let typed = std::io::stdin().is_terminal();
    let read = |prompt: &str| -> Result<String> {
        if typed {
            eprint!("{prompt}");
        }
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        Ok(line.trim_end_matches(['\r', '\n']).to_string())
    };
    let password = read("New password: ")?;
    if typed && read("The same again: ")? != password {
        bail!("the two passwords differ; nothing was saved");
    }
    let fingerprint = HostIdentity::load_or_create(&paths::config_dir()?)?.fingerprint();
    saved_password::save(&password, &fingerprint)?;
    println!(
        "Password saved. Viewers that have met this host before can use it instead of the \
         access code; restart a running host to apply it."
    );
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
    use super::{
        HostIdentity, Policy, Services, candidates, maps_port, registration_credentials,
        rendezvous_choice,
    };

    #[test]
    fn registration_seals_this_computers_local_addresses_for_the_code() {
        let dir =
            std::env::temp_dir().join(format!("tidedesk-test-registration-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let identity = HostIdentity::load_or_create(&dir).unwrap();
        let base = identity.rendezvous_credentials();
        let code = "K7QM-3XPA-WZ";
        let credentials = registration_credentials(&base, code, 50000);
        assert_eq!(credentials.device_id, base.device_id);
        assert_eq!(credentials.cert_der, base.cert_der);
        match candidates::unseal(code, base.device_id, &credentials.candidates) {
            Some(addresses) => {
                assert!(!addresses.is_empty());
                assert!(addresses.iter().all(|a| a.port() == 50000), "{addresses:?}");
            }
            // A computer with no local network address seals nothing.
            None => assert!(credentials.candidates.is_empty()),
        }
        assert_eq!(
            candidates::unseal("K7QM-3XPA-WY", base.device_id, &credentials.candidates),
            None
        );
    }

    #[test]
    fn the_command_line_decides_the_service_before_the_setting() {
        let any = &Services::Any;
        let saved = Some("rv.saved:47900");
        assert_eq!(rendezvous_choice(None, false, saved, any).as_deref(), saved);
        assert_eq!(rendezvous_choice(None, false, None, any), None);
        assert_eq!(
            rendezvous_choice(Some(" rv.arg "), false, saved, any).as_deref(),
            Some("rv.arg")
        );
        assert_eq!(rendezvous_choice(Some(""), false, saved, any), None);
        assert_eq!(rendezvous_choice(None, true, saved, any), None);
    }

    #[test]
    fn the_administrators_service_wins_over_the_command_line_and_the_setting() {
        let only = &Services::Only("rv.company:47900".into());
        let saved = Some("rv.saved:47900");
        for (argument, off) in [
            (None, false),
            (Some("rv.arg"), false),
            (Some(""), false),
            (None, true),
        ] {
            assert_eq!(
                rendezvous_choice(argument, off, saved, only).as_deref(),
                Some("rv.company:47900"),
                "{argument:?} {off}"
            );
        }
        let none = &Services::Off;
        assert_eq!(rendezvous_choice(Some("rv.arg"), false, saved, none), None);
        let list = &Services::OneOf(vec!["rv.a:47900".into(), "rv.b:47900".into()]);
        assert_eq!(
            rendezvous_choice(Some("rv.b"), false, saved, list).as_deref(),
            Some("rv.b"),
            "an allowed choice stands"
        );
        assert_eq!(
            rendezvous_choice(None, false, saved, list).as_deref(),
            Some("rv.a:47900")
        );
    }

    #[test]
    fn the_administrator_decides_router_port_mapping() {
        let config = crate::config::HostConfig::default();
        assert!(maps_port(&config, &Policy::default()));
        let off = Policy {
            port_mapping: Some(false),
            ..Policy::default()
        };
        assert!(!maps_port(&config, &off));
        let no_service = Policy {
            services: Services::Off,
            ..Policy::default()
        };
        assert!(
            !maps_port(&config, &no_service),
            "nothing to be reachable for"
        );
        let unregistered = crate::config::HostConfig {
            rendezvous: false,
            ..config
        };
        let forced = Policy {
            services: Services::Only("rv.company:47900".into()),
            port_mapping: Some(true),
            ..Policy::default()
        };
        assert!(
            maps_port(&unregistered, &forced),
            "the administrator's service registers"
        );
    }

    #[test]
    fn the_administrator_may_keep_sharing_on() {
        use super::may_stop_sharing;
        assert!(may_stop_sharing(&Policy::default()));
        let kept_on = Policy {
            stop_sharing: Some(false),
            ..Policy::default()
        };
        assert!(!may_stop_sharing(&kept_on));
    }
}
