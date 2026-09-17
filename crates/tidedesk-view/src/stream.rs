//! Network side of a session: video receive + decode, audio receive, control.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::formats::YUVSource;
use tidedesk_core::protocol::{self, ClientMessage, MAX_VIDEO_FRAME, VideoFrameHeader};
use tokio::sync::mpsc;

use crate::playback::AudioSink;

/// The most recent decoded picture, as 0RGB pixels.
#[derive(Default)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

#[derive(Debug)]
pub enum UiEvent {
    NewPicture,
    Disconnected(String),
}

pub trait Notify: Send + Sync + 'static {
    fn notify(&self, event: UiEvent);
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
) -> Result<()> {
    let (tx, rx) = mpsc::channel::<(VideoFrameHeader, Vec<u8>)>(8);
    let decoder_ui = ui.clone();
    std::thread::Builder::new()
        .name("decoder".into())
        .spawn(move || decode_thread(rx, picture, control, decoder_ui, stats))?;

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
        // Waiting here is fine: the decoder is far faster than the network.
        if tx.send((h, data)).await.is_err() {
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
        rgb.resize(w * h * 3, 0);
        decoded.write_rgb8(&mut rgb);
        spare.clear();
        spare.extend(
            rgb.as_chunks::<3>()
                .0
                .iter()
                .map(|p| (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32),
        );
        {
            let mut pic = picture.lock().unwrap();
            pic.width = w as u32;
            pic.height = h as u32;
            std::mem::swap(&mut pic.pixels, &mut spare);
        }
        if stats && let Some(line) = meter.record(data.len(), work_start.elapsed()) {
            tracing::info!("{line}");
        }
        ui.notify(UiEvent::NewPicture);
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
    while let Some(msg) = rx.recv().await {
        protocol::write_message(&mut send, &msg).await?;
    }
    Ok(())
}

/// The host sends nothing after `Welcome`; this just notices the stream end.
pub async fn control_reader(mut recv: quinn::RecvStream) -> Result<()> {
    let mut buf = [0u8; 256];
    while recv.read(&mut buf).await?.is_some_and(|n| n > 0) {}
    Ok(())
}
