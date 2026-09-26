//! The TideDesk viewer: shows a host's screen, plays its audio and forwards
//! keyboard and mouse. Started without a host, it opens a connect window.
//! Runs as `tidedesk view …` inside the one program.

mod app;
mod child;
mod computers;
mod connect;
mod icon;
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

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser};
use tidedesk_core::identity::{KnownHosts, PinStatus};
use tidedesk_core::nat::signal::DEFAULT_RENDEZVOUS;
use tidedesk_core::paths;
use winit::event_loop::{EventLoop, EventLoopProxy};

use child::ChildLine;
use stream::{Notify, Picture, UiEvent};

#[derive(Parser, Debug)]
#[command(version, about = "Connect to a TideDesk host.")]
struct Args {
    /// Open viewer settings without connecting.
    #[arg(long)]
    settings: bool,
    /// Host to connect to: name or IP, optionally with :port. Omit to open the
    /// connect window.
    host: Option<String>,

    /// Access code shown by the host (prompted for if omitted).
    #[arg(long, env = "TIDEDESK_CODE", hide_env_values = true)]
    code: Option<String>,

    /// Do not play the host's audio.
    #[arg(long)]
    no_audio: bool,

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
    print!("Access code: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

#[cfg(windows)]
fn attach_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // Borrow the terminal's console when started from one, so CLI use still prints.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(not(windows))]
fn attach_console() {}

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
/// Exits the process on failure.
pub fn main(program: &str, argv: Vec<OsString>, self_prefix: &'static [&'static str]) {
    set_self_prefix(self_prefix);
    attach_console();
    tracing_subscriber::fmt().with_target(false).init();
    if let Err(e) = run(program, argv) {
        eprintln!("{}", ChildLine::Error(format!("{e:#}")));
        std::process::exit(1);
    }
}

fn run(program: &str, argv: Vec<OsString>) -> Result<()> {
    let matches = Args::command().bin_name(program).get_matches_from(argv);
    let args = Args::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    if args.settings {
        return settings::run();
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
    let route = if connect::parse_device_id(&host).is_some() {
        if args.internet {
            bail!("a device ID is found through a rendezvous service; leave out --internet");
        }
        let saved = settings::ViewerSettings::load()
            .ok()
            .map(|s| s.rendezvous_server);
        let service = rendezvous_service(args.rendezvous.as_deref(), saved.as_deref());
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
        let dialer = connect::Dialer::new(&opts.host, &opts.route, report).await?;
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
                stream::video_loop(stream, picture, control_tx, ui.clone(), stats, game_boost).await
            };
            let audio = async {
                match audio_sink {
                    Some(sink) => stream::audio_loop(conn.clone(), sink).await,
                    None => std::future::pending().await,
                }
            };
            let reason = tokio::select! {
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
        format!("{host_name} — TideDesk"),
        (width, height),
        picture,
        control_tx,
        window_placement::WindowMemory::load(&fingerprint),
        game_boost,
    );
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

    use super::rendezvous_service;

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
}
