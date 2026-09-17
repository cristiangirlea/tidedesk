//! Lightweight throughput meter for the `--stats` output.

use std::time::{Duration, Instant};

pub struct Meter {
    label: &'static str,
    window_start: Instant,
    frames: u32,
    bytes: u64,
    work: Duration,
}

impl Meter {
    const WINDOW: Duration = Duration::from_secs(2);

    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            window_start: Instant::now(),
            frames: 0,
            bytes: 0,
            work: Duration::ZERO,
        }
    }

    /// Records one frame of `bytes` that took `work` to process, and returns a
    /// summary line whenever a reporting window has elapsed.
    pub fn record(&mut self, bytes: usize, work: Duration) -> Option<String> {
        self.frames += 1;
        self.bytes += bytes as u64;
        self.work += work;
        let elapsed = self.window_start.elapsed();
        if elapsed < Self::WINDOW {
            return None;
        }
        let secs = elapsed.as_secs_f64();
        let line = format!(
            "{}: {:.1} fps, {:.2} Mbit/s, {:.1} ms/frame",
            self.label,
            self.frames as f64 / secs,
            self.bytes as f64 * 8.0 / secs / 1e6,
            self.work.as_secs_f64() * 1000.0 / self.frames as f64,
        );
        *self = Self::new(self.label);
        Some(line)
    }
}
