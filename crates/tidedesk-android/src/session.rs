//! One viewer session, as the Android app runs it: connect by address or by
//! device ID, prove the access code, then read video frames and send mouse
//! input until it closes.
//!
//! The video is handed on undecoded: each frame is one H.264 access unit
//! (constrained baseline, Annex B, SPS and PPS in front of every keyframe),
//! which Android's hardware decoder takes as it is. Frames queue a few deep
//! only, so a slow decoder slows the host down instead of piling up.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tidedesk_core::identity::{KnownHosts, PinStatus};
use tidedesk_core::nat::signal::{LookupOutcome, resolve_service};
use tidedesk_core::nat::{Agent, SharedSocket};
pub use tidedesk_core::protocol::MouseButton;
use tidedesk_core::protocol::{
    self, ClientMessage, InputEvent, MAX_VIDEO_FRAME, PROTOCOL_VERSION, ServerMessage,
    VideoFrameHeader,
};
use tidedesk_core::sharing::PointerPosition;
use tidedesk_core::{DEFAULT_PORT, auth, net};
use tidedesk_rendezvous_proto::DeviceId;
use tokio::sync::{mpsc, watch};

/// Frames waiting for the decoder: few, so that a slow decoder slows the
/// host down (it skips captures) instead of memory filling up.
const FRAMES_QUEUED: usize = 4;

/// How long to punch towards a host the connection service introduced.
const PUNCH_WINDOW: Duration = Duration::from_secs(20);

/// The one request for the mouse a session makes.
const SHARING_REQUEST: u64 = 1;

/// One encoded frame from the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u16,
    pub height: u16,
    pub keyframe: bool,
    /// Microseconds since the host's video started.
    pub capture_us: u64,
    /// One H.264 access unit, Annex B.
    pub data: Vec<u8>,
}

/// Where the host's mouse pointer is, from 0 to 1 across its screen. The
/// video does not show it: the phone draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursor {
    pub x: f32,
    pub y: f32,
    /// Off the shared screen, it is not drawn.
    pub visible: bool,
}

/// What to connect to, and as whom.
#[derive(Debug, Clone)]
pub struct Options {
    /// A device ID (`TD-1A2B-…`), or an address with an optional port.
    pub target: String,
    pub code: String,
    /// This phone's name, as the host shows it.
    pub name: String,
    /// The app's own files: the hosts this phone has met (`known_hosts.txt`).
    pub data_dir: PathBuf,
    /// The connection service for device IDs.
    pub service: String,
}

/// Where to connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Address(String),
    DeviceId(tidedesk_rendezvous_proto::DeviceId),
}

/// Reads what was typed: a device ID, or else an address.
pub fn target(text: &str) -> Target {
    let text = text.trim();
    match text.parse::<DeviceId>() {
        Ok(id) => Target::DeviceId(id),
        Err(_) => Target::Address(text.to_string()),
    }
}

/// From 0 to 1 across the screen to the wire's 0 to 65535.
fn wire(fraction: f32) -> u16 {
    (fraction.clamp(0.0, 1.0) * 65535.0).round() as u16
}

/// Mouse control, as the host grants it: mouse events count only against
/// the host pointer's current epoch, read with `PointerSync`, and the host
/// moving its own pointer ends it. Events made before the anchor arrives
/// wait for it.
#[derive(Default)]
struct Pointer {
    epoch: Option<u64>,
    /// The `PointerSync` waiting for its anchor.
    waiting: Option<u64>,
    queued: Vec<InputEvent>,
    requests: u64,
}

impl Pointer {
    /// What to send for `event`.
    fn send(&mut self, event: InputEvent) -> Vec<ClientMessage> {
        if let Some(epoch) = self.epoch {
            return vec![ClientMessage::MouseInput { epoch, event }];
        }
        self.queued.push(event);
        if self.waiting.is_some() {
            return Vec::new();
        }
        self.requests += 1;
        self.waiting = Some(self.requests);
        vec![ClientMessage::PointerSync {
            request: self.requests,
        }]
    }

