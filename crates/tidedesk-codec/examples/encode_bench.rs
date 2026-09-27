//! Encoding time (colour conversion included, median of all but the first
//! picture, which also sets the encoder up), size and quality per frame on a
//! synthetic desktop, for each encoder:
//! `cargo run --release -p tidedesk-codec --example encode_bench -- 2560 1440`

use std::time::{Duration, Instant};

use tidedesk_codec::{Decoder, Encoder, Settings};

/// Window-like blocks with "text" stripes.
fn desktop(width: usize, height: usize) -> Vec<u8> {
    let mut bgra = vec![0u8; width * height * 4];
    for (i, pixel) in bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (i % width, i / width);
        let text = (y % 18 < 12) && ((x / 7 + y / 18) % 5 != 0) && (x % 7 < 5);
        let v = if text {
            30
        } else {
            235 - ((x / 400 + y / 300) % 3) as u8 * 20
        };
        *pixel = [v, v, v.saturating_sub(10), 255];
    }
    bgra
}

/// PSNR of what a viewer shows against what was captured.
fn psnr(decoder: &mut Decoder, unit: &[u8], bgra: &[u8], shown: &mut Vec<u32>) -> Option<f64> {
    decoder.decode(unit).ok()??.write_xrgb(shown);
    let error: f64 = shown
        .iter()
        .zip(bgra.as_chunks::<4>().0)
        .map(|(xrgb, source)| {
            let [b, g, r, _] = xrgb.to_le_bytes();
            [(b, source[0]), (g, source[1]), (r, source[2])]
                .iter()
                .map(|(a, b)| (f64::from(*a) - f64::from(*b)).powi(2))
                .sum::<f64>()
        })
        .sum();
    let mean = error / (shown.len() * 3) as f64;
    Some(10.0 * (255.0 * 255.0 / mean.max(1e-9)).log10())
}

fn main() {
    let args: Vec<usize> = std::env::args()
        .skip(1)
        .map(|a| a.parse().expect("width height [frames]"))
        .collect();
    let (width, height) = (
        args.first().copied().unwrap_or(1920),
        args.get(1).copied().unwrap_or(1080),
    );
    let frames = args.get(2).copied().unwrap_or(60);
    let settings = Settings {
        fps: 30,
        bitrate_bps: 8_000_000,
        motion: false,
    };

    let mut encoders = vec![Encoder::openh264(settings).unwrap()];
    #[cfg(windows)]
    for encoder in [
        Encoder::media_foundation(settings),
        Encoder::hardware(settings),
    ] {
        match encoder {
            Ok(encoder) => encoders.push(encoder),
            Err(e) => println!("unavailable: {e:#}"),
        }
    }
    println!(
        "{width}x{height}, {frames} frames at {} Mbit/s",
        settings.bitrate_bps / 1_000_000
    );
    'encoders: for mut encoder in encoders {
        let name = encoder.implementation().to_string();
        let mut bgra = desktop(width, height);
        let mut decoder = Decoder::openh264().unwrap();
        let (mut times, mut bytes, mut keyframes) = (Vec::new(), 0, 0);
        let (mut quality, mut decoded) = (0.0, 0);
        let (mut out, mut shown) = (Vec::new(), Vec::new());
        for frame in 0..frames {
            // Typing and scrolling: a band changes every frame.
            let band = (frame * 37) % (height - 40);
            for y in band..band + 40 {
                for x in 100..width.min(900) {
                    bgra[(y * width + x) * 4] ^= 0x55;
                }
            }
            let start = Instant::now();
            let keyframe = match encoder.encode(&bgra, (width, height), frame as u64 * 33, &mut out)
            {
                Ok(keyframe) => keyframe,
                Err(e) => {
                    println!("{name:>36}: {e:#}");
                    continue 'encoders;
                }
            };
            times.push(start.elapsed());
            bytes += out.len();
            keyframes += usize::from(keyframe);
            if let Some(db) = psnr(&mut decoder, &out, &bgra, &mut shown) {
                quality += db;
                decoded += 1;
            }
        }
        let now = encoder.implementation();
        if now.to_string() != name {
            println!("{name:>36}: failed during the run and handed over to {now}");
            continue;
        }
        // The first picture also sets the encoder up; the rest are steady.
        let ms = |time: Duration| time.as_secs_f64() * 1000.0;
        let first = ms(times[0]);
        let mut steady: Vec<f64> = times[1..].iter().map(|&t| ms(t)).collect();
        steady.sort_by(f64::total_cmp);
        println!(
            "{name:>36}: {:5.1} ms/frame median, {:5.1} worst, {first:5.1} first; {:5.1} kB/frame, {keyframes} keyframe(s), {:4.1} dB over {decoded} frames",
            steady[steady.len() / 2],
            steady.last().copied().unwrap_or(first),
            bytes as f64 / frames as f64 / 1000.0,
            quality / decoded.max(1) as f64,
        );
    }
}
