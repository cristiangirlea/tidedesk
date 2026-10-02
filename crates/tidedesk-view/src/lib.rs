//! The TideDesk viewer: shows a host's screen, plays its audio and forwards
//! keyboard and mouse. Started without a host, it opens a connect window.
//! Runs as `tidedesk view …` inside the one program.

mod app;
mod chat_window;
mod child;
mod computers;
mod connect;
mod control;
mod icon;
mod keys;
pub mod launcher;
mod layout;
mod playback;
mod pointer;
pub mod settings;
mod stream;
mod window_placement;

use std::ffi::OsString;
use std::io::Write;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use clap::{CommandFactory, FromArgMatches, Parser};
use tidedesk_core::identity::{KnownHosts, PinStatus};
use tidedesk_core::nat::signal::DEFAULT_RENDEZVOUS;
use tidedesk_core::paths;
use tidedesk_core::policy::{self, Choice, Policy, Services};
use winit::event_loop::{EventLoop, EventLoopProxy};

use child::ChildLine;
use stream::{Notify, Picture, UiEvent};

/// Files the host sends, each on its own stream, saved in Downloads\TideDesk
/// unless Viewer Settings refuse them; the host hears where each went.
async fn receive_files(
    conn: quinn::Connection,
    ui: Arc<dyn Notify>,
    control: tokio::sync::mpsc::UnboundedSender<tidedesk_core::protocol::ClientMessage>,
) {
    use tidedesk_core::files;
    use tidedesk_core::protocol::ClientMessage;
    while let Ok(mut stream) = conn.accept_uni().await {
        let (ui, control) = (ui.clone(), control.clone());
        tokio::spawn(async move {
            let Ok(header) = files::read_header(&mut stream).await else {
                return;
            };
            let allowed = settings::ViewerSettings::load().map_or(true, |s| s.allow_files);
            let saved = if allowed {
                match files::downloads() {
                    Ok(dir) => files::save(&mut stream, &header, &dir).await,
                    Err(e) => Err(e),
                }
            } else {
                let _ = stream.stop(1u32.into());
                Err(anyhow!("the viewer does not accept files"))
            };
            let reply = match saved {
                Ok(path) => {
                    let name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    ui.notify(UiEvent::Notice(format!(
                        "Received {name} from the host, in Downloads\\TideDesk"
                    )));
                    ClientMessage::FileSaved { name, error: None }
                }
                Err(e) => {
                    tracing::warn!("a file from the host was not saved: {e:#}");
                    ClientMessage::FileSaved {
                        name: header.name,
                        error: Some(format!("{e:#}")),
                    }
                }
            };
            let _ = control.send(reply);
        });
    }
}

/// Why this computer cannot connect, or a session ended, when it is a
/// company computer without a licence whose hours are used.
pub const OWN_HOURS_USED: &str = "this computer is a company computer without a TideDesk licence, and this month's hours are used";

#[derive(Parser, Debug)]
#[command(version, about = "Connect to a TideDesk host.")]
struct Args {
    /// Open viewer settings without connecting.
    #[arg(long)]
    settings: bool,
    /// With --settings: the centre to open the window on, `x,y` in points.
    #[arg(long, hide = true, requires = "settings")]
    settings_near: Option<String>,
    /// A session's chat window, fed on standard input.
    #[arg(long, hide = true)]
    chat: Option<String>,
    /// Host to connect to: name or IP, optionally with :port. Omit to open the
    /// connect window.
    host: Option<String>,

    /// Access code shown by the host, or its saved password (prompted for if
    /// omitted; empty for a host that trusts this viewer).
    #[arg(long, env = "TIDEDESK_CODE", hide_env_values = true)]
    code: Option<String>,

    /// Do not play the host's audio.
    #[arg(long)]
    no_audio: bool,

    /// Print this viewer's fingerprint, which a host trusts it by
    /// (`tidedesk host --trust-viewer`), and exit.
    #[arg(long)]
    my_fingerprint: bool,

    /// Log frame rate, bitrate and decode time every two seconds.
    #[arg(long)]
    stats: bool,

