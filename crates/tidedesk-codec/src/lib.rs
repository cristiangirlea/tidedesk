//! H.264 video for TideDesk: Windows' own codecs (Media Foundation) where
//! they are present, OpenH264 otherwise.
//!
//! The stream is plain Annex B H.264 in the constrained baseline profile, so
//! every decoder here reads every encoder's output, including TideDesk
//! releases that only had OpenH264.

#[cfg(windows)]
mod mf;

use std::fmt;

use anyhow::Result;
use openh264::OpenH264API;
use openh264::decoder::{DecodedYUV, DecoderConfig};
use openh264::formats::{YUVSlices, YUVSource};

/// Set to `openh264` to use OpenH264 even where Windows has its own codecs.
pub const CHOICE_ENV: &str = "TIDEDESK_CODEC";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Implementation {
    /// Windows' own codec, through Media Foundation.
    MediaFoundation,
    OpenH264,
}

impl fmt::Display for Implementation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MediaFoundation => "Windows (Media Foundation)",
            Self::OpenH264 => "OpenH264",
        })
    }
}

/// Whether `choice` (the value of [`CHOICE_ENV`]) asks for OpenH264.
fn prefers_openh264(choice: Option<&str>) -> bool {
    choice.is_some_and(|c| c.trim().eq_ignore_ascii_case("openh264"))
}

pub struct Decoder {
    inner: Inner,
}

enum Inner {
    #[cfg(windows)]
    MediaFoundation(mf::Decoder),
    OpenH264(openh264::decoder::Decoder),
}

impl Decoder {
    /// Windows' own decoder where it is present and not turned off with
    /// [`CHOICE_ENV`], else OpenH264.
    pub fn best() -> Result<Self> {
        Self::best_for(std::env::var(CHOICE_ENV).ok().as_deref())
    }

    fn best_for(choice: Option<&str>) -> Result<Self> {
        if prefers_openh264(choice) {
            return Self::openh264();
        }
        #[cfg(windows)]
        match Self::media_foundation() {
            Ok(decoder) => return Ok(decoder),
            Err(e) => tracing::info!("using OpenH264: {e:#}"),
        }
        Self::openh264()
    }

    pub fn openh264() -> Result<Self> {
        let decoder = openh264::decoder::Decoder::with_api_config(
            OpenH264API::from_source(),
            DecoderConfig::new(),
        )?;
        Ok(Self {
            inner: Inner::OpenH264(decoder),
        })
    }

    /// Windows' own decoder; an error where Windows has none.
    #[cfg(windows)]
    pub fn media_foundation() -> Result<Self> {
        Ok(Self {
            inner: Inner::MediaFoundation(mf::Decoder::new()?),
        })
    }

    pub fn implementation(&self) -> Implementation {
        match self.inner {
            #[cfg(windows)]
            Inner::MediaFoundation(_) => Implementation::MediaFoundation,
            Inner::OpenH264(_) => Implementation::OpenH264,
        }
    }

    /// Decodes one access unit (Annex B); `Ok(None)` while no picture is ready.
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<Picture<'_>>> {
        Ok(match &mut self.inner {
            #[cfg(windows)]
            Inner::MediaFoundation(decoder) => decoder.decode(data)?.map(Frame::Planes),
            Inner::OpenH264(decoder) => decoder.decode(data)?.map(Frame::OpenH264),
        }
        .map(Picture))
    }
}

/// A decoded picture, valid until the next call to [`Decoder::decode`].
pub struct Picture<'a>(Frame<'a>);

enum Frame<'a> {
    OpenH264(DecodedYUV<'a>),
    Planes(YUVSlices<'a>),
}

impl Picture<'_> {
    fn source(&self) -> &dyn YUVSource {
        match &self.0 {
            Frame::OpenH264(picture) => picture,
            Frame::Planes(planes) => planes,
        }
    }

    /// Width and height in pixels.
    pub fn dimensions(&self) -> (usize, usize) {
        self.source().dimensions()
    }

    /// Converts the picture into `out` as `0x00RRGGBB` pixels, row by row.
    pub fn write_xrgb(&self, out: &mut Vec<u32>) {
        write_xrgb(self.source(), out);
    }
}

