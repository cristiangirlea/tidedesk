//! Measures colour conversion and H.264 encode cost on a synthetic desktop.
//! `cargo run --release -p tidedesk-host --example encode_bench -- 2560 1440`

use std::time::{Duration, Instant};

use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType,
};
use openh264::formats::{BgraSliceU8, YUVBuffer};

fn main() {
    let args: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let (w, h) = (
        *args.first().unwrap_or(&1920),
        *args.get(1).unwrap_or(&1080),
    );
    let threads = *args.get(2).unwrap_or(&2) as u16;
    let mut bgra = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            // Window-like blocks with "text" stripes.
            let text = (y % 18 < 12) && ((x / 7 + y / 18) % 5 != 0) && (x % 7 < 5);
            let v = if text {
                30
            } else {
                235 - ((x / 400 + y / 300) % 3) as u8 * 20
            };
            bgra[i..i + 4].copy_from_slice(&[v, v, v.saturating_sub(10), 255]);
        }
    }
    let config = EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .bitrate(BitRate::from_bps(4_000_000))
        .max_frame_rate(FrameRate::from_hz(30.0))
        .complexity(Complexity::Low)
        .skip_frames(false)
        .num_threads(threads);
    let mut enc = Encoder::with_api_config(OpenH264API::from_source(), config).unwrap();
    let mut yuv = YUVBuffer::new(w, h);
    let (mut conv, mut code) = (Duration::ZERO, Duration::ZERO);
    let n = 60;
    let mut bytes = 0;
    for f in 0..n {
        // Simulate typing/scrolling: change a band each frame.
        let band = (f * 37) % (h - 40);
        for y in band..band + 40 {
            for x in 100..w.min(900) {
                bgra[(y * w + x) * 4] ^= 0x55;
            }
        }
        let t = Instant::now();
        yuv.read_bgra8(BgraSliceU8::new(&bgra, (w, h)));
        conv += t.elapsed();
        let t = Instant::now();
        bytes += enc.encode(&yuv).unwrap().to_vec().len();
        code += t.elapsed();
    }
    println!(
        "{w}x{h} threads={threads}: convert {:.1} ms, encode {:.1} ms per frame, {:.0} kB/frame",
        conv.as_secs_f64() * 1000.0 / n as f64,
        code.as_secs_f64() * 1000.0 / n as f64,
        bytes as f64 / n as f64 / 1000.0
    );
}