    /// Expected host fingerprint, to verify the very first connection.
    #[arg(long)]
    fingerprint: Option<String>,

    /// Trust the host even though its fingerprint changed since last time.
    #[arg(long)]
    accept_new_fingerprint: bool,

    /// Reach a host on another network: HOST is the internet address its
    /// window shows. The person at the host then types this computer's
    /// internet address (printed here) and presses Open. No relay is used.
    #[arg(long)]
    internet: bool,

    /// Rendezvous service for connecting by device ID (HOST is then
    /// TD-XXXX-XXXX-XXXX-XXXX) [default: the one in Viewer Settings, else
    /// TideDesk's own].
    #[arg(long, value_name = "HOST:PORT")]
    rendezvous: Option<String>,

    /// STUN servers for --internet, comma-separated [default: the built-in ones].
    #[arg(long, value_delimiter = ',', requires = "internet")]
    stun: Vec<String>,

    /// Not supported: TideDesk never relays sessions. Prints how to connect directly.
    #[arg(long, value_name = "URL")]
    relay: Option<String>,

    /// For tests: take commands on standard input, one to a line, and answer
    /// them on standard output (see docs/test-control.md).
    #[arg(long, hide = true)]
    control: bool,
}

struct ProxyNotify(Mutex<EventLoopProxy<UiEvent>>);

impl Notify for ProxyNotify {
    fn notify(&self, event: UiEvent) {
        let _ = self.0.lock().unwrap().send_event(event);
    }
}

/// The rendezvous service for a device-ID connection: the `--rendezvous`
/// argument, else the one saved in Viewer Settings, else TideDesk's own.
pub(crate) fn rendezvous_service(argument: Option<&str>, saved: Option<&str>) -> String {
    argument
        .into_iter()
        .chain(saved)
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or(DEFAULT_RENDEZVOUS)
        .to_string()
}

/// What a viewer says when the administrator turned connection services off.
pub(crate) const NO_SERVICE: &str = "This computer's administrator turned connection services     off: connect to a computer on this network by its address.";

/// What a viewer says when the administrator allows device IDs only.
pub(crate) const DEVICE_IDS_ONLY: &str = "This computer's administrator allows connecting by     device ID only: type the computer's device ID (it looks like TD-1A2B-3C4D-5E6F-7A8B).";

/// The service to look a device ID up with: [`rendezvous_service`], then
/// what the administrator allows (see [`tidedesk_core::policy`]).
pub(crate) fn lookup_service(
    argument: Option<&str>,
    saved: Option<&str>,
    allowed: &Services,
) -> Result<String, &'static str> {
    let chosen = rendezvous_service(argument, saved);
    match allowed.choose(Some(&chosen)) {
        Choice::Chosen(Some(service)) => Ok(service),
        Choice::Overruled(Some(service)) => {
            tracing::info!(
                "connection service {chosen} not allowed by this computer's administrator:                  using {service}"
            );
            Ok(service)
        }
        Choice::Chosen(None) | Choice::Overruled(None) => Err(NO_SERVICE),
    }
}

/// Whether a connection to a typed address (not a device ID) is allowed.
pub(crate) fn typed_allowed(policy: &Policy, device_id: bool) -> Result<(), &'static str> {
    if device_id || policy.typed_addresses != Some(false) {
        Ok(())
    } else {
        Err(DEVICE_IDS_ONLY)
    }
}

/// Shows how opening an internet path goes, on the console.
fn print_progress(step: connect::Progress) {
    match step {
        connect::Progress::Status(text) => eprintln!("{text}"),
        connect::Progress::ViewerAddress(me) => eprintln!(
            "\n  This computer's internet address: {me}\n  \
             Give it to the person at the host: they type it under \"Viewer on another network\" \
             and press Open.\n"
        ),
        connect::Progress::PathOpen(path) => eprintln!("Path open to {}.", path.peer),
    }
}