    /// The host's answer to `PointerSync`: control, unless its pointer is
    /// off the shared screen, as the desktop viewer does.
    fn anchored(&mut self, request: u64, position: PointerPosition) -> Vec<ClientMessage> {
        if self.waiting != Some(request) {
            return Vec::new();
        }
        self.waiting = None;
        let queued = std::mem::take(&mut self.queued);
        if !position.inside {
            return Vec::new();
        }
        self.epoch = Some(position.epoch);
        queued
            .into_iter()
            .map(|event| ClientMessage::MouseInput {
                epoch: position.epoch,
                event,
            })
            .collect()
    }

    /// The host's own pointer moved: control goes back to it.
    fn lost(&mut self) -> Vec<ClientMessage> {
        match self.epoch.take() {
            Some(_) => vec![ClientMessage::ReleaseMouse],
            None => Vec::new(),
        }
    }
}

/// A connected session.
pub struct Viewer {
    // Dropped last: it runs everything below.
    host_name: String,
    connection: quinn::Connection,
    control: mpsc::UnboundedSender<ClientMessage>,
    pointer: Arc<Mutex<Pointer>>,
    frames: Mutex<mpsc::Receiver<Frame>>,
    cursor: Mutex<watch::Receiver<Option<Cursor>>>,
    /// Smoother-video requests so far: each change is its own request.
    boosts: AtomicU64,
    _endpoint: quinn::Endpoint,
    _agent: Option<Arc<Agent>>,
    runtime: tokio::runtime::Runtime,
}

/// A session past the access code.
struct Opened {
    endpoint: quinn::Endpoint,
    agent: Option<Arc<Agent>>,
    connection: quinn::Connection,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    host_name: String,
}

impl Viewer {
    /// Connects and proves the access code; blocks until the host let this
    /// phone in, or said why not.
    pub fn connect(options: &Options) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let opened = runtime.block_on(open(options))?;
        let (control, outgoing) = mpsc::unbounded_channel();
        let (frames, incoming) = mpsc::channel(FRAMES_QUEUED);
        let pointer = Arc::new(Mutex::new(Pointer::default()));
        let (cursor_tx, cursor) = watch::channel(None);
        runtime.spawn(write_control(opened.send, outgoing));
        runtime.spawn(read_control(
            opened.recv,
            pointer.clone(),
            control.clone(),
            cursor_tx,
        ));
        runtime.spawn(read_video(opened.connection.clone(), frames));
        // Before any mouse event: the host takes them only once granted.
        let _ = control.send(ClientMessage::SetSharing {
            request: SHARING_REQUEST,
            clipboard: false,
            mouse: true,
        });
        Ok(Viewer {
            host_name: opened.host_name,
            connection: opened.connection,
            control,
            pointer,
            frames: Mutex::new(incoming),
            cursor: Mutex::new(cursor),
            boosts: AtomicU64::new(0),
            _endpoint: opened.endpoint,
            _agent: opened.agent,
            runtime,
        })
    }

    pub fn host_name(&self) -> &str {
        &self.host_name
    }

    /// The next frame, waiting for it; `None` once the session ended.
    pub fn next_frame(&self) -> Option<Frame> {
        self.frames.lock().unwrap().blocking_recv()
    }

    fn mouse(&self, event: InputEvent) {
        for message in self.pointer.lock().unwrap().send(event) {
            let _ = self.control.send(message);
        }
    }

    /// Moves the host pointer to `x`, `y`, from 0 to 1 across the screen.
    pub fn pointer(&self, x: f32, y: f32) {
        self.mouse(InputEvent::MouseMove {
            x: wire(x),
            y: wire(y),
        });
    }

    pub fn button(&self, button: MouseButton, pressed: bool) {
        self.mouse(InputEvent::MouseButton { button, pressed });
    }

    /// Scrolls by `notches`; positive is up.
    pub fn wheel(&self, notches: i32) {
        self.mouse(InputEvent::MouseWheel {
            dx: 0,
            dy: notches.saturating_mul(120),
        });
    }

    /// Presses or releases the key with PC/AT set-1 `scancode` (`0xE0` in the
    /// high byte for extended keys).
    pub fn key(&self, scancode: u16, pressed: bool) {
        let key = InputEvent::Key { scancode, pressed };
        let _ = self.control.send(ClientMessage::Input(key));
    }

    /// The host's cursor once it moves, waiting for that; `None` once the
    /// session ended.
    pub fn next_cursor(&self) -> Option<Cursor> {
        let mut cursor = self.cursor.lock().unwrap();
        match self.runtime.block_on(cursor.changed()) {
            Ok(()) => *cursor.borrow_and_update(),
            Err(_) => None,
        }
    }

    /// Smoother video (60 frames a second, less buffering), as the desktop
    /// viewer's Game Boost: more battery and data.
    pub fn game_boost(&self, enabled: bool) {
        let request = self.boosts.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self
            .control
            .send(ClientMessage::SetGameBoost { request, enabled });
    }

    /// After the decoder lost its place: the host sends a keyframe.
    pub fn request_keyframe(&self) {
        let _ = self.control.send(ClientMessage::RequestKeyframe);
    }

    /// Ends the session. A decoder waiting in [`Viewer::next_frame`] gets
    /// `None` once the connection is gone.
    pub fn close(&self) {
        self.connection.close(0u32.into(), b"viewer closed");
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        // The tasks end with the connection; the runtime goes last.
        self.close();
    }
}

