//! One viewer connection, from handshake to teardown.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tidedesk_core::auth::{self, Throttle};
use tidedesk_core::company;
use tidedesk_core::net;
use tidedesk_core::protocol::{self, ClientMessage, PROTOCOL_VERSION, RejectReason, ServerMessage};
use tokio::io::AsyncWriteExt;
use tokio::time::timeout;

use crate::input::Injector;
use crate::session_log;
use crate::video::{self, VideoControl, VideoSettings};
use tidedesk_core::clipboard::{ClipboardBridge, valid_text};
use tidedesk_core::protocol::InputEvent;
use tidedesk_core::sharing::SharingState;

/// The viewer currently connected, as shown in the host window.
#[derive(Clone)]
pub struct ViewerInfo {
    pub name: String,
    /// Its certificate's fingerprint, when it showed one (viewers from
    /// before invitations show none).
    pub fingerprint: Option<String>,
    pub address: SocketAddr,
    pub connection: quinn::Connection,
    /// It copies files ([`net::extras`]).
    pub files: bool,
    /// When it connected, and how it was let in.
    pub since: Instant,
    pub admitted_by: &'static str,
}

/// Registers with `service` for `code`.
pub type Register = Box<dyn Fn(&str, &str) + Send + Sync>;

/// Everything a session needs, shared with the UI. Video/audio settings apply on
/// connection; clipboard and mouse permissions are checked during the session.
pub struct HostState {
    pub host_name: String,
    pub codes: Mutex<crate::codes::Codes>,
    /// Viewers that come back without the access code.
    pub trusted: Mutex<crate::trusted::TrustedViewers>,
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
    /// Save files the viewer sends.
    pub files: AtomicBool,
    /// The last file received or sent, for the window to say.
    pub files_note: Mutex<Option<String>>,
    /// The chat with the viewer: (written at the viewer, text).
    pub chat: Mutex<Vec<(bool, String)>>,
    /// Chat messages to send, while a viewer that chats is connected.
    pub chat_out: Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>,
    /// Files to send to the viewer, while one that copies files is connected.
    pub outgoing: Mutex<Option<tokio::sync::mpsc::UnboundedSender<std::path::PathBuf>>>,
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
        *self.0.outgoing.lock().unwrap() = None;
        *self.0.chat_out.lock().unwrap() = None;
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

/// Whether the administrator allows coming in with `proof`; a trusted
/// viewer is not asked.
fn way_allowed(policy: &tidedesk_core::policy::Policy, proof: &Knows) -> bool {
    let allowed = match proof {
        Knows::Code(_) => policy.access_code,
        Knows::Password(_) => policy.saved_password,
    };
    allowed != Some(false)
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
    // A viewer this host trusts holds its certificate's key: no code needed.
    let fingerprint = net::peer_fingerprint(&conn);
    let trusted = fingerprint
        .as_deref()
        .is_some_and(|fp| crate::trusted::trusted_anywhere(&state.trusted.lock().unwrap(), fp));
    if !trusted && !way_allowed(&tidedesk_core::policy::current(), &proof) {
        tracing::info!("refused {remote}: the administrator does not allow this way in");
        return reject(&mut send, &conn, RejectReason::WayNotAllowed).await;
    }
    if !trusted
        && state
            .throttle
            .lock()
            .unwrap()
            .is_locked(remote.ip(), Instant::now())
    {
        return reject(&mut send, &conn, RejectReason::TooManyAttempts).await;
    }
    let right = trusted
        || match &proof {
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
    // A company computer without a licence: its trial or monthly hours.
    let company = company::allowance();
    if company == Some(company::Allowance::Used) {
        tracing::info!("refused {remote}: this company computer's hours for the month are used");
        return reject(&mut send, &conn, RejectReason::CompanyHoursUsed).await;
    }
    if state.busy.swap(true, Ordering::SeqCst) {
        return reject(&mut send, &conn, RejectReason::Busy).await;
    }
    let _session = SessionGuard(&state);
    // What this viewer may do, on top of this host's permissions.
    let limits = crate::limits::for_viewer(fingerprint.as_deref());
    if limits != crate::limits::Limits::default() {
        tracing::info!("viewer \"{client_name}\" is limited: {limits:?}");
    }
    let admitted_by = match proof {
        _ if trusted => "trusted viewer",
        Knows::Code(_) => "access code",
        Knows::Password(_) => "saved password",
    };
    let mut record = session_log::Recorder::start(session_log::Entry {
        started: std::time::SystemTime::now(),
        ended: std::time::SystemTime::now(),
        viewer: client_name.clone(),
        fingerprint: fingerprint.clone(),
        address: remote.to_string(),
        admitted_by,
        ended_because: "the session could not start".into(),
    });
    *state.viewer.lock().unwrap() = Some(ViewerInfo {
        name: client_name.clone(),
        fingerprint,
        address: remote,
        connection: conn.clone(),
        files: net::extras(&conn),
        since: Instant::now(),
        admitted_by,
    });
    state.changed();
    let how = if trusted { " (trusted)" } else { "" };
    tracing::info!("viewer \"{client_name}\" connected from {remote}{how}");

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
    if let Some(allowance) = company {
        protocol::write_message(&mut send, &ServerMessage::CompanyUse(allowance)).await?;
    }
    let mut meter = company.map(|told| company::Meter::new(company::load(), told, Instant::now()));
    let company_over = AtomicBool::new(false);

    // Chat, when the viewer chats too.
    let (chat_tx, mut chat_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    if net::extras(&conn) {
        state.chat.lock().unwrap().clear();
        *state.chat_out.lock().unwrap() = Some(chat_tx);
    }

    // Files the host picked for the viewer, one at a time.
    let (outgoing_tx, mut outgoing_rx) = tokio::sync::mpsc::unbounded_channel();
    if net::extras(&conn) {
        *state.outgoing.lock().unwrap() = Some(outgoing_tx);
    }
    let send_files_task = async {
        while let Some(path) = outgoing_rx.recv().await {
            send_file(&conn, &path, &state).await;
        }
        std::future::pending::<Result<()>>().await
    };

    // Files from a viewer that copies them, each on its own stream.
    let (saved_tx, mut saved_rx) = tokio::sync::mpsc::unbounded_channel::<ServerMessage>();
    let files_task = async {
        if !net::extras(&conn) {
            return std::future::pending::<Result<()>>().await;
        }
        loop {
            let Ok(stream) = conn.accept_uni().await else {
                // The connection is over: another task says why.
                return std::future::pending().await;
            };
            tokio::spawn(receive_file(
                stream,
                state.clone(),
                client_name.clone(),
                saved_tx.clone(),
                limits.files,
            ));
        }
    };

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
                wanted.1 && limits.clipboard && state.clipboard.load(Ordering::SeqCst),
                wanted.2 && limits.input && state.mouse.load(Ordering::SeqCst),
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
                    while let Ok(saved) = saved_rx.try_recv() {
                        protocol::write_message(&mut send, &saved).await?;
                    }
                    while let Ok(text) = chat_rx.try_recv() {
                        protocol::write_message(&mut send, &ServerMessage::Chat { text }).await?;
                    }
                    if let Some(meter) = &mut meter {
                        match meter.read(Instant::now()) {
                            company::Reading::Tell(allowance) => {
                                protocol::write_message(&mut send, &ServerMessage::CompanyUse(allowance)).await?;
                            }
                            company::Reading::Over => {
                                company_over.store(true, Ordering::SeqCst);
                                protocol::write_message(&mut send, &ServerMessage::CompanyUse(company::Allowance::Used)).await?;
                                bail!("this company computer's hours for the month are used");
                            }
                            company::Reading::Nothing => {}
                        }
                    }
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
                ClientMessage::Input(ev @ InputEvent::Key { .. }) => {
                    if limits.input {
                        injector.inject(ev)?;
                    }
                }
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
                        && limits.clipboard
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
                ClientMessage::Chat { text } => {
                    if let Some(text) = tidedesk_core::chat::clean(&text) {
                        state.chat.lock().unwrap().push((true, text));
                        state.changed();
                    }
                }
                ClientMessage::FileSaved { name, error } => {
                    *state.files_note.lock().unwrap() = Some(match error {
                        None => format!("The viewer saved {name} in Downloads\\TideDesk."),
                        Some(why) => format!("The viewer did not save {name}: {why}"),
                    });
                    state.changed();
                }
            }
        }
        anyhow::Ok(())
    };

