//! Capture → H.264 pipeline, run on its own thread.
//!
//! Latency comes from queues, so this pipeline keeps none: it only encodes when
//! the network task is ready for another frame. While the network is busy the
//! capturer keeps overwriting its single frame buffer, and whatever is newest
//! gets encoded next.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, RateControlMode, UsageType,
};
use openh264::formats::{BgraSliceU8, YUVBuffer, YUVSource};
use openh264::{OpenH264API, Timestamp};
use tidedesk_core::protocol::VideoFrameHeader;
use tidedesk_core::streaming::StreamingStatus;
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

#[derive(Default)]
pub struct VideoControl {
    pub stop: AtomicBool,
    pub keyframe: AtomicBool,
    pub boost_request: Mutex<(u64, bool)>,
    pub streaming_status: Mutex<Option<StreamingStatus>>,
}

fn encoder_for(status: StreamingStatus) -> Result<Encoder> {
    let config = EncoderConfig::new()
        .usage_type(if status.game_boost {
            UsageType::CameraVideoRealTime
        } else {
            UsageType::ScreenContentRealTime
        })
        .rate_control_mode(RateControlMode::Bitrate)
        .bitrate(BitRate::from_bps(status.bitrate_bps))
        .max_frame_rate(FrameRate::from_hz(status.fps as f32))
        .complexity(Complexity::Low)
        // Let the encoder omit frames to respect the host's bandwidth budget.
        .skip_frames(status.game_boost)
        .num_threads(if status.game_boost { 4 } else { 2 });
    Ok(Encoder::with_api_config(
        OpenH264API::from_source(),
        config,
    )?)
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

    let mut status = StreamingStatus::requested(0, false, settings.fps, settings.bitrate_bps);
    let mut encoder = encoder_for(status)?;
    let mut reported_request = 0;
    let mut last_reconfigure: Option<Instant> = None;
    let mut yuv: Option<YUVBuffer> = None;
    let mut bitstream = Vec::new();

    let mut interval = Duration::from_secs_f64(1.0 / status.fps as f64);
    let epoch = Instant::now();
    let mut next_due = Instant::now();
    let mut pending = false; // captured but not yet encoded
    let mut meter = tidedesk_core::stats::Meter::new("host video (capture+encode)");

    while !control.stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        let (request, enabled) = *control.boost_request.lock().unwrap();
        if request != status.request
            && last_reconfigure.is_none_or(|t| t.elapsed() >= Duration::from_millis(250))
        {
            let next =
                StreamingStatus::requested(request, enabled, settings.fps, settings.bitrate_bps);
            if next.game_boost != status.game_boost {
                // A fresh encoder starts with SPS/PPS + IDR; never splice new
                // prediction state onto the old stream's dependent frames.
                encoder = encoder_for(next)?;
                interval = Duration::from_secs_f64(1.0 / next.fps as f64);
                next_due = now;
            } else if yuv.is_some() {
                // A repeated request must also complete on a static screen,
                // even if rate control would otherwise skip the refresh.
                encoder.force_intra_frame();
            }
            last_reconfigure = Some(now);
            status = next;
            // Re-encode the current picture even on an idle desktop so the
            // viewer gets a real frame and acknowledgement of this preset.
            pending |= yuv.is_some();
        }
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
        let encoded = encoder.encode_at(buf, Timestamp::from_millis(capture_us / 1000))?;
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
        if reported_request != status.request {
            *control.streaming_status.lock().unwrap() = Some(status);
            reported_request = status.request;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openh264::decoder::Decoder;

    /// Exercises the production capture/encode loop without capturing the user's
    /// desktop. Switching must also work when the captured desktop is static.
    #[tokio::test]
    async fn idle_session_applies_boost_and_restores_desktop_without_reconnecting() {
        struct StaticCapture {
            frame: capture::Frame,
            first: bool,
        }
        impl capture::Capturer for StaticCapture {
            fn next_frame(&mut self, timeout: Duration) -> Result<bool> {
                if std::mem::take(&mut self.first) {
                    return Ok(true);
                }
                std::thread::sleep(timeout);
                Ok(false)
            }
            fn frame(&self) -> &capture::Frame {
                &self.frame
            }
            fn rect(&self) -> DisplayRect {
                DisplayRect {
                    left: -128,
                    top: 0,
                    width: 128,
                    height: 96,
                }
            }
        }
        let control = Arc::new(VideoControl::default());
        struct Stop(Arc<VideoControl>);
        impl Drop for Stop {
            fn drop(&mut self) {
                self.0.stop.store(true, Ordering::Relaxed);
            }
        }
        let stop = Stop(control.clone());
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let worker_control = control.clone();
        let worker = std::thread::spawn(move || {
            run(
                Box::new(StaticCapture {
                    frame: capture::Frame {
                        width: 128,
                        height: 96,
                        bgra: vec![90; 128 * 96 * 4],
                    },
                    first: true,
                }),
                VideoSettings {
                    display: 0,
                    fps: 24,
                    bitrate_bps: 4_000_000,
                    stats: false,
                },
                tx,
                &worker_control,
                ready_tx,
            )
        });
        let info = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!((info.width, info.height, info.rect.left), (128, 96, -128));
        let mut decoder = Decoder::new().unwrap();
        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(decoder.decode(&first.data).unwrap().is_some());
        for (request, enabled, fps) in
            [(1, true, 60), (2, true, 60), (3, false, 24), (4, false, 24)]
        {
            *control.boost_request.lock().unwrap() = (request, enabled);
            let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!((frame.header.width, frame.header.height), (128, 96));
            assert!(frame.header.keyframe);
            assert_eq!(
                decoder.decode(&frame.data).unwrap().unwrap().dimensions(),
                (128, 96)
            );
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(status) = control.streaming_status.lock().unwrap().take() {
                        assert_eq!(
                            status,
                            StreamingStatus {
                                request,
                                game_boost: enabled,
                                fps,
                                bitrate_bps: 4_000_000,
                            }
                        );
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
        }
        drop(stop);
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn live_preset_changes_produce_decodable_keyframes_without_resizing() {
        let mut decoder = Decoder::new().unwrap();
        let pixels = vec![90; 128 * 96 * 4];
        let mut yuv = YUVBuffer::new(128, 96);
        yuv.read_bgra8(BgraSliceU8::new(&pixels, (128, 96)));
        for enabled in [false, true, false] {
            let mut encoder =
                encoder_for(StreamingStatus::requested(1, enabled, 30, 4_000_000)).unwrap();
            let encoded = encoder.encode(&yuv).unwrap();
            assert!(matches!(
                encoded.frame_type(),
                FrameType::IDR | FrameType::I
            ));
            let mut bytes = Vec::new();
            encoded.write_vec(&mut bytes);
            let decoded = decoder.decode(&bytes).unwrap().unwrap();
            assert_eq!(decoded.dimensions(), (128, 96));
        }
    }
}