/// Finds the host, connects, checks who answered and proves the code.
async fn open(options: &Options) -> Result<Opened> {
    let (endpoint, agent, address, device_id) = match target(&options.target) {
        Target::Address(text) => {
            let wanted = net::with_default_port(&text, DEFAULT_PORT);
            let address = tokio::net::lookup_host(&wanted)
                .await
                .ok()
                .and_then(|mut found| found.next())
                .ok_or_else(|| anyhow!("cannot find {text}"))?;
            (net::client_endpoint()?, None, address, None)
        }
        Target::DeviceId(id) => {
            let (socket, tap) = SharedSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))?;
            let endpoint = net::client_endpoint_on(socket.clone())?;
            let agent = Agent::spawn(socket, tap)?;
            let service = resolve_service(&options.service)
                .await
                .ok_or_else(|| anyhow!("cannot find the connection service {}", options.service))?;
            let introduction = match agent.lookup(options.service.clone(), service, id).await {
                LookupOutcome::Introduced(introduction) => introduction,
                LookupOutcome::NotFound => bail!("{id} is not online right now"),
                LookupOutcome::Unreachable(reason) => bail!("{reason}"),
            };
            let path = agent
                .punch(introduction.peer, Some(introduction.session), PUNCH_WINDOW)
                .await
                .map_err(|_| {
                    anyhow!(
                        "{id} did not answer. If both networks use a symmetric NAT (common on \
                         mobile data), let the host's router open its port (on the host: \
                         Settings, Network) or use a VPN such as Tailscale."
                    )
                })?;
            (endpoint, Some(agent), path.peer, Some(id))
        }
    };
    let connection = endpoint
        .connect(address, "tidedesk-host")?
        .await
        .with_context(|| format!("could not reach a TideDesk host at {address}"))?;
    let fingerprint = net::peer_fingerprint(&connection)
        .ok_or_else(|| anyhow!("the host showed no certificate"))?;
    match device_id {
        Some(id) => {
            if DeviceId::from_fingerprint_hex(&fingerprint) != Some(id) {
                bail!("the computer that answered is not {id}");
            }
        }
        // Trust on first use, as the desktop viewer does.
        None => {
            let key = options.target.trim();
            let mut known = KnownHosts::load(&options.data_dir)?;
            match known.check(key, &fingerprint) {
                PinStatus::Trusted => {}
                PinStatus::Unknown => known.pin(key, &fingerprint)?,
                PinStatus::Mismatch { .. } => bail!(
                    "the computer at {key} is not the one this phone met there before: its \
                     identity changed. If it was reinstalled, connect by its device ID instead."
                ),
            }
        }
    }
    let (mut send, mut recv) = connection.open_bi().await?;
    let auth_tag = auth::client_tag(&connection, &options.code)?;
    let hello = ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        client_name: options.name.clone(),
        auth_tag,
        want_audio: false,
    };
    protocol::write_message(&mut send, &hello).await?;
    match protocol::read_message(&mut recv).await? {
        Some(ServerMessage::Welcome { host_name, .. }) => Ok(Opened {
            endpoint,
            agent,
            connection,
            send,
            recv,
            host_name,
        }),
        Some(ServerMessage::Rejected { reason }) => bail!("{reason}"),
        Some(other) => bail!("unexpected answer from the host: {other:?}"),
        None => bail!("the host closed the connection"),
    }
}