/// The same steps as lines for the launcher that started this session.
fn report_to_launcher(step: connect::Progress) {
    let line = match step {
        connect::Progress::Status(text) => ChildLine::Status(text),
        connect::Progress::ViewerAddress(me) => ChildLine::ViewerAddress(me),
        connect::Progress::PathOpen(path) => {
            ChildLine::Status(format!("Path open to {}. Connecting…", path.peer))
        }
    };
    eprintln!("{line}");
}

/// Whether the session is up. Guarded by a lock, so a `cancel` either ends the
/// process before the session counts as up, or is ignored after: a live
/// session is only ended through its window, which closes it cleanly.
static CONNECTED: Mutex<bool> = Mutex::new(false);

/// Reads the launcher's answers from stdin. `cancel` ends a session that is
/// still connecting, whatever it is doing; other answers are passed on.
fn launcher_answers() -> Receiver<String> {
    let (answers, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if line.trim() == child::CANCEL {
                let connected = CONNECTED.lock().unwrap();
                if *connected {
                    break;
                }
                eprintln!("{}", ChildLine::Disconnected(child::CANCELLED.into()));
                std::process::exit(0); // with the lock held: never half connected
            }
            if answers.send(line).is_err() {
                break;
            }
        }
    });
    received
}

/// For an internet path the probe runs here, in the process that owns the
/// path: asks the launcher when the host's identity is new or has changed,
/// and pins it once trusted. `false` when the launcher did not say trust.
async fn confirm_with_launcher(
    dialer: &connect::Dialer,
    answers: Receiver<String>,
) -> Result<bool> {
    let probe = dialer.probe().await?;
    if probe.status == PinStatus::Trusted {
        return Ok(true);
    }
    let question = ChildLine::Fingerprint {
        address: probe.address.clone(),
        fingerprint: probe.fingerprint.clone(),
        status: probe.status.clone(),
    };
    eprintln!("{question}");
    // The punched path's keepalives hold it open while the user decides.
    let answer = tokio::task::spawn_blocking(move || answers.recv()).await?;
    if answer.is_ok_and(|a| a.trim() == child::TRUST) {
        KnownHosts::load(&paths::config_dir()?)?.pin(&probe.address, &probe.fingerprint)?;
        return Ok(true);
    }
    Ok(false)
}

fn prompt_code() -> Result<String> {
    print!("Access code or password (none if the host trusts this viewer): ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

static SELF_PREFIX: OnceLock<&'static [&'static str]> = OnceLock::new();

/// The words that start this program's own command line when it launches
/// itself: `["view"]`, set by `tidedesk view` and by the one window.
pub fn self_prefix() -> &'static [&'static str] {
    SELF_PREFIX.get().copied().unwrap_or(&[])
}

/// Tells the viewer how this program launches itself (see [`self_prefix`]);
/// the one program calls it before opening its window.
pub fn set_self_prefix(prefix: &'static [&'static str]) {
    let _ = SELF_PREFIX.set(prefix);
}

/// The window icon, for the one program's window.
pub fn window_icon() -> std::sync::Arc<egui::IconData> {
    icon::egui_icon()
}

/// Runs the viewer with a command line (`program` names it in usage text).
/// Exits the process on failure. The program that calls it has borrowed the
/// terminal's console to print to.
pub fn main(program: &str, argv: Vec<OsString>, self_prefix: &'static [&'static str]) {
    set_self_prefix(self_prefix);
    let log = tracing_subscriber::fmt().with_target(false);
    // With test control, standard output is for its answers alone.
    if argv.iter().any(|word| word == "--control") {
        log.with_writer(std::io::stderr).init();
    } else {
        log.init();
    }
    if let Err(e) = run(program, argv) {
        eprintln!("{}", ChildLine::Error(format!("{e:#}")));
        std::process::exit(1);
    }
}

