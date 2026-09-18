//! Session streaming presets. These never change display geometry or input permissions.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamingStatus {
    pub request: u64,
    pub game_boost: bool,
    /// Encoder target, not measured throughput.
    pub fps: u32,
    pub bitrate_bps: u32,
}

impl StreamingStatus {
    pub fn requested(request: u64, game_boost: bool, desktop_fps: u32, bitrate_bps: u32) -> Self {
        Self {
            request,
            game_boost,
            fps: if game_boost { 60 } else { desktop_fps.max(1) },
            // The viewer cannot override the host's bandwidth budget.
            bitrate_bps,
        }
    }
}

/// Playback jitter targets in milliseconds, not end-to-end latency guarantees.
pub fn audio_buffer_ms(game_boost: bool) -> (usize, usize) {
    if game_boost { (20, 60) } else { (40, 150) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boost_restores_desktop_fps_and_preserves_bandwidth() {
        for fps in [5, 30, 60] {
            let desktop = StreamingStatus::requested(1, false, fps, 8_000_000);
            let boost = StreamingStatus::requested(2, true, fps, 8_000_000);
            let restored = StreamingStatus::requested(3, false, fps, 8_000_000);
            assert_eq!(boost.fps, 60);
            assert_eq!(restored.fps, desktop.fps);
            assert_eq!(boost.bitrate_bps, desktop.bitrate_bps);
            assert_eq!(restored.bitrate_bps, desktop.bitrate_bps);
        }
        assert_eq!(audio_buffer_ms(false), (40, 150));
        assert_eq!(audio_buffer_ms(true), (20, 60));
    }
}
