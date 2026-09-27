//! Decoding and colour conversion time per frame, for each decoder:
//! `cargo run --release -p tidedesk-codec --example decode_bench -- 1920 1080`

use std::time::{Duration, Instant};

use openh264::encoder::Encoder;
use openh264::formats::{BgraSliceU8, YUVBuffer};
use tidedesk_codec::Decoder;

/// A desktop-like picture with a moving window, so P-frames carry change.
fn scene(width: usize, height: usize, frame: usize) -> Vec<u8> {
    let mut bgra = vec![0u8; width * height * 4];
    for (i, pixel) in bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (i % width, i / width);
        pixel.copy_from_slice(&[(x / 8) as u8, (y / 8) as u8, 120, 255]);
    }
    let (side, speed) = (width / 4, 12);
    let (left, top) = ((frame * speed) % (width - side), height / 3);
    for y in top..top + side.min(height - top) {
        for x in left..left + side {
            bgra[(y * width + x) * 4..][..3].copy_from_slice(&[240, 240, 240]);
        }
    }
    bgra
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

    let mut encoder = Encoder::new().unwrap();
    let units: Vec<Vec<u8>> = (0..frames)
        .map(|frame| {
            let mut yuv = YUVBuffer::new(width, height);
            yuv.read_bgra8(BgraSliceU8::new(
                &scene(width, height, frame),
                (width, height),
            ));
            encoder.encode(&yuv).unwrap().to_vec()
        })
        // The encoder may skip a frame to keep its bitrate.
        .filter(|unit| !unit.is_empty())
        .collect();
    let frames = units.len();

    let mut decoders = vec![("OpenH264", Decoder::openh264().unwrap())];
    #[cfg(windows)]
    match Decoder::media_foundation() {
        Ok(decoder) => decoders.push(("Windows", decoder)),
        Err(e) => println!("Windows decoder unavailable: {e:#}"),
    }
    println!("{width}x{height}, {frames} frames");
    for (name, mut decoder) in decoders {
        let (mut decode, mut convert) = (Duration::ZERO, Duration::ZERO);
        let mut pixels = Vec::new();
        for unit in &units {
            let start = Instant::now();
            let picture = decoder.decode(unit).unwrap().expect("a picture per frame");
            decode += start.elapsed();
            let start = Instant::now();
            picture.write_xrgb(&mut pixels);
            convert += start.elapsed();
        }
        let per_frame = |total: Duration| total.as_secs_f64() * 1000.0 / frames as f64;
        println!(
            "  {name:>8}: decode {:.2} ms, convert {:.2} ms per frame",
            per_frame(decode),
            per_frame(convert)
        );
    }

    // What the viewer did before: OpenH264's RGB conversion, then packing.
    let mut decoder = openh264::decoder::Decoder::new().unwrap();
    let (mut rgb, mut pixels, mut total) = (Vec::new(), Vec::<u32>::new(), Duration::ZERO);
    for unit in &units {
        let picture = decoder.decode(unit).unwrap().unwrap();
        let start = Instant::now();
        rgb.resize(width * height * 3, 0);
        picture.write_rgb8(&mut rgb);
        pixels.clear();
        pixels.extend(
            rgb.as_chunks::<3>()
                .0
                .iter()
                .map(|p| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2])),
        );
        total += start.elapsed();
    }
    println!(
        "  previous two-pass conversion: {:.2} ms per frame",
        total.as_secs_f64() * 1000.0 / frames as f64
    );
}