fn run(program: &str, argv: Vec<OsString>) -> Result<()> {
    let matches = Args::command().bin_name(program).get_matches_from(argv);
    let args = Args::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    if args.settings {
        return settings::run(args.settings_near.as_deref().and_then(settings::parse_near));
    }
    if let Some(title) = &args.chat {
        return chat_window::run(title);
    }
    // Shown to every host, which may trust it.
    let identity = paths::config_dir()
        .and_then(|dir| tidedesk_core::identity::ViewerIdentity::load_or_create(&dir));
    match identity {
        Ok(identity) => {
            if args.my_fingerprint {
                println!("{}", identity.fingerprint());
                return Ok(());
            }
            tidedesk_core::net::set_viewer_identity(identity);
        }
        Err(e) if args.my_fingerprint => return Err(e),
        Err(e) => tracing::warn!("no identity for this viewer, hosts cannot trust it: {e:#}"),
    }
    if args.relay.is_some() {
        eprintln!(
            "TideDesk never relays sessions: every connection runs directly between the two \
             computers.\n\
             To reach a host on another network, connect with --internet to the internet address \
             its window shows, or use a VPN such as Tailscale. See docs/internet-access.md."
        );
        std::process::exit(2);
    }
    let Some(host) = args.host.clone() else {
        return launcher::run();
    };
    if args.rendezvous.is_some() && connect::parse_device_id(&host).is_none() {
        bail!(
            "--rendezvous is for connecting by device ID, and {host} is not one (device IDs \
             look like TD-1A2B-3C4D-5E6F-7A8B)"
        );
    }
    let managed = policy::current();
    let device_id = connect::parse_device_id(&host).is_some();
    typed_allowed(&managed, device_id).map_err(|why| anyhow!(why))?;
    let route = if device_id {
        if args.internet {
            bail!("a device ID is found through a rendezvous service; leave out --internet");
        }
        let saved = settings::ViewerSettings::load()
            .ok()
            .map(|s| s.rendezvous_server);
        let service = lookup_service(
            args.rendezvous.as_deref(),
            saved.as_deref(),
            &managed.services,
        )
        .map_err(|why| anyhow!(why))?;
        connect::Route::Rendezvous { service }
    } else if args.internet {
        connect::parse_internet_host(&host)?;
        if args.stun.is_empty() {
            connect::Route::internet()
        } else {
            connect::Route::Internet {
                stun_servers: args.stun.clone(),
            }
        }
    } else {
        connect::Route::Direct
    };
    // This computer may be a company computer without a licence.
    let own_company = tidedesk_core::company::allowance();
    if own_company == Some(tidedesk_core::company::Allowance::Used) {
        bail!("{OWN_HOURS_USED}");
    }
    let code = match args.code.clone() {
        Some(c) => c,
        None => prompt_code()?,
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;

    let opts = connect::ConnectOptions {
        host,
        code,
        want_audio: !args.no_audio,
        expected_fingerprint: args.fingerprint.clone(),
        accept_new_fingerprint: args.accept_new_fingerprint,
        route,
    };
    let launcher = std::env::var_os(child::LAUNCHER_ENV).is_some();
    let answers = launcher.then(launcher_answers);
    let session = runtime.block_on(async {
        let report = move |step| {
            if launcher {
                report_to_launcher(step)
            } else {
                print_progress(step)
            }
        };
        let dialer =
            connect::Dialer::new(&opts.host, &opts.route, Some(&opts.code), report).await?;
        // The launcher probes directly reachable hosts itself.
        if let Some(answers) = answers
            && opts.route != connect::Route::Direct
            && !confirm_with_launcher(&dialer, answers).await?
        {
            return Ok(None);
        }
        dialer.connect(&opts).await.map(Some)
    })?;
    let Some(session) = session else {
        eprintln!("{}", ChildLine::Disconnected(child::CANCELLED.into()));
        return Ok(());
    };
    if launcher {
        *CONNECTED.lock().unwrap() = true;
        eprintln!("{}", ChildLine::Connected(session.host_name.clone()));
    }
    tracing::info!(
        "connected to \"{}\" ({}x{}, audio {})",
        session.host_name,
        session.width,
        session.height,
        if session.audio { "on" } else { "off" }
    );
    tracing::info!("path: {}", session.route);

    let event_loop = EventLoop::<UiEvent>::with_user_event()
        .build()
        .context("creating event loop")?;
    let ui: Arc<dyn Notify> = Arc::new(ProxyNotify(Mutex::new(event_loop.create_proxy())));
    let picture = Arc::new(Mutex::new(Picture::default()));
    let (control_tx, control_rx) = tokio::sync::mpsc::unbounded_channel();
    let game_boost = Arc::new(AtomicBool::new(false));

    // The output stream must stay on this thread for the whole session.
    let mut _audio_stream = None;
    let audio_sink = if session.audio {
        match playback::start(game_boost.clone()) {
            Ok((stream, sink)) => {
                _audio_stream = Some(stream);
                Some(sink)
            }
            Err(e) => {
                tracing::warn!("audio playback unavailable: {e:#}");
                None
            }
        }
    } else {
        None
    };

    let connect::Session {
        endpoint,
        conn,
        send,
        recv,
        host_name,
        fingerprint,
        width,
        height,
        ..
    } = session;
    if args.stats {
        runtime.spawn(tidedesk_core::stats::log_path(conn.clone(), "path to host"));
    }
    {
        let ui = ui.clone();
        let tell = move |silent| ui.notify(UiEvent::Silent(silent));
        runtime.spawn(tidedesk_core::stats::watch_silence(conn.clone(), tell));
    }
    {
        let stats = args.stats;
        let conn = conn.clone();
        let ui = ui.clone();
        let picture = picture.clone();
        let control_tx = control_tx.clone();
        let game_boost = game_boost.clone();
        runtime.spawn(async move {
            let video = async {
                let stream = conn
                    .accept_uni()
                    .await
                    .context("waiting for video stream")?;
                // The video stream comes first; any after it carry files.
                if tidedesk_core::net::extras(&conn) {
                    tokio::spawn(receive_files(conn.clone(), ui.clone(), control_tx.clone()));
                }
                stream::video_loop(stream, picture, control_tx, ui.clone(), stats, game_boost).await
            };
            let audio = async {
                match audio_sink {
                    Some(sink) => stream::audio_loop(conn.clone(), sink).await,
                    None => std::future::pending().await,
                }
            };
            let company = async {
                use tidedesk_core::company::{Meter, Reading};
                let Some(told) = own_company else {
                    return std::future::pending::<Result<()>>().await;
                };
                let mut meter = Meter::new(tidedesk_core::company::load(), told, Instant::now());
                let mut tick = tokio::time::interval(Duration::from_secs(15));
                loop {
                    tick.tick().await;
                    match meter.read(Instant::now()) {
                        Reading::Tell(allowance) => ui.notify(UiEvent::OwnCompany(allowance)),
                        Reading::Over => {
                            conn.close(0u32.into(), b"company hours used");
                            return Err(anyhow!("{OWN_HOURS_USED}"));
                        }
                        Reading::Nothing => {}
                    }
                }
            };
            let reason = tokio::select! {
                r = company => r.err().map(|e: anyhow::Error| format!("{e:#}")),
                r = video => r.err().map(|e| format!("{e:#}")),
                r = audio => r.err().map(|e| format!("{e:#}")),
                r = stream::control_writer(send, control_rx) => r.err().map(|e| format!("{e:#}")),
                r = stream::control_reader(recv, ui.clone()) => r.err().map(|e| format!("{e:#}")),
            };
            ui.notify(UiEvent::Disconnected(
                reason.unwrap_or_else(|| "host ended the session".into()),
            ));
        });
    }

    let mut app = app::App::new(
        format!("{host_name} | TideDesk"),
        (width, height),
        picture,
        control_tx,
        window_placement::WindowMemory::load(&fingerprint),
        game_boost,
    );
    app.own_company = own_company;
    app.notify = Some(ui.clone());
    app.chats = tidedesk_core::net::extras(&conn);
    if tidedesk_core::net::extras(&conn) {
        let (files_tx, mut files_rx) = tokio::sync::mpsc::unbounded_channel::<std::path::PathBuf>();
        app.files = Some(files_tx);
        let conn = conn.clone();
        let ui = ui.clone();
        runtime.spawn(async move {
            use tidedesk_core::files;
            // One at a time, below the picture, input and sound.
            while let Some(path) = files_rx.recv().await {
                let sent = async {
                    let mut stream = conn.open_uni().await?;
                    stream.set_priority(files::PRIORITY)?;
                    files::send(&mut stream, &path).await?;
                    stream.finish()?;
                    anyhow::Ok(())
                }
                .await;
                // A host that refuses the file stops it and says why itself.
                if let Some(e) = sent.as_ref().err().filter(|e| !files::stopped(e)) {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    ui.notify(UiEvent::Notice(format!("Could not send {name}: {e:#}")));
                }
            }
        });
    }
    if args.control {
        let conn = conn.clone();
        app.test.path = Some(Box::new(move || {
            let path = conn.stats().path;
            let rtt = path.rtt.as_secs_f64() * 1000.0;
            format!("rtt {rtt:.1} ms, {} packets lost", path.lost_packets)
        }));
        let ui = ui.clone();
        let read = move || {
            let lines = std::io::stdin().lines().map_while(Result::ok);
            for line in lines.filter(|line| !line.trim().is_empty()) {
                ui.notify(UiEvent::Command(control::parse(&line)));
            }
        };
        std::thread::Builder::new()
            .name("test control".into())
            .spawn(read)
            .context("test control cannot start")?;
    }
    event_loop.run_app(&mut app)?;

    // `exiting` queued key releases; dropping the app closes the control
    // channel, and the short grace period lets those releases reach the host.
    let exit_message = app.exit_message.take();
    drop(app);
    runtime.block_on(async {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        conn.close(0u32.into(), b"viewer closed");
        let _ =
            tokio::time::timeout(std::time::Duration::from_millis(500), endpoint.wait_idle()).await;
    });
    if let Some(msg) = exit_message {
        eprintln!("disconnected: {msg}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tidedesk_core::nat::signal::DEFAULT_RENDEZVOUS;

    use tidedesk_core::policy::{Policy, Services};

    use super::{DEVICE_IDS_ONLY, NO_SERVICE, lookup_service, rendezvous_service, typed_allowed};

    #[test]
    fn device_id_connections_use_tidedesks_service_unless_told_otherwise() {
        assert_eq!(rendezvous_service(None, None), DEFAULT_RENDEZVOUS);
        assert_eq!(rendezvous_service(None, Some("  ")), DEFAULT_RENDEZVOUS);
        assert_eq!(
            rendezvous_service(None, Some(" rv.example:47900 ")),
            "rv.example:47900"
        );
        assert_eq!(
            rendezvous_service(Some("other.example"), Some("rv.example:47900")),
            "other.example"
        );
        assert_eq!(
            rendezvous_service(Some(" "), Some("rv.example")),
            "rv.example"
        );
    }

    #[test]
    fn the_administrator_decides_which_service_finds_device_ids() {
        let saved = Some("rv.saved:47900");
        assert_eq!(
            lookup_service(None, saved, &Services::Any).as_deref(),
            Ok("rv.saved:47900")
        );
        let only = Services::Only("rv.company:47900".into());
        assert_eq!(
            lookup_service(Some("rv.arg"), saved, &only).as_deref(),
            Ok("rv.company:47900"),
            "over the command line too"
        );
        let list = Services::OneOf(vec!["rv.a:47900".into(), "rv.saved".into()]);
        assert_eq!(
            lookup_service(None, saved, &list).as_deref(),
            Ok("rv.saved:47900"),
            "an allowed choice stands"
        );
        assert_eq!(
            lookup_service(None, None, &list).as_deref(),
            Ok("rv.a:47900")
        );
        assert_eq!(lookup_service(None, saved, &Services::Off), Err(NO_SERVICE));
    }

    #[test]
    fn the_administrator_may_allow_device_ids_only() {
        let open = Policy::default();
        assert_eq!(typed_allowed(&open, false), Ok(()));
        let ids_only = Policy {
            typed_addresses: Some(false),
            ..Policy::default()
        };
        assert_eq!(typed_allowed(&ids_only, true), Ok(()));
        assert_eq!(typed_allowed(&ids_only, false), Err(DEVICE_IDS_ONLY));
    }
}
