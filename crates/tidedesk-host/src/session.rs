//! One viewer connection, from handshake to teardown.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tidedesk_core::auth::{self, Throttle};
use tidedesk_core::protocol::{self, ClientMessage, PROTOCOL_VERSION, RejectReason, ServerMessage};
use tokio::io::AsyncWriteExt;
use tokio::time::timeout;

use crate::input::Injector;
use crate::video::{self, VideoControl, VideoSettings};
use tidedesk_core::clipboard::{ClipboardBridge, valid_text};
use tidedesk_core::protocol::InputEvent;
use tidedesk_core::sharing::SharingState;

/// The viewer currently connected, as shown in the host window.
#[derive(Clone)]
pub struct ViewerInfo {
    pub name: String,
    pub address: SocketAddr,
    pub connection: quinn::Connection,
}

/// Everything a session needs, shared with the UI. Video/audio settings apply on
/// connection; clipboard and mouse permissions are checked during the session.
pub struct HostState {
    pub host_name: String,
    pub code: Mutex<String>,
    pub video: Mutex<VideoSettings>,
    pub audio: AtomicBool,
    pub clipboard: AtomicBool,
    pub mouse: AtomicBool,
    pub accepting: AtomicBool,
    pub throttle: Mutex<Throttle>,
    pub busy: AtomicBool,
    pub viewer: Mutex<Option<ViewerInfo>>,
    /// Called whenever something the UI shows has changed.
    pub on_change: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl HostState {
    pub fn changed(&self) {
        if let Some(notify) = self.on_change.lock().unwrap().as_ref() {
            notify();
        }
    }
}

/// Clears the busy flag and the viewer shown in the UI however the session ends.
struct SessionGuard<'a>(&'a HostState);
impl Drop for SessionGuard<'_> {
    fn drop(&mut self) {
        *self.0.viewer.lock().unwrap() = None;
        self.0.busy.store(false, Ordering::SeqCst);
        self.0.changed();
    }
}

async fn reject(
    send: &mut quinn::SendStream,
    conn: &quinn::Connection,
    reason: RejectReason,
) -> Result<()> {
    protocol::write_message(send, &ServerMessage::Rejected { reason }).await?;
    let _ = send.finish();
    // Give the message a moment to leave before closing the connection.
    let _ = timeout(Duration::from_secs(1), send.stopped()).await;
    conn.close(1u32.into(), b"rejected");
    bail!("rejected viewer: {reason}");
}

