//! Encoding time (colour conversion included, median of all but the first
//! picture, which also sets the encoder up), size and quality per frame on a
//! synthetic desktop, for each encoder:
//! `cargo run --release -p tidedesk-codec --example encode_bench -- 2560 1440`
//! With `texture` the pictures come as Direct3D 11 textures, as screen
//! capture hands them over (the upload is not timed). With `paced` they come
//! at the frame rate (`fps=60` to change it from 30), as a host sends them,
//! instead of back to back. With `flight` each is handed over as soon as the
//! encoder has room for it, as a host does, which the graphics card's has
//! while it encodes the last: the frame rate reached, and how long each
//! picture was in the encoder, which gets one at a time again if pictures
//! take longer with the next one in it. Textures are then made beforehand, a
//! few that take turns, as uploading one keeps the card from encoding.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tidedesk_codec::{Decoder, Encoder, Image, Received, Settings};

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

/// Pictures that take turns when they are made beforehand.
const TURNS: usize = 6;

/// Typing and scrolling: a band changes every frame.
fn change(bgra: &mut [u8], (width, height): (usize, usize), frame: usize) {
    let band = (frame * 37) % (height - 40);
    for y in band..band + 40 {
        for x in 100..width.min(900) {
            bgra[(y * width + x) * 4] ^= 0x55;
        }
    }
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
    let textures = std::env::args().any(|a| a == "texture");
    let paced = std::env::args().any(|a| a == "paced");
    let flight = std::env::args().any(|a| a == "flight");
    let fps = std::env::args()
        .find_map(|a| a.strip_prefix("fps=").map(|n| n.parse().expect("fps=N")))
        .unwrap_or(30);
    let args: Vec<usize> = std::env::args()
        .skip(1)
        .filter(|a| !["texture", "paced", "flight"].contains(&a.as_str()) && !a.starts_with("fps="))
        .map(|a| {
            a.parse()
                .expect("width height [frames] [texture] [paced] [flight] [fps=N]")
        })
        .collect();
    let (width, height) = (
        args.first().copied().unwrap_or(1920),
        args.get(1).copied().unwrap_or(1080),
    );
    // The first picture sets the encoder up; the times are of the rest.
    let frames = args.get(2).copied().unwrap_or(60).max(2);
    let settings = Settings {
        fps,
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
    #[cfg(windows)]
    let gpu = textures.then(|| on_the_card::Picture::new((width, height)));
    #[cfg(not(windows))]
    assert!(!textures, "textures are Direct3D 11 textures, on Windows");
    println!(
        "{width}x{height}, {frames} frames at {} Mbit/s{}{}{}",
        settings.bitrate_bps / 1_000_000,
        if textures { ", as textures" } else { "" },
        if paced {
            format!(", {fps} a second")
        } else {
            String::new()
        },
        if flight { ", in flight" } else { "" }
    );
    'encoders: for mut encoder in encoders {
        let name = encoder.implementation().to_string();
        let mut bgra = desktop(width, height);
        let mut decoder = Decoder::openh264().unwrap();
        let (mut times, mut bytes, mut keyframes) = (Vec::new(), 0, 0);
        let (mut quality, mut decoded) = (0.0, 0);
        let (mut out, mut shown) = (Vec::new(), Vec::new());
        let began = Instant::now();
        let ms = |time: Duration| time.as_secs_f64() * 1000.0;
        if flight {
            // Made beforehand and decoded afterwards, so that only the
            // encoder is timed.
            let pictures: Vec<Vec<u8>> = (0..TURNS)
                .map(|turn| {
                    change(&mut bgra, (width, height), turn);
                    bgra.clone()
                })
                .collect();
            #[cfg(windows)]
            let uploaded: Vec<_> = pictures
                .iter()
                .filter_map(|picture| {
                    // On one device, as capture's pictures are.
                    let uploaded = gpu.as_ref()?.another();
                    uploaded.update(picture);
                    Some(uploaded)
                })
                .collect();
            let (mut sent, mut units) = (VecDeque::new(), Vec::new());
            let (mut frame, mut received) = (0, Vec::new());
            let began = Instant::now();
            while units.len() < frames {
                let due = match paced {
                    true => began + Duration::from_secs_f64(frame as f64 / fps as f64),
                    false => began,
                };
                let room = frame < frames && encoder.in_flight() < encoder.depth();
                if room && due <= Instant::now() {
                    let image = match () {
                        #[cfg(windows)]
                        () if textures => Image::Texture(&uploaded[frame % TURNS].texture),
                        () => Image::Bgra(&pictures[frame % TURNS]),
                    };
                    sent.push_back(Instant::now());
                    if let Err(e) = encoder.send(image, (width, height), frame as u64 * 33) {
                        println!("{name:>36}: {e:#}");
                        continue 'encoders;
                    }
                    frame += 1;
                    continue;
                }
                let wait = match room {
                    true => due.saturating_duration_since(Instant::now()),
                    false => Duration::from_millis(100),
                };
                match encoder.receive(wait, &mut out) {
                    Ok(Received::Picture(encoded)) => {
                        times.push(sent.pop_front().expect("sent before").elapsed());
                        received.push(Instant::now());
                        keyframes += usize::from(encoded.keyframe);
                        bytes += out.len();
                        units.push(out.clone());
                    }
                    Ok(Received::Waiting) => {}
                    Ok(Received::Lost) => {
                        let now = encoder.implementation();
                        println!("{name:>36}: failed during the run and handed over to {now}");
                        continue 'encoders;
                    }
                    Err(e) => {
                        println!("{name:>36}: {e:#}");
                        continue 'encoders;
                    }
                }
            }
            for (frame, unit) in units.iter().enumerate() {
                let source = &pictures[frame % TURNS];
                if let Some(db) = psnr(&mut decoder, unit, source, &mut shown) {
                    quality += db;
                    decoded += 1;
                }
            }
            // The first picture also sets the encoder up; the rest are steady.
            let steadily = received[frames - 1] - received[0];
            let mut steady: Vec<f64> = times[1..].iter().map(|&t| ms(t)).collect();
            steady.sort_by(f64::total_cmp);
            println!(
                "{name:>36}: {:5.1} frames a second, {} at once in the end, each {:5.1} ms in the encoder median, {:5.1} worst, {:5.1} first; {:5.1} kB/frame, {keyframes} keyframe(s), {:4.1} dB over {decoded} frames",
                (frames - 1) as f64 / steadily.as_secs_f64(),
                encoder.depth(),
                steady[steady.len() / 2],
                steady.last().copied().unwrap_or_default(),
                ms(times[0]),
                bytes as f64 / frames as f64 / 1000.0,
                quality / decoded.max(1) as f64,
            );
            continue;
        }
        for frame in 0..frames {
            if paced {
                let due = began + Duration::from_secs_f64(frame as f64 / fps as f64);
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
            }
            change(&mut bgra, (width, height), frame);
            #[cfg(windows)]
            if let Some(gpu) = &gpu {
                gpu.update(&bgra);
            }
            let image = match () {
                #[cfg(windows)]
                () if gpu.is_some() => Image::Texture(&gpu.as_ref().unwrap().texture),
                () => Image::Bgra(&bgra),
            };
            let start = Instant::now();
            let keyframe = match encoder.encode(image, (width, height), frame as u64 * 33, &mut out)
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

/// The picture on the default graphics card, as screen capture keeps it.
#[cfg(windows)]
mod on_the_card {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

    pub struct Picture {
        pub texture: ID3D11Texture2D,
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        size: (usize, usize),
    }

    impl Picture {
        pub fn new((width, height): (usize, usize)) -> Self {
            let (mut device, mut context) = (None, None);
            unsafe {
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_HARDWARE,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            }
            .expect("a Direct3D 11 device");
            Self::on(device.unwrap(), context.unwrap(), (width, height))
        }

        /// Another picture of this size on the same device.
        pub fn another(&self) -> Self {
            Self::on(self.device.clone(), self.context.clone(), self.size)
        }

        fn on(
            device: ID3D11Device,
            context: ID3D11DeviceContext,
            (width, height): (usize, usize),
        ) -> Self {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width as u32,
                Height: height as u32,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut texture = None;
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }.expect("a texture");
            Self {
                texture: texture.unwrap(),
                device,
                context,
                size: (width, height),
            }
        }

        pub fn update(&self, bgra: &[u8]) {
            let pitch = (self.size.0 * 4) as u32;
            unsafe {
                self.context.UpdateSubresource(
                    &self.texture,
                    0,
                    None,
                    bgra.as_ptr().cast(),
                    pitch,
                    0,
                )
            };
        }
    }
}
