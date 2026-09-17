//! One viewer connection, from handshake to teardown.

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

pub struct HostState {
    pub code: String,
    pub host_name: String,
    pub video: VideoSettings,
    pub audio: bool,
    pub throttle: Mutex<Throttle>,
    pub busy: AtomicBool,
}

/// Clears the busy flag however the session ends.
struct BusyGuard<'a>(&'a AtomicBool);
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
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
    if state.throttle.lock().unwrap().is_locked(Instant::now()) {
        return reject(&mut send, &conn, RejectReason::TooManyAttempts).await;
    }
    if !auth::verify_tag(&conn, &state.code, &auth_tag)? {
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
    let _busy = BusyGuard(&state.busy);
    tracing::info!("viewer \"{client_name}\" connected from {remote}");

    // Video.
    let control = Arc::new(VideoControl {
        stop: AtomicBool::new(false),
        keyframe: AtomicBool::new(false),
    });
    let stop_video = StopOnDrop(&control.stop);
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel(1);
    let video_settings = state.video;
    let control2 = control.clone();
    let info =
        tokio::task::spawn_blocking(move || video::start(video_settings, frame_tx, control2))
            .await??;

    // Audio.
    let audio_stop = Arc::new(AtomicBool::new(false));
    let _stop_audio = StopOnDrop(&audio_stop);
    let (audio_tx, mut audio_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
    let mut audio_on = false;
    if state.audio && want_audio {
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
    let control_task = async {
        while let Some(msg) = protocol::read_message::<_, ClientMessage>(&mut recv).await? {
            match msg {
                ClientMessage::Input(ev) => injector.inject(ev)?,
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
