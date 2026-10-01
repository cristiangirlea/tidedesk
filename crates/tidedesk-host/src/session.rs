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

/// Registers with `service` for `code`.
pub type Register = Box<dyn Fn(&str, &str) + Send + Sync>;

/// Everything a session needs, shared with the UI. Video/audio settings apply on
/// connection; clipboard and mouse permissions are checked during the session.
pub struct HostState {
    pub host_name: String,
    pub codes: Mutex<crate::codes::Codes>,
    /// The saved password's key, if one is set.
    pub password: Mutex<Option<tidedesk_core::password::Key>>,
    /// A new code once a session ends (the window's host; a headless one
    /// keeps its code, as nobody sees a new one there).
    pub new_code_after_session: AtomicBool,
    /// The connection service this host registers with, if any.
    pub registration: Mutex<Option<String>>,
    /// Registers with a service again for a new code: the registration
    /// seals the local addresses with it.
    pub register: Mutex<Option<Register>>,
    /// Why the code changed, for the window to say.
    pub code_note: Mutex<Option<String>>,
    pub video: Mutex<VideoSettings>,
    pub audio: AtomicBool,
    pub clipboard: AtomicBool,
    pub mouse: AtomicBool,
    pub accepting: AtomicBool,
    pub throttle: Mutex<Throttle>,
    pub busy: AtomicBool,
    pub viewer: Mutex<Option<ViewerInfo>>,
    /// A viewer on another network this host opens a path to.
    pub expected_viewer: Mutex<Option<crate::internet::ExpectedViewer>>,
    /// Called whenever something the UI shows has changed.
    pub on_change: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl HostState {
    pub fn changed(&self) {
        if let Some(notify) = self.on_change.lock().unwrap().as_ref() {
            notify();
        }
    }

    /// Makes and saves a new access code. After a session the old one
    /// works for a few minutes more; asked for, it stops at once.
    pub fn renew_code(&self, after_session: bool) -> Result<String> {
        let new = crate::load_code(true)?;
        {
            let mut codes = self.codes.lock().unwrap();
            if after_session {
                codes.after_session(new.clone(), Instant::now());
            } else {
                codes.replace(new.clone());
            }
        }
        let service = self.registration.lock().unwrap().clone();
        if let (Some(service), Some(register)) = (service, self.register.lock().unwrap().as_ref()) {
            register(&service, &new);
        }
        *self.code_note.lock().unwrap() = Some(if after_session {
            format!(
                "New code after the session. The old one works for {} more minutes.",
                crate::codes::GRACE.as_secs() / 60
            )
        } else {
            "New code saved. The old one no longer works.".into()
        });
        self.changed();
        Ok(new)
    }
}

/// Clears the busy flag and the viewer shown in the UI however the session ends.
struct SessionGuard<'a>(&'a HostState);
impl Drop for SessionGuard<'_> {
    fn drop(&mut self) {
        *self.0.viewer.lock().unwrap() = None;
        self.0.busy.store(false, Ordering::SeqCst);
        if self.0.new_code_after_session.load(Ordering::SeqCst)
            && let Err(e) = self.0.renew_code(true)
        {
            tracing::warn!("could not make a new access code: {e:#}");
        }
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

/// What a viewer says it knows.
enum Knows {
    /// The proof of the access code.
    Code([u8; 32]),
    /// The first SPAKE2 message for the saved password.
    Password(Vec<u8>),
}

/// The password exchange: whether the viewer knows the saved password, or
/// None when this host has none.
async fn password_admits(
    conn: &quinn::Connection,
    state: &HostState,
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    start: &[u8],
) -> Result<Option<bool>> {
    let Some(key) = *state.password.lock().unwrap() else {
        return Ok(None);
    };
    let binding = auth::session_binding(conn)?;
    let Ok((host, answer, proof)) = tidedesk_core::password::Host::answer(&key, binding, start)
    else {
        return Ok(Some(false));
    };
    protocol::write_message(send, &ServerMessage::PasswordAnswer { answer, proof }).await?;
    // A viewer whose password is wrong finds out from the answer and leaves.
    let reply = timeout(Duration::from_secs(10), protocol::read_message(recv)).await;
    Ok(Some(matches!(
        reply,
        Ok(Ok(Some(ClientMessage::PasswordProof { proof }))) if host.accepts(&proof)
    )))
}

pub async fn run(conn: quinn::Connection, state: Arc<HostState>) -> Result<()> {
    let remote = conn.remote_address();
    let (mut send, mut recv) = timeout(Duration::from_secs(10), conn.accept_bi())
        .await
        .context("viewer never opened the control stream")??;

    let hello = timeout(Duration::from_secs(10), protocol::read_message(&mut recv))
        .await
        .context("viewer never sent Hello")??;
    // What the viewer proves it knows: the access code or the saved password.
    let (protocol_version, client_name, want_audio, proof) = match hello {
        Some(ClientMessage::Hello {
            protocol_version,
            client_name,
            auth_tag,
            want_audio,
        }) => (
            protocol_version,
            client_name,
            want_audio,
            Knows::Code(auth_tag),
        ),
        Some(ClientMessage::PasswordHello {
            protocol_version,
            client_name,
            want_audio,
            start,
        }) => (
            protocol_version,
            client_name,
            want_audio,
            Knows::Password(start),
        ),
        _ => bail!("expected Hello from {remote}"),
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
    if state
        .throttle
        .lock()
        .unwrap()
        .is_locked(remote.ip(), Instant::now())
    {
        return reject(&mut send, &conn, RejectReason::TooManyAttempts).await;
    }
    let right = match &proof {
        Knows::Code(tag) => {
            let codes = state.codes.lock().unwrap().valid(Instant::now());
            let mut right = false;
            for code in &codes {
                if auth::verify_tag(&conn, code, tag)? {
                    right = true;
                    break;
                }
            }
            right
        }
        Knows::Password(start) => {
            match password_admits(&conn, &state, &mut send, &mut recv, start).await? {
                Some(right) => right,
                None => return reject(&mut send, &conn, RejectReason::NoPassword).await,
            }
        }
    };
    if !right {
        {
            let mut throttle = state.throttle.lock().unwrap();
            throttle.record_failure(remote.ip(), Instant::now());
            // A password counts double: three wrong ones lock the address out.
            if matches!(proof, Knows::Password(_)) {
                throttle.record_failure(remote.ip(), Instant::now());
            }
        }
        state.changed();
        tokio::time::sleep(Duration::from_secs(1)).await;
        let reason = match proof {
            Knows::Code(_) => RejectReason::BadCode,
            Knows::Password(_) => RejectReason::BadPassword,
        };
        tracing::warn!("{reason} from {remote}");
        return reject(&mut send, &conn, reason).await;
    }
    state.throttle.lock().unwrap().record_success(remote.ip());
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
    let control = Arc::new(VideoControl::default());
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
            let streaming = control.streaming_status.lock().unwrap().take();
            if let Some(status) = streaming {
                protocol::write_message(&mut send, &ServerMessage::Streaming(status)).await?;
            }
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
                    let (external, position) = injector.poll_pointer();
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
                        let position = injector.anchor();
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
                ClientMessage::SetGameBoost { request, enabled } => {
                    *control.boost_request.lock().unwrap() = (request, enabled);
                }
                ClientMessage::RequestKeyframe => control.keyframe.store(true, Ordering::Relaxed),
                ClientMessage::Hello { .. } | ClientMessage::PasswordHello { .. } => {
                    bail!("duplicate Hello")
                }
                ClientMessage::PasswordProof { .. } => bail!("password proof after the handshake"),
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