async fn write_control(
    mut send: quinn::SendStream,
    mut outgoing: mpsc::UnboundedReceiver<ClientMessage>,
) {
    while let Some(message) = outgoing.recv().await {
        if protocol::write_message(&mut send, &message).await.is_err() {
            return;
        }
    }
}

async fn read_control(
    mut recv: quinn::RecvStream,
    pointer: Arc<Mutex<Pointer>>,
    control: mpsc::UnboundedSender<ClientMessage>,
    cursor: watch::Sender<Option<Cursor>>,
) {
    // The host's pointer, wherever it moved: for the phone to draw.
    let show = |position: PointerPosition| {
        let now = Some(Cursor {
            x: f32::from(position.x) / 65535.0,
            y: f32::from(position.y) / 65535.0,
            visible: position.inside,
        });
        cursor.send_if_modified(|shown| {
            let changed = *shown != now;
            *shown = now;
            changed
        });
    };
    while let Ok(Some(message)) = protocol::read_message::<_, ServerMessage>(&mut recv).await {
        let answers = match message {
            ServerMessage::PointerAnchor { request, position } => {
                show(position);
                pointer.lock().unwrap().anchored(request, position)
            }
            ServerMessage::Cursor(position) => {
                show(position);
                Vec::new()
            }
            ServerMessage::Pointer(position) => {
                show(position);
                pointer.lock().unwrap().lost()
            }
            // The cursor, clipboard, chat and the rest: not on the phone yet.
            _ => Vec::new(),
        };
        for answer in answers {
            let _ = control.send(answer);
        }
    }
}

