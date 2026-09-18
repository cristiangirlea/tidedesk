//! Network side of a session: video receive + decode, audio receive, control.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::formats::YUVSource;
use tidedesk_core::protocol::{
    self, ClientMessage, MAX_VIDEO_FRAME, ServerMessage, VideoFrameHeader,
};
use tokio::sync::mpsc;

use crate::playback::AudioSink;

/// The most recent decoded picture, as 0RGB pixels.
#[derive(Default)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
    pub redraw_pending: bool,
}

#[derive(Debug)]
pub enum UiEvent {
    NewPicture,
    Control(ServerMessage),
    Disconnected(String),
}

pub trait Notify: Send + Sync + 'static {
    fn notify(&self, event: UiEvent);
}

const VIDEO_QUEUE: usize = 8;
type EncodedFrame = (VideoFrameHeader, Vec<u8>);

fn skip_conversion(game_boost: bool, backlog: bool, skipped: &mut u8) -> bool {
    if game_boost && backlog && *skipped < 2 {
        *skipped += 1;
        true
    } else {
        // Sustained overload must never suppress presentation indefinitely.
        *skipped = 0;
        false
    }
}

async fn enqueue_frame(
    tx: &mpsc::Sender<EncodedFrame>,
    frame: EncodedFrame,
    game_boost: bool,
) -> bool {
    if game_boost {
        // Wait for all queue slots, then use just one. This reduces the live
        // queue to one pending frame without dropping dependent H.264 frames.
        // Any old desktop backlog is decoded in order before this proceeds.
        let Ok(mut permits) = tx.reserve_many(VIDEO_QUEUE).await else {
            return false;
        };
        permits.next().unwrap().send(frame);
        true
    } else {
        tx.send(frame).await.is_ok()
    }
}

/// Receives encoded frames and decodes them on a dedicated thread (H.264
/// frames depend on each other, so every one is decoded; only presentation is
/// coalesced to the latest).
pub async fn video_loop(
    mut stream: quinn::RecvStream,
    picture: Arc<Mutex<Picture>>,
    control: mpsc::UnboundedSender<ClientMessage>,
    ui: Arc<dyn Notify>,
    stats: bool,
    game_boost: Arc<AtomicBool>,
) -> Result<()> {
    let (tx, rx) = mpsc::channel::<EncodedFrame>(VIDEO_QUEUE);
    let decoder_ui = ui.clone();
    let decoder_boost = game_boost.clone();
    std::thread::Builder::new()
        .name("decoder".into())
        .spawn(move || decode_thread(rx, picture, control, decoder_ui, stats, decoder_boost))?;

    let mut header = [0u8; VideoFrameHeader::SIZE];
    loop {
        match stream.read_exact(&mut header).await {
            Ok(_) => {}
            Err(quinn::ReadExactError::FinishedEarly(_)) => break,
            Err(e) => return Err(e).context("reading video stream"),
        }
        let h = VideoFrameHeader::decode(&header);
        if h.len as usize > MAX_VIDEO_FRAME {
            bail!("video frame of {} bytes exceeds limit", h.len);
        }
        let mut data = vec![0u8; h.len as usize];
        stream.read_exact(&mut data).await?;
        if !enqueue_frame(&tx, (h, data), game_boost.load(Ordering::Relaxed)).await {
            break;
        }
    }
    Ok(())
}

fn decode_thread(
    mut rx: mpsc::Receiver<(VideoFrameHeader, Vec<u8>)>,
    picture: Arc<Mutex<Picture>>,
    control: mpsc::UnboundedSender<ClientMessage>,
    ui: Arc<dyn Notify>,
    stats: bool,
    game_boost: Arc<AtomicBool>,
) {
    let mut decoder =
        match Decoder::with_api_config(OpenH264API::from_source(), DecoderConfig::new()) {
            Ok(d) => d,
            Err(e) => {
                ui.notify(UiEvent::Disconnected(format!(
                    "cannot start video decoder: {e}"
                )));
                return;
            }
        };
    let mut rgb = Vec::new();
    let mut spare: Vec<u32> = Vec::new();
    let mut awaiting_keyframe = false;
    let mut skipped_conversions = 0;
    let mut meter = tidedesk_core::stats::Meter::new("viewer video (decode+convert)");

    while let Some((header, data)) = rx.blocking_recv() {
        if awaiting_keyframe && !header.keyframe {
            continue;
        }
        awaiting_keyframe = false;
        let work_start = std::time::Instant::now();
        let decoded = match decoder.decode(&data) {
            Ok(Some(yuv)) => yuv,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!("video decode error, requesting keyframe: {e}");
                awaiting_keyframe = true;
                let _ = control.send(ClientMessage::RequestKeyframe);
                continue;
            }
        };
        let (w, h) = decoded.dimensions();
        // Retain decoder reference state, but don't spend RGB conversion time
        // presenting a stale frame while a newer one already awaits decoding.
        if skip_conversion(
            game_boost.load(Ordering::Relaxed),
            !rx.is_empty(),
            &mut skipped_conversions,
        ) {
            if stats && let Some(line) = meter.record(data.len(), work_start.elapsed()) {
                tracing::info!("{line}");
            }
            continue;
        }
        rgb.resize(w * h * 3, 0);
        decoded.write_rgb8(&mut rgb);
        spare.clear();
        spare.extend(
            rgb.as_chunks::<3>()
                .0
                .iter()
                .map(|p| (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32),
        );
        let notify = {
            let mut pic = picture.lock().unwrap();
            pic.width = w as u32;
            pic.height = h as u32;
            std::mem::swap(&mut pic.pixels, &mut spare);
            let notify = !pic.redraw_pending;
            pic.redraw_pending = true;
            notify
        };
        if stats && let Some(line) = meter.record(data.len(), work_start.elapsed()) {
            tracing::info!("{line}");
        }
        if notify {
            ui.notify(UiEvent::NewPicture);
        }
    }
}

