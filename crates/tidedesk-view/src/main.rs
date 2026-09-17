//! `tidedesk-view`: shows a TideDesk host's screen, plays its audio and
//! forwards keyboard and mouse. Started without a host, it opens a connect window.

// Release builds are GUI apps with no console window; see `attach_console`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod computers;
mod connect;
mod icon;
mod launcher;
mod layout;
mod playback;
mod stream;

use std::io::Write;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::Parser;
use winit::event_loop::{EventLoop, EventLoopProxy};

use stream::{Notify, Picture, UiEvent};

#[derive(Parser, Debug)]
#[command(version, about = "Connect to a TideDesk host.")]
struct Args {
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

    /// Connect through a relay server (not available yet).
    #[arg(long, value_name = "URL")]
    relay: Option<String>,
}

struct ProxyNotify(Mutex<EventLoopProxy<UiEvent>>);

impl Notify for ProxyNotify {
    fn notify(&self, event: UiEvent) {
        let _ = self.0.lock().unwrap().send_event(event);
    }
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

fn main() {
    attach_console();
    tracing_subscriber::fmt().with_target(false).init();
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    if args.relay.is_some() {
        eprintln!(
            "Relay connections are not implemented yet.\n\
             For now, connect through a VPN such as Tailscale or WireGuard, or have the host \
             forward its UDP port. See docs/internet-access.md."
        );
        std::process::exit(2);
    }
    let Some(host) = args.host.clone() else {
        return launcher::run();
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
    };
    let session = runtime.block_on(connect::connect(&opts))?;
    tracing::info!(
        "connected to \"{}\" ({}x{}, audio {})",
        session.host_name,
        session.width,
        session.height,
        if session.audio { "on" } else { "off" }
    );

    let event_loop = EventLoop::<UiEvent>::with_user_event()
        .build()
        .context("creating event loop")?;
    let ui: Arc<dyn Notify> = Arc::new(ProxyNotify(Mutex::new(event_loop.create_proxy())));
    let picture = Arc::new(Mutex::new(Picture::default()));
    let (control_tx, control_rx) = tokio::sync::mpsc::unbounded_channel();

    // The output stream must stay on this thread for the whole session.
    let mut _audio_stream = None;
    let audio_sink = if session.audio {
        match playback::start() {
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
        width,
        height,
        ..
    } = session;
    {
        let stats = args.stats;
        let conn = conn.clone();
        let ui = ui.clone();
        let picture = picture.clone();
        let control_tx = control_tx.clone();
        runtime.spawn(async move {
            let video = async {
                let stream = conn
                    .accept_uni()
                    .await
                    .context("waiting for video stream")?;
                stream::video_loop(stream, picture, control_tx, ui.clone(), stats).await
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
                r = stream::control_reader(recv) => r.err().map(|e| format!("{e:#}")),
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
