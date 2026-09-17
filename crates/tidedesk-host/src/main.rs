//! `tidedesk-host`: shares this machine's screen and audio.

mod audio;
mod capture;
mod input;
mod session;
mod video;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use clap::Parser;
use tidedesk_core::identity::HostIdentity;
use tidedesk_core::{DEFAULT_PORT, auth, net, paths};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Share this computer's screen and sound with a TideDesk viewer."
)]
struct Args {
    /// Address and UDP port to listen on.
    #[arg(long, default_value_t = SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)))]
    listen: SocketAddr,

    /// Which display to share (see --list-displays).
    #[arg(long, default_value_t = 0)]
    display: usize,

    /// Maximum frames per second.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=120))]
    fps: u32,

    /// Target video bitrate in kbit/s.
    #[arg(long, default_value_t = 4000, value_parser = clap::value_parser!(u32).range(250..=100_000))]
    bitrate: u32,

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

fn load_code(dir: &std::path::Path, regenerate: bool) -> Result<String> {
    let path = dir.join("access-code.txt");
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

#[cfg(windows)]
fn enable_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    // Without this, Windows reports scaled coordinates to us and pointer
    // positions land in the wrong place on high-DPI displays.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let args = Args::parse();

    if args.relay.is_some() {
        eprintln!(
            "Relay connections are not implemented yet.\n\
             For now, reach this host over the internet through a VPN such as Tailscale or \
             WireGuard, or forward UDP port {} on your router.\n\
             See docs/internet-access.md.",
            args.listen.port()
        );
        std::process::exit(2);
    }

    #[cfg(windows)]
    enable_dpi_awareness();

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

    let dir = paths::config_dir()?;
    let identity = HostIdentity::load_or_create(&dir)?;
    let code = load_code(&dir, args.new_code)?;
    let endpoint = net::server_endpoint(args.listen, &identity)?;

    let host_name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "tidedesk-host".into());
    let state = Arc::new(session::HostState {
        code: code.clone(),
        host_name: host_name.clone(),
        video: video::VideoSettings {
            display: args.display,
            fps: args.fps,
            bitrate_bps: args.bitrate * 1000,
            stats: args.stats,
        },
        audio: !args.no_audio,
        throttle: Mutex::new(auth::Throttle::default()),
        busy: AtomicBool::new(false),
    });

    println!();
    println!(
        "  TideDesk host \"{host_name}\" is listening on UDP {}",
        args.listen
    );
    println!("  Access code:  {code}");
    println!("  Fingerprint:  {}", identity.fingerprint());
    println!();
    println!("  Connect with: tidedesk-view <this-pc-address> --code {code}");
    println!();

    while let Some(incoming) = endpoint.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            let remote = incoming.remote_address();
            let result = async {
                let conn = incoming.await?;
                session::run(conn, state).await
            }
            .await;
            if let Err(e) = result {
                tracing::warn!("session with {remote} ended: {e:#}");
            }
        });
    }
    Ok(())
}
