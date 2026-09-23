//! Lightweight throughput meter and network path lines for the `--stats`
//! output.

use std::net::SocketAddr;
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

/// One `--stats` line about a connection's network path.
pub fn path_line(label: &str, remote: SocketAddr, rtt: Duration, lost_packets: u64) -> String {
    let rtt_ms = rtt.as_secs_f64() * 1000.0;
    format!("{label}: {remote}, rtt {rtt_ms:.1} ms, {lost_packets} packets lost")
}

/// Logs [`path_line`] every two seconds until the connection closes.
pub async fn log_path(conn: quinn::Connection, label: &'static str) {
    let first = tokio::time::Instant::now() + Meter::WINDOW;
    let mut every = tokio::time::interval_at(first, Meter::WINDOW);
    loop {
        tokio::select! {
            _ = conn.closed() => return,
            _ = every.tick() => {
                let path = conn.stats().path;
                let line = path_line(label, conn.remote_address(), path.rtt, path.lost_packets);
                tracing::info!("{line}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_line_names_the_peer_rtt_and_losses() {
        let line = path_line(
            "viewer path (direct)",
            "203.0.113.5:40000".parse().unwrap(),
            Duration::from_micros(23_400),
            3,
        );
        assert_eq!(
            line,
            "viewer path (direct): 203.0.113.5:40000, rtt 23.4 ms, 3 packets lost"
        );
    }
}