/// OpenH264's limited-range BT.601 factors, in 16.16 fixed point.
const fn fixed(factor: f32) -> i32 {
    let scaled = factor * 65536.0;
    if scaled < 0.0 {
        (scaled - 0.5) as i32
    } else {
        (scaled + 0.5) as i32
    }
}
const Y_MUL: i32 = fixed(255.0 / 219.0);
const RV_MUL: i32 = fixed(255.0 / 224.0 * 1.402);
const GV_MUL: i32 = fixed(-255.0 / 224.0 * 1.402 * 0.299 / 0.587);
const GU_MUL: i32 = fixed(-255.0 / 224.0 * 1.772 * 0.114 / 0.587);
const BU_MUL: i32 = fixed(255.0 / 224.0 * 1.772);

/// Limited-range BT.601 YUV 4:2:0 to `0x00RRGGBB`, the colours OpenH264 uses
/// in both directions, in one pass.
///
/// The colour part of each pixel is worked out once per chroma row, which
/// two picture rows share; what is left per pixel is plain arithmetic the
/// compiler vectorises.
fn write_xrgb(source: &dyn YUVSource, out: &mut Vec<u32>) {
    let (width, height) = source.dimensions();
    let (y_stride, u_stride, v_stride) = source.strides();
    let (y_plane, u_plane, v_plane) = (source.y(), source.u(), source.v());
    out.clear();
    out.resize(width * height, 0);
    let mut chroma = Chroma::default();
    for (row, target) in out.chunks_exact_mut(width.max(1)).enumerate() {
        if row % 2 == 0 {
            let us = &u_plane[(row / 2) * u_stride..][..width.div_ceil(2)];
            let vs = &v_plane[(row / 2) * v_stride..][..width.div_ceil(2)];
            chroma.fill(us, vs);
        }
        convert_row(&y_plane[row * y_stride..][..width], &chroma, target);
    }
}

/// The colour offsets of one chroma row, one entry per pair of pixels, with
/// rounding folded in.
#[derive(Default)]
struct Chroma {
    red: Vec<i32>,
    green: Vec<i32>,
    blue: Vec<i32>,
}

impl Chroma {
    fn fill(&mut self, us: &[u8], vs: &[u8]) {
        // Rounded, so limited-range white (235) is 255, not 254.
        const HALF: i32 = 1 << 15;
        let count = us.len().min(vs.len());
        self.red.resize(count, 0);
        self.green.resize(count, 0);
        self.blue.resize(count, 0);
        let offsets = self.red.iter_mut().zip(&mut self.green).zip(&mut self.blue);
        for (((red, green), blue), (&u, &v)) in offsets.zip(us.iter().zip(vs)) {
            let (u, v) = (i32::from(u) - 128, i32::from(v) - 128);
            *red = RV_MUL * v + HALF;
            *green = GU_MUL * u + GV_MUL * v + HALF;
            *blue = BU_MUL * u + HALF;
        }
    }
}

fn convert_row(ys: &[u8], chroma: &Chroma, target: &mut [u32]) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU has AVX2, checked just above.
        return unsafe { convert_row_avx2(ys, chroma, target) };
    }
    convert_row_portable(ys, chroma, target);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn convert_row_avx2(ys: &[u8], chroma: &Chroma, target: &mut [u32]) {
    convert_row_portable(ys, chroma, target);
}