    record.ended_because("TideDesk stopped during the session".into());
    let result = tokio::select! {
        r = video_task => r.context("video stream"),
        r = audio_task => r.context("audio"),
        r = control_task => r.context("control stream"),
        r = reader_task => r.context("control reader"),
        r = files_task => r.context("files"),
        r = send_files_task => r.context("sending files"),
        e = conn.closed() => { tracing::debug!("connection closed: {e}"); Ok(()) }
    };
    record.ended_because(match &result {
        Ok(()) => "the connection closed".into(),
        Err(e) => plain_reason(&format!("{e:#}")),
    });
    drop(stop_video);
    injector.release_all();
    let _ = send.shutdown().await;
    let reason: &[u8] = if company_over.load(Ordering::SeqCst) {
        b"company hours used"
    } else {
        b"bye"
    };
    conn.close(0u32.into(), reason);
    tracing::info!("viewer \"{client_name}\" disconnected");
    result
}

/// Sends one file the host picked to the viewer; the viewer says where it
/// went (`ClientMessage::FileSaved`).
async fn send_file(conn: &quinn::Connection, path: &std::path::Path, state: &HostState) {
    use tidedesk_core::files;
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    *state.files_note.lock().unwrap() = Some(format!("Sending {name} to the viewer..."));
    state.changed();
    let sent = async {
        let mut stream = conn.open_uni().await?;
        stream.set_priority(files::PRIORITY)?;
        files::send(&mut stream, path).await?;
        stream.finish()?;
        anyhow::Ok(())
    }
    .await;
    // A viewer that refused the file stopped it and says why itself.
    if let Some(e) = sent.as_ref().err().filter(|e| !files::stopped(e)) {
        tracing::warn!("could not send {name} to the viewer: {e:#}");
        *state.files_note.lock().unwrap() = Some(format!("Could not send {name}: {e:#}"));
        state.changed();
    }
}

