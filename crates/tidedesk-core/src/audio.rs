//! Audio format shared by host and viewer, plus small DSP helpers.
//!
//! On the wire audio is always 48 kHz stereo Opus in 10 ms frames: small
//! enough for low latency, large enough that packet overhead stays negligible.

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
pub const FRAME_SAMPLES: usize = 480; // per channel, 10 ms
pub const DEFAULT_BITRATE: i32 = 96_000;

/// Converts interleaved audio with `from` channels to stereo.
pub fn to_stereo(input: &[f32], from: usize, out: &mut Vec<f32>) {
    match from {
        0 => {}
        1 => input.iter().for_each(|&s| out.extend_from_slice(&[s, s])),
        2 => out.extend_from_slice(input),
        // Surround: keep front left/right, which carry most desktop audio.
        n => input
            .chunks_exact(n)
            .for_each(|f| out.extend_from_slice(&[f[0], f[1]])),
    }
}

/// Converts interleaved stereo to `to` channels.
pub fn from_stereo(input: &[f32], to: usize, out: &mut Vec<f32>) {
    for f in input.as_chunks::<2>().0 {
        match to {
            1 => out.push((f[0] + f[1]) * 0.5),
            n => {
                out.push(f[0]);
                out.push(f[1]);
                out.extend(std::iter::repeat_n(0.0, n.saturating_sub(2)));
            }
        }
    }
}

/// Streaming linear resampler for interleaved stereo. Linear interpolation is
/// audibly transparent enough for desktop audio between the common 44.1/48 kHz
/// rates and costs almost nothing, which matters more here than studio quality.
#[derive(Debug, Clone)]
pub struct StereoResampler {
    step: f64,
    pos: f64,
    prev: [f32; 2],
}

impl StereoResampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        Self {
            step: from_rate as f64 / to_rate as f64,
            pos: 0.0,
            prev: [0.0; 2],
        }
    }

    pub fn is_passthrough(&self) -> bool {
        self.step == 1.0
    }

    /// Appends the resampled form of `input` to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.is_passthrough() {
            out.extend_from_slice(input);
            return;
        }
        let frames = input.len() / 2;
        // `pos` is measured in input frames, where index -1 is `prev`.
        while self.pos < frames as f64 {
            let i = self.pos.floor() as isize;
            let t = (self.pos - i as f64) as f32;
            for c in 0..2 {
                let a = if i == 0 {
                    self.prev[c]
                } else {
                    input[(i as usize - 1) * 2 + c]
                };
                let b = input[i as usize * 2 + c];
                out.push(a + (b - a) * t);
            }
            self.pos += self.step;
        }
        self.pos -= frames as f64;
        if frames > 0 {
            self.prev = [input[(frames - 1) * 2], input[(frames - 1) * 2 + 1]];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_conversion() {
        let mut out = Vec::new();
        to_stereo(&[0.5, 0.25], 1, &mut out);
        assert_eq!(out, [0.5, 0.5, 0.25, 0.25]);
        out.clear();
        to_stereo(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 6, &mut out);
        assert_eq!(out, [1.0, 2.0]);
        out.clear();
        from_stereo(&[1.0, 0.0], 1, &mut out);
        assert_eq!(out, [0.5]);
    }

    #[test]
    fn resampler_preserves_duration() {
        let mut r = StereoResampler::new(44_100, 48_000);
        let mut out = Vec::new();
        let chunk = vec![0.1f32; 441 * 2];
        for _ in 0..100 {
            r.process(&chunk, &mut out);
        }
        // One second in → one second out, within a frame.
        assert!(
            (out.len() as i64 / 2 - 48_000).abs() <= 1,
            "{}",
            out.len() / 2
        );
        // Past the one-frame fade-in from the initial silent state, a constant
        // input stays constant.
        assert!(out[4..].iter().all(|s| (s - 0.1).abs() < 1e-6));
    }

    #[test]
    fn resampler_passthrough_is_identity() {
        let mut r = StereoResampler::new(48_000, 48_000);
        let mut out = Vec::new();
        r.process(&[1.0, 2.0, 3.0, 4.0], &mut out);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
    }
}