#[inline(always)]
fn convert_row_portable(ys: &[u8], chroma: &Chroma, target: &mut [u32]) {
    let channel = |value: i32| (value >> 16).clamp(0, 255) as u32;
    let pixel = |y: u8, red: i32, green: i32, blue: i32| {
        let luma = (i32::from(y) - 16) * Y_MUL;
        channel(luma + red) << 16 | channel(luma + green) << 8 | channel(luma + blue)
    };
    let offsets = chroma.red.iter().zip(&chroma.green).zip(&chroma.blue);
    let (out_pairs, _) = target.as_chunks_mut::<2>();
    let (y_pairs, _) = ys.as_chunks::<2>();
    for ((out, y), ((&red, &green), &blue)) in out_pairs.iter_mut().zip(y_pairs).zip(offsets) {
        out[0] = pixel(y[0], red, green, blue);
        out[1] = pixel(y[1], red, green, blue);
    }
    // An odd width leaves one pixel with a chroma sample of its own.
    if ys.len() % 2 == 1 {
        let (last, chroma_last) = (ys.len() - 1, chroma.red.len() - 1);
        target[last] = pixel(
            ys[last],
            chroma.red[chroma_last],
            chroma.green[chroma_last],
            chroma.blue[chroma_last],
        );
    }
}

#[cfg(test)]
mod tests {
    use openh264::encoder::Encoder;
    use openh264::formats::{BgraSliceU8, YUVBuffer};

    use super::*;

