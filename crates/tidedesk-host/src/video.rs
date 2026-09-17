//! Capture → H.264 pipeline, run on its own thread.
//!
//! Latency comes from queues, so this pipeline keeps none: it only encodes when
//! the network task is ready for another frame. While the network is busy the
//! capturer keeps overwriting its single frame buffer, and whatever is newest
//! gets encoded next.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, RateControlMode, UsageType,
};
use openh264::formats::{BgraSliceU8, YUVBuffer, YUVSource};
use tidedesk_core::protocol::VideoFrameHeader;
use tokio::sync::mpsc::error::TrySendError;

use crate::capture::{self, DisplayRect};

pub struct EncodedFrame {
    pub header: VideoFrameHeader,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
pub struct VideoSettings {
    pub display: usize,
    pub fps: u32,
    pub bitrate_bps: u32,
    pub stats: bool,
}

/// Size and position of the display being streamed, reported once at start-up.
#[derive(Debug, Clone, Copy)]
pub struct StreamInfo {
    pub width: u32,
    pub height: u32,
    pub rect: DisplayRect,
}

pub struct VideoControl {
    pub stop: AtomicBool,
    pub keyframe: AtomicBool,
}

/// Starts capture and encoding, returning once the display is open.
pub fn start(
    settings: VideoSettings,
    frames: tokio::sync::mpsc::Sender<EncodedFrame>,
    control: Arc<VideoControl>,
) -> Result<StreamInfo> {
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("video".into())
        .spawn(move || {
            // The capturer holds COM objects bound to this thread, so it is
            // created here and never leaves.
            let capturer = match capture::open(settings.display) {
                Ok(c) => c,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            if let Err(e) = run(capturer, settings, frames, &control, ready_tx) {
                tracing::error!("video pipeline stopped: {e:#}");
            }
        })?;
    ready_rx
        .recv()
        .context("video thread exited during start-up")?
}

fn run(
    mut capturer: Box<dyn capture::Capturer>,
    settings: VideoSettings,
    frames: tokio::sync::mpsc::Sender<EncodedFrame>,
    control: &VideoControl,
    ready: std::sync::mpsc::SyncSender<Result<StreamInfo>>,
) -> Result<()> {
    let rect = capturer.rect();
    let _ = ready.send(Ok(StreamInfo {
        width: (rect.width as u32) & !1,
        height: (rect.height as u32) & !1,
        rect,
    }));

    let config = EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .bitrate(BitRate::from_bps(settings.bitrate_bps))
        .max_frame_rate(FrameRate::from_hz(settings.fps as f32))
        .complexity(Complexity::Low)
        .skip_frames(false)
        // Two threads keep 1080p/4K real-time without taking over the host.
        .num_threads(2);
    let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), config)?;
    let mut yuv: Option<YUVBuffer> = None;
    let mut bitstream = Vec::new();

    let interval = Duration::from_secs_f64(1.0 / settings.fps.max(1) as f64);
    let epoch = Instant::now();
    let mut next_due = Instant::now();
    let mut pending = false; // captured but not yet encoded
    let mut meter = tidedesk_core::stats::Meter::new("host video (capture+encode)");

    while !control.stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now < next_due {
            std::thread::sleep(next_due - now);
        }

        // When a frame is already pending we only drain newer ones; otherwise
        // block until the desktop changes (this is where an idle host sleeps).
        let wait = if pending {
            Duration::ZERO
        } else {
            Duration::from_millis(100)
        };
        if capturer.next_frame(wait)? {
            pending = true;
        }
        if !pending {
            continue;
        }

        let permit = match frames.try_reserve() {
            Ok(p) => p,
            Err(TrySendError::Full(())) => {
                // Network still busy with the previous frame.
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            Err(TrySendError::Closed(())) => break,
        };

        let work_start = Instant::now();
        let frame = capturer.frame();
        let (w, h) = (frame.width, frame.height);
        let buf = match &mut yuv {
            Some(b) if b.dimensions() == (w, h) => b,
            slot => slot.insert(YUVBuffer::new(w, h)),
        };
        buf.read_bgra8(BgraSliceU8::new(&frame.bgra, (w, h)));

        if control.keyframe.swap(false, Ordering::Relaxed) {
            encoder.force_intra_frame();
        }
        let capture_us = epoch.elapsed().as_micros() as u64;
        let encoded = encoder.encode(buf)?;
        let keyframe = matches!(encoded.frame_type(), FrameType::IDR | FrameType::I);
        bitstream.clear();
        encoded.write_vec(&mut bitstream);
        pending = false;
        next_due = next_due.max(now) + interval;

        if settings.stats
            && let Some(line) = meter.record(bitstream.len(), work_start.elapsed())
        {
            tracing::info!("{line}");
        }
        if bitstream.is_empty() {
            continue;
        }
        permit.send(EncodedFrame {
            header: VideoFrameHeader {
                len: bitstream.len() as u32,
                width: w as u16,
                height: h as u16,
                keyframe,
                capture_us,
            },
            data: std::mem::take(&mut bitstream),
        });
    }
    Ok(())
}