pub async fn audio_loop(conn: quinn::Connection, mut sink: AudioSink) -> Result<()> {
    loop {
        let datagram = conn.read_datagram().await?;
        if let Some((seq, packet)) = protocol::decode_audio_datagram(&datagram) {
            sink.push_packet(seq, packet);
        }
    }
}

pub async fn control_writer(
    mut send: quinn::SendStream,
    mut rx: mpsc::UnboundedReceiver<ClientMessage>,
) -> Result<()> {
    let mut pending = None;
    while let Some(msg) = match pending.take() {
        Some(msg) => Some(msg),
        None => rx.recv().await,
    } {
        let (msg, next) = latest_motion(msg, &mut rx);
        pending = next;
        protocol::write_message(&mut send, &msg).await?;
    }
    Ok(())
}

/// Collapse only adjacent absolute moves from the same pointer epoch. Keyboard,
/// button, wheel, handoff and permission messages are strict ordering barriers.
fn latest_motion(
    mut message: ClientMessage,
    rx: &mut mpsc::UnboundedReceiver<ClientMessage>,
) -> (ClientMessage, Option<ClientMessage>) {
    use tidedesk_core::protocol::InputEvent;
    let ClientMessage::MouseInput {
        epoch,
        event: InputEvent::MouseMove { .. },
    } = message
    else {
        return (message, None);
    };
    // Bound each batch so a constantly moving mouse cannot starve the writer.
    for _ in 0..256 {
        let Ok(next) = rx.try_recv() else { break };
        if matches!(next, ClientMessage::MouseInput {
            epoch: next_epoch, event: InputEvent::MouseMove { .. }
        } if next_epoch == epoch)
        {
            message = next;
        } else {
            return (message, Some(next));
        }
    }
    (message, None)
}

/// Receives live sharing permissions, clipboard text and pointer handoffs.
pub async fn control_reader(mut recv: quinn::RecvStream, ui: Arc<dyn Notify>) -> Result<()> {
    while let Some(message) = protocol::read_message::<_, ServerMessage>(&mut recv).await? {
        if matches!(
            message,
            ServerMessage::Welcome { .. } | ServerMessage::Rejected { .. }
        ) {
            bail!("unexpected handshake message during session");
        }
        if let ServerMessage::Clipboard { text, .. } = &message
            && !tidedesk_core::clipboard::valid_text(text)
        {
            bail!("invalid clipboard text");
        }
        ui.notify(UiEvent::Control(message));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn frame(n: u8) -> EncodedFrame {
        (
            VideoFrameHeader {
                len: 1,
                width: 128,
                height: 96,
                keyframe: n == 0,
                capture_us: n as u64,
            },
            vec![n],
        )
    }

    #[test]
    fn sustained_decode_backlog_never_starves_presentation() {
        let mut skipped = 0;
        for _ in 0..10 {
            assert!(skip_conversion(true, true, &mut skipped));
            assert!(skip_conversion(true, true, &mut skipped));
            assert!(!skip_conversion(true, true, &mut skipped));
        }
        assert!(!skip_conversion(true, false, &mut skipped));
        assert!(skip_conversion(true, true, &mut skipped));
        assert!(!skip_conversion(false, true, &mut skipped));
        assert_eq!(skipped, 0);
    }

    #[test]
    fn mouse_backlog_coalesces_without_crossing_key_button_or_epoch_barriers() {
        use tidedesk_core::protocol::{InputEvent, MouseButton};
        let movement = |epoch, x| ClientMessage::MouseInput {
            epoch,
            event: InputEvent::MouseMove { x, y: 0 },
        };
        for barrier in [
            ClientMessage::Input(InputEvent::Key {
                scancode: 17,
                pressed: false,
            }),
            ClientMessage::MouseInput {
                epoch: 1,
                event: InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                },
            },
            ClientMessage::ReleaseMouse,
            movement(2, 900),
        ] {
            let (tx, mut rx) = mpsc::unbounded_channel();
            tx.send(movement(1, 20)).unwrap();
            tx.send(movement(1, 30)).unwrap();
            tx.send(barrier.clone()).unwrap();
            tx.send(movement(1, 40)).unwrap();
            let (sent, pending) = latest_motion(movement(1, 10), &mut rx);
            assert_eq!(sent, movement(1, 30));
            assert_eq!(pending, Some(barrier));
            assert_eq!(rx.try_recv().unwrap(), movement(1, 40));
        }
    }

    #[tokio::test]
    async fn boost_bounds_queue_and_preserves_dependent_frame_order() {
        let (tx, mut rx) = mpsc::channel(VIDEO_QUEUE);
        for n in 0..8 {
            assert!(enqueue_frame(&tx, frame(n), false).await);
        }
        let send = enqueue_frame(&tx, frame(8), true);
        tokio::pin!(send);
        for n in 0..8 {
            assert!(
                tokio::time::timeout(Duration::from_millis(1), &mut send)
                    .await
                    .is_err()
            );
            assert_eq!(rx.recv().await.unwrap().1, vec![n]);
        }
        assert!(send.await);
        assert_eq!(rx.len(), 1);
        let send = enqueue_frame(&tx, frame(9), true);
        tokio::pin!(send);
        assert!(
            tokio::time::timeout(Duration::from_millis(1), &mut send)
                .await
                .is_err()
        );
        assert_eq!(rx.recv().await.unwrap().1, vec![8]);
        assert!(send.await);
        assert_eq!(rx.recv().await.unwrap().1, vec![9]);
        drop(rx);
        assert!(!enqueue_frame(&tx, frame(10), true).await);
    }
}