/// The video stream: the first one the host opens. Later ones carry files,
/// which the phone does not take.
async fn read_video(connection: quinn::Connection, frames: mpsc::Sender<Frame>) {
    let Ok(mut video) = connection.accept_uni().await else {
        return;
    };
    let others = connection.clone();
    tokio::spawn(async move {
        while let Ok(mut stream) = others.accept_uni().await {
            let _ = stream.stop(0u32.into());
        }
    });
    let mut head = [0u8; VideoFrameHeader::SIZE];
    loop {
        if video.read_exact(&mut head).await.is_err() {
            return;
        }
        let header = VideoFrameHeader::decode(&head);
        let length = header.len as usize;
        if length > MAX_VIDEO_FRAME {
            return;
        }
        let mut data = vec![0; length];
        if video.read_exact(&mut data).await.is_err() {
            return;
        }
        let frame = Frame {
            width: header.width,
            height: header.height,
            keyframe: header.keyframe,
            capture_us: header.capture_us,
            data,
        };
        if frames.send(frame).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use tidedesk_core::identity::HostIdentity;
    use tidedesk_core::nat::SharedSocket;
    use tidedesk_core::protocol::{
        self, ClientMessage, InputEvent, PROTOCOL_VERSION, RejectReason, ServerMessage,
        VideoFrameHeader,
    };
    use tidedesk_core::sharing::{PointerPosition, SharingState};
    use tidedesk_core::{auth, net};

    const CODE: &str = "ABCD-EFGH-JK";

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tidedesk-android-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn keyframe() -> Vec<u8> {
        // SPS, PPS and the start of an IDR slice: only the bytes matter here.
        vec![
            0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x65, 0x88,
        ]
    }

    /// A host on the loopback that speaks the real protocol: checks the
    /// code, sends two frames, grants the mouse and keeps the mouse input
    /// it got.
    fn fake_host(
        runtime: &tokio::runtime::Runtime,
    ) -> (SocketAddr, Arc<Mutex<Vec<ClientMessage>>>) {
        let identity = HostIdentity::load_or_create(&temp("host")).unwrap();
        let _guard = runtime.enter();
        let (socket, _tap) = SharedSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = socket.local_addr().unwrap();
        let endpoint = net::server_endpoint_on(socket, &identity).unwrap();
        let got = Arc::new(Mutex::new(Vec::new()));
        let log = got.clone();
        runtime.spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let Ok(conn) = incoming.await else { continue };
                let log = log.clone();
                tokio::spawn(async move {
                    let (mut send, mut recv) = conn.accept_bi().await.unwrap();
                    let hello: ClientMessage =
                        protocol::read_message(&mut recv).await.unwrap().unwrap();
                    let ClientMessage::Hello {
                        auth_tag,
                        want_audio,
                        ..
                    } = hello
                    else {
                        panic!("Hello first");
                    };
                    assert!(!want_audio, "no audio on the phone");
                    if !auth::verify_tag(&conn, CODE, &auth_tag).unwrap() {
                        let reason = RejectReason::BadCode;
                        protocol::write_message(&mut send, &ServerMessage::Rejected { reason })
                            .await
                            .unwrap();
                        let _ = send.finish();
                        conn.closed().await;
                        return;
                    }
                    let welcome = ServerMessage::Welcome {
                        host_name: "Office PC".into(),
                        width: 1920,
                        height: 1080,
                        audio: false,
                    };
                    protocol::write_message(&mut send, &welcome).await.unwrap();
                    // Where the host's pointer is: a quarter across, three quarters down.
                    let cursor = ServerMessage::Cursor(PointerPosition {
                        epoch: 7,
                        x: 16384,
                        y: 49151,
                        inside: true,
                    });
                    protocol::write_message(&mut send, &cursor).await.unwrap();
                    let mut video = conn.open_uni().await.unwrap();
                    for (keyframe, data) in
                        [(true, keyframe()), (false, vec![0, 0, 0, 1, 0x41, 0x9A])]
                    {
                        let header = VideoFrameHeader {
                            len: data.len() as u32,
                            width: 1920,
                            height: 1080,
                            keyframe,
                            capture_us: 1000,
                        };
                        video.write_all(&header.encode()).await.unwrap();
                        video.write_all(&data).await.unwrap();
                    }
                    while let Ok(Some(message)) =
                        protocol::read_message::<_, ClientMessage>(&mut recv).await
                    {
                        let answer = match &message {
                            ClientMessage::SetSharing {
                                request,
                                clipboard,
                                mouse,
                            } => Some(ServerMessage::Sharing(SharingState {
                                request: *request,
                                generation: 1,
                                clipboard: *clipboard,
                                mouse: *mouse,
                            })),
                            ClientMessage::PointerSync { request } => {
                                Some(ServerMessage::PointerAnchor {
                                    request: *request,
                                    position: PointerPosition {
                                        epoch: 7,
                                        x: 100,
                                        y: 100,
                                        inside: true,
                                    },
                                })
                            }
                            _ => None,
                        };
                        log.lock().unwrap().push(message);
                        if let Some(answer) = answer {
                            protocol::write_message(&mut send, &answer).await.unwrap();
                        }
                    }
                });
            }
        });
        (address, got)
    }

    fn options(target: SocketAddr, code: &str) -> Options {
        Options {
            target: target.to_string(),
            code: code.into(),
            name: "Ana's phone".into(),
            data_dir: temp("phone"),
            service: "127.0.0.1:9".into(),
        }
    }

    /// Waits until the host got `count` messages that `wanted` picks.
    fn wait_for(
        got: &Mutex<Vec<ClientMessage>>,
        count: usize,
        wanted: fn(&ClientMessage) -> bool,
    ) -> Vec<ClientMessage> {
        for _ in 0..200 {
            let picked: Vec<_> = got
                .lock()
                .unwrap()
                .iter()
                .filter(|m| wanted(m))
                .cloned()
                .collect();
            if picked.len() >= count {
                return picked;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the host never got them: {:?}", got.lock().unwrap());
    }

    #[test]
    fn a_session_brings_the_video_and_a_tap_clicks_where_it_lands() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (host, got) = fake_host(&runtime);
        let viewer = Viewer::connect(&options(host, CODE)).unwrap();
        assert_eq!(viewer.host_name(), "Office PC");

        let first = viewer.next_frame().unwrap();
        assert_eq!(
            (first.width, first.height, first.keyframe),
            (1920, 1080, true)
        );
        assert_eq!(first.data, keyframe());
        assert!(!viewer.next_frame().unwrap().keyframe);

        // The host's cursor, drawn by the phone since it is not in the video.
        let cursor = viewer.next_cursor().unwrap();
        assert!((cursor.x - 0.25).abs() < 0.001 && (cursor.y - 0.75).abs() < 0.001);
        assert!(cursor.visible);

        // A tap in the middle: move there, press, release.
        viewer.pointer(0.5, 0.5);
        viewer.button(MouseButton::Left, true);
        viewer.button(MouseButton::Left, false);
        let mouse = wait_for(&got, 3, |m| matches!(m, ClientMessage::MouseInput { .. }));
        assert_eq!(
            mouse,
            [
                ClientMessage::MouseInput {
                    epoch: 7,
                    event: InputEvent::MouseMove { x: 32768, y: 32768 }
                },
                ClientMessage::MouseInput {
                    epoch: 7,
                    event: InputEvent::MouseButton {
                        button: MouseButton::Left,
                        pressed: true
                    }
                },
                ClientMessage::MouseInput {
                    epoch: 7,
                    event: InputEvent::MouseButton {
                        button: MouseButton::Left,
                        pressed: false
                    }
                },
            ]
        );
        // The mouse was asked for first, and the host's pointer read once.
        let sent = got.lock().unwrap().clone();
        assert!(matches!(
            sent[0],
            ClientMessage::SetSharing {
                mouse: true,
                clipboard: false,
                ..
            }
        ));
        assert_eq!(
            sent.iter()
                .filter(|m| matches!(m, ClientMessage::PointerSync { .. }))
                .count(),
            1
        );

        // Keys go as they are, with no pointer epoch.
        viewer.key(0x1E, true);
        viewer.key(0x1E, false);
        let keys = wait_for(&got, 2, |m| {
            matches!(m, ClientMessage::Input(InputEvent::Key { .. }))
        });
        assert_eq!(
            keys,
            [
                ClientMessage::Input(InputEvent::Key {
                    scancode: 0x1E,
                    pressed: true
                }),
                ClientMessage::Input(InputEvent::Key {
                    scancode: 0x1E,
                    pressed: false
                }),
            ]
        );

        // Smoother video, as the desktop's Game Boost: each change its own request.
        viewer.game_boost(true);
        viewer.game_boost(false);
        let boosts = wait_for(&got, 2, |m| matches!(m, ClientMessage::SetGameBoost { .. }));
        assert_eq!(
            boosts,
            [
                ClientMessage::SetGameBoost {
                    request: 1,
                    enabled: true
                },
                ClientMessage::SetGameBoost {
                    request: 2,
                    enabled: false
                },
            ]
        );

        viewer.wheel(2);
        viewer.request_keyframe();
        viewer.close();
        assert_eq!(viewer.next_frame(), None, "nothing after the end");
        // Moves not yet drawn come first, then the end.
        let mut pending = 0;
        while viewer.next_cursor().is_some() {
            pending += 1;
            assert!(pending < 10, "the cursor ends with the session");
        }
        drop(runtime);
    }

    #[test]
    fn a_wrong_code_says_what_the_host_said() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (host, _) = fake_host(&runtime);
        let refused = Viewer::connect(&options(host, "WRON-GCOD-EX"))
            .err()
            .unwrap();
        assert_eq!(refused.to_string(), RejectReason::BadCode.to_string());
        let _ = PROTOCOL_VERSION;
    }

    #[test]
    fn a_device_id_must_be_one() {
        assert!(matches!(
            target("TD-1A2B-3C4D-5E6F-7A8B"),
            Target::DeviceId(_)
        ));
        assert!(matches!(
            target("td 1a2b 3c4d 5e6f 7a8b"),
            Target::DeviceId(_)
        ));
        assert_eq!(
            target("192.168.1.50"),
            Target::Address("192.168.1.50".into())
        );
        assert_eq!(
            target(" office-pc:47801 "),
            Target::Address("office-pc:47801".into())
        );
    }
}