/// Saves one file from the viewer in Downloads\TideDesk, and tells the
/// viewer where it went or why not.
async fn receive_file(
    mut stream: quinn::RecvStream,
    state: Arc<HostState>,
    viewer: String,
    saved: tokio::sync::mpsc::UnboundedSender<ServerMessage>,
    // This viewer may send files (its limits), on top of the host's setting.
    allowed: bool,
) {
    use tidedesk_core::files;
    let header = match files::read_header(&mut stream).await {
        Ok(header) => header,
        Err(e) => {
            tracing::warn!("a file from \"{viewer}\" could not be read: {e:#}");
            return;
        }
    };
    let result = if !allowed || !state.files.load(Ordering::SeqCst) {
        let _ = stream.stop(1u32.into());
        Err(anyhow::anyhow!("the host does not accept files"))
    } else {
        match files::downloads() {
            Ok(dir) => files::save(&mut stream, &header, &dir).await,
            Err(e) => Err(e),
        }
    };
    let message = match result {
        Ok(path) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            tracing::info!("received {name} from \"{viewer}\"");
            *state.files_note.lock().unwrap() = Some(format!(
                "Received {name} from {viewer}, in Downloads\\TideDesk."
            ));
            state.changed();
            ServerMessage::FileSaved { name, error: None }
        }
        Err(e) => {
            tracing::warn!("a file from \"{viewer}\" was not saved: {e:#}");
            ServerMessage::FileSaved {
                name: header.name,
                error: Some(format!("{e:#}")),
            }
        }
    };
    let _ = saved.send(message);
}

/// Why a session ended, in words for the session history; the technical
/// reason goes to the log.
fn plain_reason(reason: &str) -> String {
    let says = |text: &str| reason.contains(text);
    if says("viewer closed") {
        "the viewer left"
    } else if says("disconnected by host") {
        "disconnected here"
    } else if says("host quit") {
        "TideDesk was closed here"
    } else if says("company hours used") || says("hours for the month are used") {
        "this month's hours were used"
    } else if says("timed out") {
        "no answer from the viewer"
    } else {
        return reason.to_string();
    }
    .into()
}

struct StopOnDrop<'a>(&'a AtomicBool);
impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_history_says_why_in_plain_words() {
        for (reason, plain) in [
            (
                "control reader: connection lost: closed by peer: viewer closed (code 0)",
                "the viewer left",
            ),
            (
                "control stream: connection lost: closed: disconnected by host (code 2)",
                "disconnected here",
            ),
            (
                "video stream: connection lost: timed out",
                "no answer from the viewer",
            ),
            ("something else", "something else"),
        ] {
            assert_eq!(super::plain_reason(reason), plain, "{reason}");
        }
    }

    #[test]
    fn the_administrator_decides_which_ways_in_are_allowed() {
        use super::{Knows, way_allowed};
        use tidedesk_core::policy::Policy;
        let (code, password) = (Knows::Code([0; 32]), Knows::Password(vec![1]));
        let open = Policy::default();
        assert!(way_allowed(&open, &code) && way_allowed(&open, &password));
        let password_only = Policy {
            access_code: Some(false),
            saved_password: Some(true),
            ..Policy::default()
        };
        assert!(!way_allowed(&password_only, &code));
        assert!(way_allowed(&password_only, &password));
        let code_only = Policy {
            saved_password: Some(false),
            ..Policy::default()
        };
        assert!(way_allowed(&code_only, &code));
        assert!(!way_allowed(&code_only, &password));
    }
}