pub async fn run(conn: quinn::Connection, state: Arc<HostState>) -> Result<()> {
    let remote = conn.remote_address();
    let (mut send, mut recv) = timeout(Duration::from_secs(10), conn.accept_bi())
        .await
        .context("viewer never opened the control stream")??;

    let hello = timeout(Duration::from_secs(10), protocol::read_message(&mut recv))
        .await
        .context("viewer never sent Hello")??;
    let Some(ClientMessage::Hello {
        protocol_version,
        client_name,
        auth_tag,
        want_audio,
    }) = hello
    else {
        bail!("expected Hello from {remote}");
    };

    if protocol_version != PROTOCOL_VERSION {
        let reason = RejectReason::IncompatibleVersion {
            host_version: PROTOCOL_VERSION,
        };
        return reject(&mut send, &conn, reason).await;
    }
    if !state.accepting.load(Ordering::SeqCst) {
        return reject(&mut send, &conn, RejectReason::NotAccepting).await;
    }
    if state.throttle.lock().unwrap().is_locked(Instant::now()) {
        return reject(&mut send, &conn, RejectReason::TooManyAttempts).await;
    }
    let code = state.code.lock().unwrap().clone();
    if !auth::verify_tag(&conn, &code, &auth_tag)? {
        state
            .throttle
            .lock()
            .unwrap()
            .record_failure(Instant::now());
        tracing::warn!("wrong access code from {remote}");
        tokio::time::sleep(Duration::from_secs(1)).await;
        return reject(&mut send, &conn, RejectReason::BadCode).await;
    }
    state.throttle.lock().unwrap().record_success();
    if state.busy.swap(true, Ordering::SeqCst) {
        return reject(&mut send, &conn, RejectReason::Busy).await;
    }
    let _session = SessionGuard(&state);
    *state.viewer.lock().unwrap() = Some(ViewerInfo {
        name: client_name.clone(),
        address: remote,
        connection: conn.clone(),
    });
    state.changed();
    tracing::info!("viewer \"{client_name}\" connected from {remote}");

    // Video.
    let control = Arc::new(VideoControl {
        stop: AtomicBool::new(false),
        keyframe: AtomicBool::new(false),
    });
    let stop_video = StopOnDrop(&control.stop);
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel(1);
    let video_settings = *state.video.lock().unwrap();
    let control2 = control.clone();
    let info =
        tokio::task::spawn_blocking(move || video::start(video_settings, frame_tx, control2))
            .await??;

    // Audio.
    let audio_stop = Arc::new(AtomicBool::new(false));
    let _stop_audio = StopOnDrop(&audio_stop);
    let (audio_tx, mut audio_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
    let mut audio_on = false;
    if state.audio.load(Ordering::SeqCst) && want_audio {
        match crate::audio::start(audio_tx, audio_stop.clone()) {
            Ok(()) => audio_on = true,
            Err(e) => tracing::warn!("audio disabled for this session: {e:#}"),
        }
    }

    protocol::write_message(
        &mut send,
        &ServerMessage::Welcome {
            host_name: state.host_name.clone(),
            width: info.width,
            height: info.height,
            audio: audio_on,
        },
    )
    .await?;

    let mut video_stream = conn.open_uni().await?;
    let video_task = async {
        while let Some(frame) = frame_rx.recv().await {
            video_stream.write_all(&frame.header.encode()).await?;
            video_stream.write_all(&frame.data).await?;
        }
        anyhow::Ok(())
    };

    let audio_task = async {
        let mut seq: u32 = 0;
        while let Some(packet) = audio_rx.recv().await {
            let datagram = protocol::encode_audio_datagram(seq, &packet);
            seq = seq.wrapping_add(1);
            if let Err(e) = conn.send_datagram(datagram.into()) {
                tracing::debug!("audio datagram dropped: {e}");
            }
        }
        anyhow::Ok(())
    };

    let mut injector = Injector::new(info.rect)?;
    // Keep framing in its own future: cancelling a partial read on every
    // pointer/clipboard tick would corrupt the reliable control stream.
    let (messages_tx, mut messages_rx) = tokio::sync::mpsc::channel(64);
    let reader_task = async {
        while let Some(msg) = protocol::read_message::<_, ClientMessage>(&mut recv).await? {
            if messages_tx.send(msg).await.is_err() {
                break;
            }
        }
        anyhow::Ok(())
    };
    let control_task = async {
        let mut wanted = (0, false, false);
        let mut sharing = SharingState::default();
        let mut clipboard = ClipboardBridge::default();
        let mut pending_clipboard: Option<(u64, String)> = None;
        let mut clipboard_due = Instant::now();
        let mut last_cursor = None;
        let mut tick = tokio::time::interval(Duration::from_millis(16));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if sharing.update(
                wanted.0,
                wanted.1 && state.clipboard.load(Ordering::SeqCst),
                wanted.2 && state.mouse.load(Ordering::SeqCst),
            ) {
                injector.release_mouse();
                pending_clipboard = None;
                clipboard.set_enabled(false);
                clipboard.set_enabled(sharing.clipboard);
                // Capture a baseline before acknowledging activation.
                let _ = clipboard.poll();
                protocol::write_message(&mut send, &ServerMessage::Sharing(sharing)).await?;
            }
            let msg = tokio::select! {
                msg = messages_rx.recv() => {
                    let Some(msg) = msg else { break; };
                    msg
                }
                _ = tick.tick() => {
                    // Recheck permissions changed while select was waiting.
                    if (sharing.clipboard && !state.clipboard.load(Ordering::SeqCst))
                        || (sharing.mouse && !state.mouse.load(Ordering::SeqCst)) {
                        continue;
                    }
                    let (external, position) = injector.poll_pointer()?;
                    if sharing.mouse && external {
                        protocol::write_message(&mut send, &ServerMessage::Pointer(position)).await?;
                    }
                    // Cursor visibility belongs to screen sharing, not input permission.
                    // Send an initial position and changes, including our own injected moves.
                    if last_cursor != Some(position) {
                        protocol::write_message(&mut send, &ServerMessage::Cursor(position)).await?;
                        last_cursor = Some(position);
                    }
                    if Instant::now() >= clipboard_due {
                        clipboard_due = Instant::now() + Duration::from_millis(200);
                        if let Some((generation, text)) = &pending_clipboard {
                            if !sharing.accepts_clipboard(*generation) || clipboard.receive(text) {
                                pending_clipboard = None;
                            }
                        } else if let Some(text) = clipboard.poll() {
                            protocol::write_message(&mut send, &ServerMessage::Clipboard {
                                generation: sharing.generation, text
                            }).await?;
                        }
                    }
                    continue;
                }
            };
            match msg {
                ClientMessage::Input(ev @ InputEvent::Key { .. }) => injector.inject(ev)?,
                ClientMessage::Input(_) => bail!("mouse input requires a pointer epoch"),
                ClientMessage::SetSharing {
                    request,
                    clipboard,
                    mouse,
                } => {
                    wanted = (request, clipboard, mouse);
                }
                ClientMessage::Clipboard { generation, text } => {
                    if !valid_text(&text) {
                        bail!("invalid clipboard text");
                    }
                    if sharing.accepts_clipboard(generation)
                        && state.clipboard.load(Ordering::SeqCst)
                    {
                        pending_clipboard = if clipboard.receive(&text) {
                            None
                        } else {
                            Some((generation, text))
                        };
                    }
                }
                ClientMessage::PointerSync { request } => {
                    if sharing.mouse && state.mouse.load(Ordering::SeqCst) {
                        let position = injector.anchor()?;
                        protocol::write_message(
                            &mut send,
                            &ServerMessage::PointerAnchor { request, position },
                        )
                        .await?;
                    }
                }
                ClientMessage::MouseInput { epoch, event } => {
                    if sharing.mouse
                        && state.mouse.load(Ordering::SeqCst)
                        && let Some(position) = injector.inject_mouse(epoch, event)?
                    {
                        protocol::write_message(&mut send, &ServerMessage::Pointer(position))
                            .await?;
                    }
                }
                ClientMessage::ReleaseMouse => injector.release_mouse(),
                ClientMessage::RequestKeyframe => control.keyframe.store(true, Ordering::Relaxed),
                ClientMessage::Hello { .. } => bail!("duplicate Hello"),
            }
        }
        anyhow::Ok(())
    };

    let result = tokio::select! {
        r = video_task => r.context("video stream"),
        r = audio_task => r.context("audio"),
        r = control_task => r.context("control stream"),
        r = reader_task => r.context("control reader"),
        e = conn.closed() => { tracing::debug!("connection closed: {e}"); Ok(()) }
    };
    drop(stop_video);
    injector.release_all();
    let _ = send.shutdown().await;
    conn.close(0u32.into(), b"bye");
    tracing::info!("viewer \"{client_name}\" disconnected");
    result
}

struct StopOnDrop<'a>(&'a AtomicBool);
impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}
