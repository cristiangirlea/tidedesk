//! Capture → H.264 pipeline, run on its own thread.
//!
//! Latency comes from queues, so this pipeline keeps none: it only encodes when
//! the network task is ready for another frame. While the network is busy the
//! capturer keeps overwriting its single frame buffer, and whatever is newest
//! gets encoded next.
//!
//! A graphics card's encoder that works on two pictures at once gets the
//! next one while it encodes the last, so that the time it takes over each
//! (15 to 30 ms at desktop sizes) does not bound the frame rate. Frames still
//! leave in the order their pictures were taken, and none is left out: the
//! viewer needs each one to decode the next.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tidedesk_codec::{Encoder, Image, Implementation, Received};
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
    /// `None`: set by the screen's size ([`automatic_bitrate`]).
    pub bitrate_bps: Option<u32>,
    pub stats: bool,
}

/// The bitrate for a screen when none is chosen: about 0.1 bit per pixel at
/// 30 frames a second (6.2 Mbit/s at 1080p, 12.3 at 2560x1600), never below
/// the earlier fixed 4 Mbit/s nor above the settings' most. It depends on the
/// size alone, so Game Boost keeps it: the viewer cannot raise the budget.
pub(crate) fn automatic_bitrate((width, height): (usize, usize)) -> u32 {
    let most = *crate::config::HostConfig::BITRATE_RANGE.end() as usize * 1000;
    (width * height * 3).max(4_000_000).min(most) as u32
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

/// A picture in the encoder: what its frame needs once it comes out.
struct Flying {
    capture_us: u64,
    began: Instant,
    status: StreamingStatus,
}

/// The picture of `timestamp_ms`, which the encoder gave back. Pictures
/// before it that an encoder failed with never come back.
fn landed(flying: &mut VecDeque<Flying>, timestamp_ms: u64) -> Option<Flying> {
    loop {
        let picture = flying.pop_front()?;
        if picture.capture_us / 1000 == timestamp_ms {
            return Some(picture);
        }
    }
}

/// What the pipeline asks of an encoder; see [`Encoder`].
trait Encode {
    fn implementation(&self) -> Implementation;
    fn force_keyframe(&mut self);
    fn depth(&self) -> usize;
    fn in_flight(&self) -> usize;
    fn send(&mut self, image: Image<'_>, size: (usize, usize), timestamp_ms: u64) -> Result<()>;
    fn receive(&mut self, wait: Duration, out: &mut Vec<u8>) -> Result<Received>;
}

impl Encode for Encoder {
    fn implementation(&self) -> Implementation {
        self.implementation()
    }
    fn force_keyframe(&mut self) {
        self.force_keyframe()
    }
    fn depth(&self) -> usize {
        self.depth()
    }
    fn in_flight(&self) -> usize {
        self.in_flight()
    }
    fn send(&mut self, image: Image<'_>, size: (usize, usize), timestamp_ms: u64) -> Result<()> {
        self.send(image, size, timestamp_ms)
    }
    fn receive(&mut self, wait: Duration, out: &mut Vec<u8>) -> Result<Received> {
        self.receive(wait, out)
    }
}

fn encoder_for(status: StreamingStatus) -> Result<Encoder> {
    Encoder::best(tidedesk_codec::Settings {
        fps: status.fps,
        bitrate_bps: status.bitrate_bps,
        motion: status.game_boost,
    })
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
    capturer: Box<dyn capture::Capturer>,
    settings: VideoSettings,
    frames: tokio::sync::mpsc::Sender<EncodedFrame>,
    control: &VideoControl,
    ready: std::sync::mpsc::SyncSender<Result<StreamInfo>>,
) -> Result<()> {
    run_with(encoder_for, capturer, settings, frames, control, ready)
}

/// [`run`] with encoders made by `encoder_for`.
fn run_with<E: Encode>(
    encoder_for: impl Fn(StreamingStatus) -> Result<E>,
    mut capturer: Box<dyn capture::Capturer>,
    settings: VideoSettings,
    frames: tokio::sync::mpsc::Sender<EncodedFrame>,
    control: &VideoControl,
    ready: std::sync::mpsc::SyncSender<Result<StreamInfo>>,
) -> Result<()> {
    let rect = capturer.rect();
    let screen = ((rect.width as usize) & !1, (rect.height as usize) & !1);
    let _ = ready.send(Ok(StreamInfo {
        width: screen.0 as u32,
        height: screen.1 as u32,
        rect,
    }));

    let bitrate = |size| {
        settings
            .bitrate_bps
            .unwrap_or_else(|| automatic_bitrate(size))
    };
    let mut status = StreamingStatus::requested(0, false, settings.fps, bitrate(screen));
    let mut encoder = encoder_for(status)?;
    tracing::info!("video encoder: {}", encoder.implementation());
    tracing::info!(
        "video bitrate: {:.1} Mbit/s, {}",
        f64::from(status.bitrate_bps) / 1e6,
        if settings.bitrate_bps.is_some() {
            "as chosen"
        } else {
            "set by the screen's size"
        }
    );
    let mut reported_request = 0;
    let mut last_reconfigure: Option<Instant> = None;
    let mut captured = false;
    let mut bitstream = Vec::new();

    let mut interval = Duration::from_secs_f64(1.0 / status.fps as f64);
    let epoch = Instant::now();
    let mut next_due = Instant::now();
    let mut pending = false; // captured but not yet encoded
    let mut flying = VecDeque::new();
    // Encoded while the network was busy with the frame before, and the
    // preset it was made with.
    let mut held: Option<(EncodedFrame, StreamingStatus)> = None;
    // Times in a row that the network was found busy.
    let mut busy = 0;
    // A preset is acknowledged once a frame made with it has left.
    let mut acknowledge = |status: StreamingStatus| {
        if reported_request != status.request {
            *control.streaming_status.lock().unwrap() = Some(status);
            reported_request = status.request;
        }
    };
    let mut meter = tidedesk_core::stats::Meter::new("host video (capture+encode)");

    while !control.stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        let (request, enabled) = *control.boost_request.lock().unwrap();
        if request != status.request
            && last_reconfigure.is_none_or(|t| t.elapsed() >= Duration::from_millis(250))
        {
            let next =
                StreamingStatus::requested(request, enabled, settings.fps, status.bitrate_bps);
            if next.game_boost != status.game_boost {
                // A fresh encoder starts with SPS/PPS + IDR; never splice new
                // prediction state onto the old stream's dependent frames.
                // The pictures the old one held go with it.
                encoder = encoder_for(next)?;
                flying.clear();
                interval = Duration::from_secs_f64(1.0 / next.fps as f64);
                next_due = now;
            } else if captured {
                // A repeated request must also complete on a static screen,
                // even if rate control would otherwise skip the refresh.
                encoder.force_keyframe();
            }
            last_reconfigure = Some(now);
            status = next;
            // Re-encode the current picture even on an idle desktop so the
            // viewer gets a real frame and acknowledgement of this preset.
            pending |= captured;
        }
        if let Some((frame, made_with)) = held.take() {
            match frames.try_send(frame) {
                Ok(()) => acknowledge(made_with),
                Err(TrySendError::Full(frame)) => {
                    // Network still busy with the previous frame.
                    held = Some((frame, made_with));
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(TrySendError::Closed(_)) => break,
            }
        }
        if frames.is_closed() {
            break;
        }

        // A picture is handed over when the network waits for a frame and
        // the encoder has room, which the graphics card's has with a picture
        // in it.
        let room = |encoder: &E| {
            encoder.in_flight() < encoder.depth() && frames.capacity() == frames.max_capacity()
        };
        if now >= next_due && room(&encoder) {
            // When a frame is already pending or being encoded we only drain
            // newer ones; otherwise block until the desktop changes (this is
            // where an idle host sleeps).
            let wait = if pending || encoder.in_flight() > 0 {
                Duration::ZERO
            } else {
                Duration::from_millis(100)
            };
            if capturer.next_frame(wait)? {
                pending = true;
            }
            if pending {
                let began = Instant::now();
                let (w, h) = capturer.size();
                captured = true;
                if bitrate((w, h)) != status.bitrate_bps {
                    // An automatic bitrate follows the screen's size. A fresh
                    // encoder starts with a keyframe, as the new size needs
                    // anyway.
                    status.bitrate_bps = bitrate((w, h));
                    encoder = encoder_for(status)?;
                    flying.clear();
                    tracing::info!(
                        "video bitrate: {:.1} Mbit/s, set by the screen's size ({w}x{h})",
                        f64::from(status.bitrate_bps) / 1e6
                    );
                }

                if control.keyframe.swap(false, Ordering::Relaxed) {
                    encoder.force_keyframe();
                }
                let capture_us = epoch.elapsed().as_micros() as u64;
                encoder.send(capturer.image(), (w, h), capture_us / 1000)?;
                flying.push_back(Flying {
                    capture_us,
                    began,
                    status,
                });
                pending = false;
                busy = 0;
                // From when it was due, so that a picture that went in a
                // little late does not make the following ones late too.
                // After a screen that stayed the same, or with an encoder or
                // network that cannot keep up, the pace starts anew.
                next_due += interval;
                if next_due <= began {
                    next_due = began + interval;
                }
            }
        }

        let until_due = next_due.saturating_duration_since(Instant::now());
        if encoder.in_flight() == 0 {
            if frames.capacity() < frames.max_capacity() && until_due.is_zero() {
                // Network still busy with the previous frame, which it takes
                // within moments unless it has to wait itself.
                busy += 1;
                std::thread::sleep(Duration::from_micros(if busy > 4 { 2000 } else { 500 }));
            } else {
                std::thread::sleep(until_due);
            }
            continue;
        }
        // The encoder's picture is waited for until the next one can go in:
        // once it is due, which is looked at again shortly while the screen
        // stays the same or the network is busy, or once the encoder has room.
        let wait = if encoder.in_flight() < encoder.depth() {
            until_due.max(Duration::from_millis(2))
        } else {
            Duration::from_millis(50)
        };
        let encoded = match encoder.receive(wait, &mut bitstream)? {
            Received::Picture(encoded) => encoded,
            Received::Waiting => continue,
            Received::Lost => {
                // The encoder that took over starts with the picture on the
                // screen now.
                flying.clear();
                pending |= captured;
                continue;
            }
        };
        let picture = landed(&mut flying, encoded.timestamp_ms)
            .context("the encoder gave back a picture it did not get")?;
        if settings.stats
            && let Some(line) = meter.record(bitstream.len(), picture.began.elapsed())
        {
            tracing::info!("{line}");
        }
        if bitstream.is_empty() {
            continue;
        }
        let frame = EncodedFrame {
            header: VideoFrameHeader {
                len: bitstream.len() as u32,
                width: encoded.size.0 as u16,
                height: encoded.size.1 as u16,
                keyframe: encoded.keyframe,
                capture_us: picture.capture_us,
            },
            data: std::mem::take(&mut bitstream),
        };
        match frames.try_send(frame) {
            Ok(()) => acknowledge(picture.status),
            Err(TrySendError::Full(frame)) => held = Some((frame, picture.status)),
            Err(TrySendError::Closed(_)) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidedesk_codec::Decoder;

    /// Exercises the production capture/encode loop without capturing the user's
    /// desktop. Switching must also work when the captured desktop is static.
    #[tokio::test]
    async fn idle_session_applies_boost_and_restores_desktop_without_reconnecting() {
        /// A still desktop in BGRA pixels.
        struct StaticCapture {
            bgra: Vec<u8>,
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
            fn image(&self) -> tidedesk_codec::Image<'_> {
                (&self.bgra).into()
            }
            fn size(&self) -> (usize, usize) {
                (256, 144)
            }
            fn rect(&self) -> DisplayRect {
                DisplayRect {
                    left: -128,
                    top: 0,
                    width: 256,
                    height: 144,
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
                    bgra: vec![90; 256 * 144 * 4],
                    first: true,
                }),
                VideoSettings {
                    display: 0,
                    fps: 24,
                    bitrate_bps: Some(4_000_000),
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
        assert_eq!((info.width, info.height, info.rect.left), (256, 144, -128));
        let mut decoder = Decoder::openh264().unwrap();
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
            assert_eq!((frame.header.width, frame.header.height), (256, 144));
            assert!(frame.header.keyframe);
            assert_eq!(
                decoder.decode(&frame.data).unwrap().unwrap().dimensions(),
                (256, 144)
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
    fn the_automatic_bitrate_follows_the_screen_size() {
        assert_eq!(automatic_bitrate((1920, 1080)), 6_220_800);
        assert_eq!(automatic_bitrate((2560, 1600)), 12_288_000);
        // Never below the earlier fixed default, nor above the settings' most.
        assert_eq!(automatic_bitrate((1280, 720)), 4_000_000);
        assert_eq!(automatic_bitrate((3840, 2160)), 20_000_000);
    }

    /// With no bitrate chosen, the encoder's budget is set by the screen's
    /// size, and follows it when the size changes during a session.
    #[tokio::test]
    async fn an_automatic_bitrate_follows_a_changing_screen() {
        /// A still desktop whose size the test changes.
        struct Resizable {
            size: Arc<Mutex<(usize, usize)>>,
            shown: (usize, usize),
            bgra: Vec<u8>,
        }
        impl capture::Capturer for Resizable {
            fn next_frame(&mut self, timeout: Duration) -> Result<bool> {
                let size = *self.size.lock().unwrap();
                if size != self.shown {
                    self.shown = size;
                    return Ok(true);
                }
                std::thread::sleep(timeout);
                Ok(false)
            }
            fn image(&self) -> tidedesk_codec::Image<'_> {
                self.bgra[..self.shown.0 * self.shown.1 * 4].into()
            }
            fn size(&self) -> (usize, usize) {
                self.shown
            }
            fn rect(&self) -> DisplayRect {
                let (width, height) = *self.size.lock().unwrap();
                DisplayRect {
                    left: 0,
                    top: 0,
                    width: width as i32,
                    height: height as i32,
                }
            }
        }
        let size = Arc::new(Mutex::new((1920, 1080)));
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
        let capture = Resizable {
            size: size.clone(),
            shown: (0, 0),
            bgra: vec![90; 2560 * 1440 * 4],
        };
        let worker = std::thread::spawn(move || {
            run(
                Box::new(capture),
                VideoSettings {
                    display: 0,
                    fps: 30,
                    bitrate_bps: None,
                    stats: false,
                },
                tx,
                &worker_control,
                ready_tx,
            )
        });
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        for (request, (width, height)) in [(1, (1920, 1080)), (2, (2560, 1440))] {
            *size.lock().unwrap() = (width, height);
            let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                (frame.header.width, frame.header.height),
                (width as u16, height as u16)
            );
            // The budget in use is reported with the next preset request.
            *control.boost_request.lock().unwrap() = (request, false);
            let status = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(status) = control.streaming_status.lock().unwrap().take() {
                        return status;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(status.bitrate_bps, automatic_bitrate((width, height)));
            // The picture the request refreshed.
            tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
        }
        drop(stop);
        worker.join().unwrap().unwrap();
    }

    /// A screen with a window that moves with every picture, or one that
    /// stays as it is after the first.
    struct Screen {
        bgra: Vec<u8>,
        frame: usize,
        busy: bool,
        /// Stays as it is for 60 ms before every eighth picture, until then.
        pauses: Option<Instant>,
    }

    impl Screen {
        fn new(busy: bool) -> Box<Self> {
            Box::new(Self {
                bgra: vec![0; 256 * 144 * 4],
                frame: 0,
                busy,
                pauses: None,
            })
        }

        fn with_pauses() -> Box<Self> {
            let mut screen = Self::new(true);
            screen.pauses = Some(Instant::now());
            screen
        }
    }

    impl capture::Capturer for Screen {
        fn next_frame(&mut self, timeout: Duration) -> Result<bool> {
            if !self.busy && self.frame > 0 {
                std::thread::sleep(timeout);
                return Ok(false);
            }
            if let Some(until) = self.pauses {
                let left = until.saturating_duration_since(Instant::now());
                std::thread::sleep(left.min(timeout));
                if left > timeout {
                    return Ok(false);
                }
                if self.frame % 8 == 6 {
                    self.pauses = Some(Instant::now() + Duration::from_millis(60));
                }
            }
            self.frame += 1;
            let left = self.frame * 7 % 200;
            for (i, pixel) in self.bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let (x, y) = (i % 256, i / 256);
                let window = (40..80).contains(&y) && (left..left + 48).contains(&x);
                *pixel = if window {
                    [200, 90, 40, 255]
                } else {
                    [90, 90, 90, 255]
                };
            }
            Ok(true)
        }
        fn image(&self) -> Image<'_> {
            (&self.bgra).into()
        }
        fn size(&self) -> (usize, usize) {
            (256, 144)
        }
        fn rect(&self) -> DisplayRect {
            DisplayRect {
                left: 0,
                top: 0,
                width: 256,
                height: 144,
            }
        }
    }

    /// The pipeline on `screen` at Game Boost's rate, until dropped.
    struct Pipeline {
        control: Arc<VideoControl>,
        frames: tokio::sync::mpsc::Receiver<EncodedFrame>,
        worker: Option<std::thread::JoinHandle<Result<()>>>,
    }

    impl Pipeline {
        fn start<E: Encode>(
            encoder_for: impl Fn(StreamingStatus) -> Result<E> + Send + 'static,
            screen: Box<Screen>,
        ) -> Self {
            let control = Arc::new(VideoControl::default());
            let (tx, frames) = tokio::sync::mpsc::channel(1);
            let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
            let worker_control = control.clone();
            let settings = VideoSettings {
                display: 0,
                fps: 60,
                bitrate_bps: Some(4_000_000),
                stats: false,
            };
            let worker = std::thread::spawn(move || {
                run_with(encoder_for, screen, settings, tx, &worker_control, ready_tx)
            });
            ready_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            Self {
                control,
                frames,
                worker: Some(worker),
            }
        }

        async fn frame(&mut self) -> EncodedFrame {
            tokio::time::timeout(Duration::from_secs(5), self.frames.recv())
                .await
                .expect("a frame in time")
                .expect("the pipeline runs")
        }
    }

    impl Drop for Pipeline {
        fn drop(&mut self) {
            self.control.stop.store(true, Ordering::Relaxed);
            let ended = self.worker.take().unwrap().join();
            if !std::thread::panicking() {
                ended.unwrap().unwrap();
            }
        }
    }

    /// A busy screen at Game Boost's rate, with a network that takes its
    /// time: the frames reach the viewer in the order their pictures were
    /// taken, and each decodes after the one before.
    #[tokio::test]
    async fn frames_of_a_busy_screen_arrive_in_order() {
        let mut pipeline = Pipeline::start(encoder_for, Screen::new(true));
        let mut decoder = Decoder::openh264().unwrap();
        let mut last = None;
        for i in 0..40 {
            let frame = pipeline.frame().await;
            assert_eq!(frame.header.keyframe, i == 0, "frame {i}");
            assert_eq!(frame.header.len as usize, frame.data.len(), "frame {i}");
            assert!(Some(frame.header.capture_us) > last, "frame {i}");
            last = Some(frame.header.capture_us);
            let decoded = decoder.decode(&frame.data).unwrap();
            assert_eq!(decoded.unwrap().dimensions(), (256, 144), "frame {i}");
            if i % 4 == 3 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        }
    }

    /// What the [`Slow`] encoders of a test did.
    #[derive(Default)]
    struct Log {
        given_back: Vec<u64>,
        most_at_once: usize,
    }

    /// An encoder that takes 25 ms over each picture and holds two at once,
    /// as a graphics card's does at desktop sizes, which no test could count
    /// on. A frame of its holds its picture's timestamp.
    struct Slow {
        pictures: VecDeque<(Instant, tidedesk_codec::Encoded)>,
        keyframe: bool,
        /// Fails with the first pictures it holds.
        fails: bool,
        log: Arc<Mutex<Log>>,
    }

    impl Slow {
        fn maker(fails: bool, log: Arc<Mutex<Log>>) -> impl Fn(StreamingStatus) -> Result<Self> {
            move |_| {
                Ok(Self {
                    pictures: VecDeque::new(),
                    keyframe: false,
                    fails,
                    log: log.clone(),
                })
            }
        }
    }

    impl Encode for Slow {
        fn implementation(&self) -> Implementation {
            Implementation::Hardware
        }
        fn force_keyframe(&mut self) {
            self.keyframe = true;
        }
        fn depth(&self) -> usize {
            2
        }
        fn in_flight(&self) -> usize {
            self.pictures.len()
        }
        fn send(&mut self, _: Image<'_>, size: (usize, usize), timestamp_ms: u64) -> Result<()> {
            assert!(self.pictures.len() < 2, "more than the encoder holds");
            let encoded = tidedesk_codec::Encoded {
                timestamp_ms,
                size,
                keyframe: std::mem::take(&mut self.keyframe),
            };
            let ready = Instant::now() + Duration::from_millis(25);
            self.pictures.push_back((ready, encoded));
            let mut log = self.log.lock().unwrap();
            log.most_at_once = log.most_at_once.max(self.pictures.len());
            Ok(())
        }
        fn receive(&mut self, wait: Duration, out: &mut Vec<u8>) -> Result<Received> {
            out.clear();
            let Some(&(ready, encoded)) = self.pictures.front() else {
                return Ok(Received::Waiting);
            };
            let left = ready.saturating_duration_since(Instant::now());
            std::thread::sleep(left.min(wait));
            if left > wait {
                return Ok(Received::Waiting);
            }
            if std::mem::take(&mut self.fails) {
                self.pictures.clear();
                return Ok(Received::Lost);
            }
            self.pictures.pop_front();
            out.extend_from_slice(&encoded.timestamp_ms.to_le_bytes());
            let mut log = self.log.lock().unwrap();
            log.given_back.push(encoded.timestamp_ms);
            Ok(Received::Picture(encoded))
        }
    }

    /// The encoder takes longer over a picture than the next is in coming:
    /// it gets the next meanwhile, and every frame it gives back reaches the
    /// viewer, in order, also those the network had no room for at first.
    #[tokio::test]
    async fn no_frame_is_left_out_while_the_network_is_busy() {
        let log = Arc::new(Mutex::new(Log::default()));
        let mut pipeline = Pipeline::start(Slow::maker(false, log.clone()), Screen::new(true));
        let mut received = Vec::new();
        for i in 0..40 {
            let frame = pipeline.frame().await;
            let timestamp_ms = frame.header.capture_us / 1000;
            assert_eq!(frame.data, timestamp_ms.to_le_bytes(), "frame {i}");
            received.push(timestamp_ms);
            if i % 4 == 3 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        }
        drop(pipeline);
        let log = log.lock().unwrap();
        assert_eq!(log.most_at_once, 2);
        assert_eq!(received, log.given_back[..received.len()]);
    }

    /// At 60 frames a second a picture is due every 16.7 ms. An encoder that
    /// takes 25 ms over each still gives 60 a second with the next picture in
    /// it meanwhile, where one at a time gives 40.
    #[tokio::test]
    async fn the_frame_rate_is_reached_though_pictures_take_longer() {
        let log = Arc::new(Mutex::new(Log::default()));
        let mut pipeline = Pipeline::start(Slow::maker(false, log), Screen::new(true));
        let mut taken = Vec::new();
        for _ in 0..61 {
            taken.push(pipeline.frame().await.header.capture_us);
        }
        let apart = (taken[60] - taken[0]) as f64 / 60.0 / 1000.0;
        // 25 ms with one at a time; the rest is for a computer that is busy.
        assert!(apart < 22.0, "pictures {apart:.1} ms apart");
        // Nor faster than asked for.
        assert!(apart > 16.0, "pictures {apart:.1} ms apart");
    }

    /// After a screen that stayed the same, the pace starts anew: the next
    /// picture is not taken at once for being late.
    #[tokio::test]
    async fn the_pace_starts_anew_after_a_pause() {
        let log = Arc::new(Mutex::new(Log::default()));
        let mut pipeline = Pipeline::start(Slow::maker(false, log), Screen::with_pauses());
        let mut taken = Vec::new();
        for _ in 0..40 {
            taken.push(pipeline.frame().await.header.capture_us);
        }
        let apart: Vec<u64> = taken.windows(2).map(|pair| pair[1] - pair[0]).collect();
        let mut pauses = 0;
        for (i, pair) in apart.windows(2).enumerate() {
            if pair[0] > 50_000 {
                pauses += 1;
                assert!(pair[1] > 15_000, "picture {}: {apart:?}", i + 2);
            }
        }
        assert!(pauses >= 3, "{apart:?}");
    }

    /// A preset is acknowledged once a frame made with it leaves, not while
    /// that frame waits for the network.
    #[tokio::test]
    async fn a_preset_is_acknowledged_when_its_frame_leaves() {
        let log = Arc::new(Mutex::new(Log::default()));
        let mut pipeline = Pipeline::start(Slow::maker(false, log), Screen::new(true));
        for _ in 0..6 {
            pipeline.frame().await;
        }
        // The network takes no frames for a while: one waits for it, the
        // next is kept. The request's picture goes in meanwhile.
        *pipeline.control.boost_request.lock().unwrap() = (1, false);
        tokio::time::sleep(Duration::from_millis(150)).await;
        let acknowledged = |pipeline: &Pipeline| {
            let status = pipeline.control.streaming_status.lock().unwrap();
            status.is_some_and(|status| status.request == 1)
        };
        let early = acknowledged(&pipeline);
        let mut frame = pipeline.frame().await;
        assert!(
            !early || frame.header.keyframe,
            "acknowledged before its frame"
        );
        while !frame.header.keyframe {
            frame = pipeline.frame().await;
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while !acknowledged(&pipeline) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("acknowledged with its frame");
    }

    /// An encoder that fails loses the pictures it holds. The one on the
    /// screen is encoded again, though the screen stays as it is.
    #[tokio::test]
    async fn a_picture_lost_in_the_encoder_is_encoded_again() {
        let log = Arc::new(Mutex::new(Log::default()));
        let mut pipeline = Pipeline::start(Slow::maker(true, log.clone()), Screen::new(false));
        let frame = pipeline.frame().await;
        assert_eq!(frame.data, (frame.header.capture_us / 1000).to_le_bytes());
        // Taken after the first, which was lost 25 ms after it went in.
        assert!(
            frame.header.capture_us >= 25_000,
            "{}",
            frame.header.capture_us
        );
    }

    #[test]
    fn live_preset_changes_produce_decodable_keyframes_without_resizing() {
        let mut decoder = Decoder::openh264().unwrap();
        let pixels = vec![90; 256 * 144 * 4];
        let mut bytes = Vec::new();
        for enabled in [false, true, false] {
            let mut encoder =
                encoder_for(StreamingStatus::requested(1, enabled, 30, 4_000_000)).unwrap();
            assert!(encoder.encode(&pixels, (256, 144), 0, &mut bytes).unwrap());
            let decoded = decoder.decode(&bytes).unwrap().unwrap();
            assert_eq!(decoded.dimensions(), (256, 144));
        }
    }
}