    /// A picture that changes with `frame`, so P-frames carry motion.
    fn scene(width: usize, height: usize, frame: usize) -> Vec<u8> {
        let mut bgra = vec![0u8; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let pixel = &mut bgra[(y * width + x) * 4..][..4];
                pixel[0] = (x * 255 / width) as u8;
                pixel[1] = (y * 255 / height) as u8;
                pixel[2] = ((x + y + frame * 7) % 256) as u8;
                pixel[3] = 255;
            }
        }
        // A bright square that moves.
        let (sx, sy) = ((frame * 5) % (width - 16), (frame * 3) % (height - 16));
        for y in sy..sy + 16 {
            for x in sx..sx + 16 {
                bgra[(y * width + x) * 4..][..3].copy_from_slice(&[250, 250, 250]);
            }
        }
        bgra
    }

    /// Encodes the scenes with OpenH264, as every TideDesk host so far did.
    fn stream(sizes: &[(usize, usize, usize)]) -> Vec<Vec<u8>> {
        let mut encoder = Encoder::new().unwrap();
        let mut units = Vec::new();
        for &(width, height, frames) in sizes {
            for frame in 0..frames {
                let pixels = scene(width, height, frame);
                let mut yuv = YUVBuffer::new(width, height);
                yuv.read_bgra8(BgraSliceU8::new(&pixels, (width, height)));
                units.push(encoder.encode(&yuv).unwrap().to_vec());
            }
        }
        units
    }

    /// The visible planes of a picture, without row padding.
    fn planes(picture: &Picture<'_>) -> ((usize, usize), Vec<u8>, Vec<u8>, Vec<u8>) {
        let source = picture.source();
        let (width, height) = source.dimensions();
        let (sy, su, sv) = source.strides();
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let rows = |plane: &[u8], stride: usize, w: usize, h: usize| {
            (0..h)
                .flat_map(|r| plane[r * stride..][..w].to_vec())
                .collect::<Vec<u8>>()
        };
        (
            (width, height),
            rows(source.y(), sy, width, height),
            rows(source.u(), su, cw, ch),
            rows(source.v(), sv, cw, ch),
        )
    }

    /// Windows' decoder, or `None` (with a note) where this Windows has none.
    /// `TIDEDESK_REQUIRE_MF=1` turns its absence into a failure.
    #[cfg(windows)]
    fn media_foundation_or_skip() -> Option<Decoder> {
        match Decoder::media_foundation() {
            Ok(decoder) => Some(decoder),
            Err(e) if std::env::var_os("TIDEDESK_REQUIRE_MF").is_none() => {
                eprintln!("skipping: no Windows H.264 decoder here ({e:#})");
                None
            }
            Err(e) => panic!("TIDEDESK_REQUIRE_MF is set, but {e:#}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_decodes_openh264_streams_bit_exactly_and_at_once() {
        let Some(mut windows) = media_foundation_or_skip() else {
            return;
        };
        assert_eq!(windows.implementation(), Implementation::MediaFoundation);
        let mut reference = Decoder::openh264().unwrap();
        // Size changes, a height and then a width that are not multiples of
        // 16 (cropped, as on a 1366x768 laptop screen).
        let units = stream(&[(128, 96, 6), (176, 120, 4), (180, 120, 4)]);
        for (i, unit) in units.iter().enumerate() {
            let expected = reference.decode(unit).unwrap().map(|p| planes(&p));
            let actual = windows.decode(unit).unwrap().map(|p| planes(&p));
            assert!(expected.is_some(), "OpenH264 gave no picture for unit {i}");
            // Every access unit yields its picture immediately: no added latency.
            assert_eq!(
                actual.as_ref().map(|p| p.0),
                expected.as_ref().map(|p| p.0),
                "unit {i}"
            );
            assert!(actual == expected, "unit {i}: planes differ");
        }
    }

    #[test]
    fn conversion_matches_openh264_colours() {
        let mut decoder = Decoder::openh264().unwrap();
        let unit = &stream(&[(128, 96, 1)])[0];
        let Some(Frame::OpenH264(picture)) = decoder.decode(unit).unwrap().map(|p| p.0) else {
            panic!("expected an OpenH264 picture");
        };
        let mut reference = vec![0u8; picture.rgb8_len()];
        picture.write_rgb8(&mut reference);
        let mut ours = Vec::new();
        write_xrgb(&picture, &mut ours);
        assert_eq!(ours.len(), 128 * 96);
        let mut worst = 0;
        for (pixel, rgb) in ours.iter().zip(reference.as_chunks::<3>().0) {
            assert_eq!(pixel >> 24, 0);
            let channels = [(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8];
            for (a, b) in channels.iter().zip(rgb) {
                worst = worst.max(a.abs_diff(*b));
            }
        }
        // OpenH264's own fast path is within one of its formula; so are we.
        assert!(worst <= 2, "largest channel difference {worst}");
    }

    #[test]
    fn conversion_maps_limited_range_black_and_white() {
        let y = [16u8, 235, 16, 235];
        let (u, v) = ([128u8], [128u8]);
        let planes = YUVSlices::new((&y, &u, &v), (2, 2), (2, 1, 1));
        let mut out = Vec::new();
        write_xrgb(&planes, &mut out);
        assert_eq!(out, [0x000000, 0xFFFFFF, 0x000000, 0xFFFFFF]);

        // An odd width: the last pixel of each row has a chroma sample alone.
        let y = [16u8, 16, 235, 16, 16, 235];
        let (u, v) = ([128u8, 128], [128u8, 255]);
        let planes = YUVSlices::new((&y, &u, &v), (3, 2), (3, 2, 2));
        write_xrgb(&planes, &mut out);
        assert_eq!(out.len(), 6);
        assert_eq!(out[0], 0x000000);
        // A strong red difference over white: red and blue stay full, green drops.
        assert_eq!(out[2] >> 16, 0xFF);
        assert_eq!(out[2] & 0xFF, 0xFF);
        assert!((out[2] >> 8) & 0xFF < 0xFF, "{:06X}", out[2]);
        assert_eq!(out[5], out[2]);
    }

    #[test]
    fn the_choice_can_force_openh264() {
        assert!(prefers_openh264(Some("openh264")));
        assert!(prefers_openh264(Some(" OpenH264 ")));
        assert!(!prefers_openh264(None));
        assert!(!prefers_openh264(Some("")));
        assert!(!prefers_openh264(Some("windows")));
        let forced = Decoder::best_for(Some("openh264")).unwrap();
        assert_eq!(forced.implementation(), Implementation::OpenH264);
        #[cfg(windows)]
        if media_foundation_or_skip().is_some() {
            let best = Decoder::best_for(None).unwrap();
            assert_eq!(best.implementation(), Implementation::MediaFoundation);
        }
    }
}
