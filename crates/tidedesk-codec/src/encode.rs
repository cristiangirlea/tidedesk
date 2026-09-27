//! Encoding: the graphics card's H.264 encoder where there is one, then
//! Windows' own, then OpenH264. All write constrained-baseline Annex B H.264
//! with the parameter sets in front of every keyframe, which every TideDesk
//! viewer decodes.

use anyhow::{Result, bail};
use openh264::encoder::{
    BitRate, Complexity, EncoderConfig, FrameRate, FrameType, RateControlMode, UsageType,
};
use openh264::formats::{BgraSliceU8, YUVBuffer, YUVSource};
use openh264::{OpenH264API, Timestamp};

use crate::{CHOICE_ENV, Choice, Implementation, choice};

/// The most macroblocks in a picture that H.264 decoders must take (level
/// 5.2): 4096x2304 or 5120x1440, for example.
const MAX_MACROBLOCKS: usize = 36_864;

/// How a stream is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub fps: u32,
    pub bitrate_bps: u32,
    /// Tuned for motion (Game Boost) rather than for text and still pictures.
    /// Only OpenH264 has such presets; Windows' encoder has one for both.
    pub motion: bool,
}

pub struct Encoder {
    inner: Inner,
    keyframe: bool,
}

enum Inner {
    #[cfg(windows)]
    MediaFoundation {
        /// Created for the first picture's size, and again when it changes.
        current: Option<crate::mf_encode::Encoder>,
        settings: Settings,
        /// The graphics card's encoder rather than Windows' software one.
        hardware: bool,
    },
    OpenH264 {
        // Boxed: its parameters are large.
        encoder: Box<openh264::encoder::Encoder>,
        yuv: Option<YUVBuffer>,
    },
}

impl Encoder {
    /// The graphics card's encoder, else Windows' own, else OpenH264; see
    /// [`CHOICE_ENV`] for leaving the first ones out.
    pub fn best(settings: Settings) -> Result<Self> {
        Self::best_for(settings, std::env::var(CHOICE_ENV).ok().as_deref())
    }

    fn best_for(settings: Settings, value: Option<&str>) -> Result<Self> {
        let choice = choice(value);
        if choice == Choice::OpenH264 {
            return Self::openh264(settings);
        }
        #[cfg(windows)]
        {
            if choice == Choice::Best {
                match Self::hardware(settings) {
                    Ok(encoder) => return Ok(encoder),
                    Err(e) => tracing::info!("using Windows' software encoder: {e:#}"),
                }
            }
            match Self::media_foundation(settings) {
                Ok(encoder) => return Ok(encoder),
                Err(e) => tracing::info!("using OpenH264: {e:#}"),
            }
        }
        Self::openh264(settings)
    }

    pub fn openh264(settings: Settings) -> Result<Self> {
        let config = EncoderConfig::new()
            .usage_type(if settings.motion {
                UsageType::CameraVideoRealTime
            } else {
                UsageType::ScreenContentRealTime
            })
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(settings.bitrate_bps))
            .max_frame_rate(FrameRate::from_hz(settings.fps as f32))
            .complexity(Complexity::Low)
            // Let the encoder omit frames to respect the host's bandwidth budget.
            .skip_frames(settings.motion)
            .num_threads(if settings.motion { 4 } else { 2 });
        let encoder =
            openh264::encoder::Encoder::with_api_config(OpenH264API::from_source(), config)?;
        Ok(Self {
            inner: Inner::OpenH264 {
                encoder: Box::new(encoder),
                yuv: None,
            },
            keyframe: false,
        })
    }

    /// Windows' own encoder; an error where Windows has none.
    #[cfg(windows)]
    pub fn media_foundation(settings: Settings) -> Result<Self> {
        crate::mf_encode::Encoder::check_available()?;
        Ok(Self::windows(settings, false))
    }

    /// The graphics card's encoder; an error where there is none.
    #[cfg(windows)]
    pub fn hardware(settings: Settings) -> Result<Self> {
        crate::mf_encode::Encoder::check_hardware_available()?;
        Ok(Self::windows(settings, true))
    }

    #[cfg(windows)]
    fn windows(settings: Settings, hardware: bool) -> Self {
        Self {
            inner: Inner::MediaFoundation {
                current: None,
                settings,
                hardware,
            },
            keyframe: false,
        }
    }

    pub fn implementation(&self) -> Implementation {
        match self.inner {
            #[cfg(windows)]
            Inner::MediaFoundation { hardware: true, .. } => Implementation::Hardware,
            Inner::MediaFoundation { .. } => Implementation::MediaFoundation,
            Inner::OpenH264 { .. } => Implementation::OpenH264,
        }
    }

    /// Makes the next picture a keyframe.
    pub fn force_keyframe(&mut self) {
        self.keyframe = true;
    }

    /// Encodes one BGRA picture of `size` (even width and height) into `out`,
    /// which stays empty when the encoder skips the picture; whether it is a
    /// keyframe. Should the graphics card's encoder fail, Windows' software
    /// encoder takes over from this picture on, starting with a keyframe;
    /// should that fail, OpenH264 does. Pictures larger than H.264 decoders
    /// take are refused.
    pub fn encode(
        &mut self,
        bgra: &[u8],
        size: (usize, usize),
        timestamp_ms: u64,
        out: &mut Vec<u8>,
    ) -> Result<bool> {
        out.clear();
        if size.0.div_ceil(16) * size.1.div_ceil(16) > MAX_MACROBLOCKS {
            bail!(
                "a {}x{} picture is larger than H.264 decoders take (4096x2304 or 5120x1440 at most)",
                size.0,
                size.1
            );
        }
        let keyframe = std::mem::take(&mut self.keyframe);
        match &mut self.inner {
            #[cfg(windows)]
            Inner::MediaFoundation {
                current,
                settings,
                hardware,
            } => {
                let (settings, hardware) = (*settings, *hardware);
                let encoded = match current {
                    Some(encoder) if encoder.size() == size => Ok(encoder),
                    // A new encoder for a new size starts with a keyframe.
                    slot => crate::mf_encode::Encoder::new(size, settings, hardware)
                        .map(|encoder| slot.insert(encoder)),
                }
                .and_then(|encoder| encoder.encode(bgra, timestamp_ms, keyframe, out));
                encoded.or_else(|e| {
                    *self = if hardware {
                        tracing::warn!("using Windows' software encoder from here on: {e:#}");
                        Self::media_foundation(settings).or_else(|_| Self::openh264(settings))?
                    } else {
                        tracing::warn!("using OpenH264 from here on: {e:#}");
                        Self::openh264(settings)?
                    };
                    self.encode(bgra, size, timestamp_ms, out)
                })
            }
            Inner::OpenH264 { encoder, yuv } => {
                let buffer = match yuv {
                    Some(buffer) if buffer.dimensions() == size => buffer,
                    slot => slot.insert(YUVBuffer::new(size.0, size.1)),
                };
                buffer.read_bgra8(BgraSliceU8::new(bgra, size));
                if keyframe {
                    encoder.force_intra_frame();
                }
                let encoded = encoder.encode_at(buffer, Timestamp::from_millis(timestamp_ms))?;
                let keyframe = matches!(encoded.frame_type(), FrameType::IDR | FrameType::I);
                encoded.write_vec(out);
                Ok(keyframe)
            }
        }
    }
}

/// Where each NAL unit's start code begins in an Annex B access unit, and
/// where the unit itself does.
pub(crate) fn start_codes(annex_b: &[u8]) -> impl Iterator<Item = (usize, usize)> {
    let mut i = 0;
    std::iter::from_fn(move || {
        while i + 3 <= annex_b.len() {
            if annex_b[i..].starts_with(&[0, 0, 1]) {
                // A four-byte start code has one more zero in front.
                let code = if i > 0 && annex_b[i - 1] == 0 {
                    i - 1
                } else {
                    i
                };
                i += 3;
                return Some((code, i));
            }
            i += 1;
        }
        None
    })
}

/// The NAL units of an Annex B access unit, without start codes.
pub(crate) fn nal_units(annex_b: &[u8]) -> impl Iterator<Item = &[u8]> {
    let codes: Vec<_> = start_codes(annex_b).collect();
    let ends: Vec<_> = codes
        .iter()
        .skip(1)
        .map(|&(code, _)| code)
        .chain([annex_b.len()])
        .collect();
    codes
        .into_iter()
        .zip(ends)
        .map(move |((_, start), end)| &annex_b[start..end])
        .filter(|unit| !unit.is_empty())
}

pub(crate) fn nal_type(unit: &[u8]) -> u8 {
    unit.first().map_or(0, |header| header & 0x1F)
}

/// Whether an access unit holds an IDR picture.
pub(crate) fn is_keyframe(annex_b: &[u8]) -> bool {
    nal_units(annex_b).any(|unit| nal_type(unit) == 5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Decoder;

    const SETTINGS: Settings = Settings {
        fps: 30,
        bitrate_bps: 4_000_000,
        motion: false,
    };

    /// A desktop-like picture: text stripes, and a window that moves.
    fn scene(width: usize, height: usize, frame: usize) -> Vec<u8> {
        let mut bgra = vec![0u8; width * height * 4];
        for (i, pixel) in bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i % width, i / width);
            let text = y % 12 < 8 && (x / 5 + y / 12) % 4 != 0 && x % 5 < 3;
            let v = if text {
                30
            } else {
                230 - (x * 40 / width) as u8
            };
            *pixel = [v, v, v.saturating_sub(20), 255];
        }
        let left = (frame * 6) % (width - 32);
        for y in height / 3..height / 3 + 24 {
            for x in left..left + 32 {
                bgra[(y * width + x) * 4..][..4].copy_from_slice(&[200, 90, 40, 255]);
            }
        }
        bgra
    }

    /// Y, U and V.
    type Planes = [Vec<u8>; 3];

    /// The planes of the source picture, as both encoders see them.
    fn source_planes(bgra: &[u8], size: (usize, usize)) -> Planes {
        let mut yuv = YUVBuffer::new(size.0, size.1);
        yuv.read_bgra8(BgraSliceU8::new(bgra, size));
        [yuv.y().to_vec(), yuv.u().to_vec(), yuv.v().to_vec()]
    }

    /// Peak signal-to-noise ratio of a decoded plane against the source's.
    fn psnr(decoded: &[u8], source: &[u8]) -> f64 {
        let error: f64 = decoded
            .iter()
            .zip(source)
            .map(|(a, b)| (f64::from(*a) - f64::from(*b)).powi(2))
            .sum::<f64>()
            / source.len() as f64;
        10.0 * (255.0 * 255.0 / error.max(1e-9)).log10()
    }

    fn decode(decoder: &mut Decoder, unit: &[u8]) -> Option<((usize, usize), Planes)> {
        let picture = decoder.decode(unit).unwrap()?;
        let (size, y, u, v) = crate::tests::planes(&picture);
        Some((size, [y, u, v]))
    }

    /// Encodes `frames` pictures, forcing a keyframe at `force` if given, and
    /// checks what every viewer needs from the result.
    fn check_encoder(mut encoder: Encoder, sizes: &[(usize, usize)], force: Option<usize>) {
        let implementation = encoder.implementation();
        let mut openh264 = Decoder::openh264().unwrap();
        #[cfg(windows)]
        let mut windows = Decoder::media_foundation().ok();
        let mut out = Vec::new();
        for (i, &size) in sizes.iter().enumerate() {
            let pixels = scene(size.0, size.1, i);
            if force == Some(i) {
                encoder.force_keyframe();
            }
            let keyframe = encoder
                .encode(&pixels, size, i as u64 * 33, &mut out)
                .unwrap();
            // Still the encoder under test, not OpenH264 standing in.
            assert_eq!(encoder.implementation(), implementation, "frame {i}");
            // A picture in, its access unit out: no added latency.
            assert!(!out.is_empty(), "frame {i} gave no data");
            let first_of_size = i == 0 || sizes[i - 1] != size;
            if first_of_size || force == Some(i) {
                assert!(keyframe, "frame {i} should be a keyframe");
                // Self-contained: a fresh decoder (a viewer that just
                // connected or recovered) starts here.
                let mut fresh = Decoder::openh264().unwrap();
                assert!(decode(&mut fresh, &out).is_some(), "frame {i}");
            }
            // Keyframes only when needed: they cost several ordinary frames.
            assert_eq!(keyframe, first_of_size || force == Some(i), "frame {i}");
            assert_eq!(keyframe, is_keyframe(&out), "frame {i}");
            let (dims, decoded) = decode(&mut openh264, &out).expect("OpenH264 decodes it");
            assert_eq!(dims, size, "frame {i}");
            let source = source_planes(&pixels, size);
            for (plane, (decoded, source)) in
                ["Y", "U", "V"].iter().zip(decoded.iter().zip(&source))
            {
                let quality = psnr(decoded, source);
                assert!(quality > 30.0, "frame {i}: {plane} PSNR {quality:.1} dB");
            }
            #[cfg(windows)]
            if let Some(windows) = &mut windows {
                let (dims, same) = decode(windows, &out).expect("Windows decodes it");
                assert_eq!(
                    (dims, &same),
                    (size, &decoded),
                    "frame {i}: decoders disagree"
                );
            }
        }
    }

    #[test]
    fn openh264_output_suits_every_viewer() {
        let mut sizes = vec![(128, 96); 6];
        sizes.extend([(176, 120); 4]);
        check_encoder(Encoder::openh264(SETTINGS).unwrap(), &sizes, Some(3));
    }

    #[cfg(windows)]
    fn windows_or_skip(settings: Settings) -> Option<Encoder> {
        let made = Encoder::media_foundation(settings);
        crate::or_skip("Windows H.264 encoder", "TIDEDESK_REQUIRE_MF", made)
    }

    /// The graphics card's encoder, or `None` where there is none (as on CI);
    /// `TIDEDESK_REQUIRE_HW=1` turns its absence into a failure.
    #[cfg(windows)]
    fn hardware_or_skip(settings: Settings) -> Option<Encoder> {
        let made = Encoder::hardware(settings);
        crate::or_skip("hardware H.264 encoder", "TIDEDESK_REQUIRE_HW", made)
    }

    /// Every encoder this computer has. Hardware encoders take pictures from
    /// 128 rows up, so the tests that use them all use [`SIZE`].
    fn encoders(settings: Settings) -> Vec<Encoder> {
        let mut encoders = vec![Encoder::openh264(settings).unwrap()];
        #[cfg(windows)]
        {
            encoders.extend(windows_or_skip(settings));
            encoders.extend(hardware_or_skip(settings));
        }
        encoders
    }

    const SIZE: (usize, usize) = (256, 144);

    #[cfg(windows)]
    #[test]
    fn windows_output_suits_every_viewer_at_once() {
        let Some(encoder) = windows_or_skip(SETTINGS) else {
            return;
        };
        assert_eq!(encoder.implementation(), Implementation::MediaFoundation);
        // A forced keyframe, then a size change with a cropped width and
        // height (a 1366x768 laptop screen has the same shape).
        let mut sizes = vec![(128, 96); 6];
        sizes.extend([(180, 120); 4]);
        check_encoder(encoder, &sizes, Some(3));
    }

    #[cfg(windows)]
    #[test]
    fn openh264_takes_over_when_windows_fails() {
        let Some(mut encoder) = windows_or_skip(SETTINGS) else {
            return;
        };
        let mut out = Vec::new();
        encoder
            .encode(&scene(128, 96, 0), (128, 96), 0, &mut out)
            .unwrap();
        // Windows refuses pictures this small; OpenH264 does not.
        let tiny = vec![128; 16 * 16 * 4];
        let keyframe = encoder.encode(&tiny, (16, 16), 33, &mut out).unwrap();
        assert_eq!(encoder.implementation(), Implementation::OpenH264);
        assert!(keyframe);
        let mut fresh = Decoder::openh264().unwrap();
        assert_eq!(decode(&mut fresh, &out).unwrap().0, (16, 16));
    }

    #[cfg(windows)]
    #[test]
    fn hardware_output_suits_every_viewer_at_once() {
        let Some(encoder) = hardware_or_skip(SETTINGS) else {
            return;
        };
        assert_eq!(encoder.implementation(), Implementation::Hardware);
        // A forced keyframe, then a size change with a cropped width and
        // height.
        let mut sizes = vec![SIZE; 6];
        sizes.extend([(330, 180); 4]);
        check_encoder(encoder, &sizes, Some(3));
    }

    #[cfg(windows)]
    #[test]
    fn windows_software_takes_over_when_the_hardware_fails() {
        let Some(mut encoder) = hardware_or_skip(SETTINGS) else {
            return;
        };
        let mut out = Vec::new();
        encoder
            .encode(&scene(SIZE.0, SIZE.1, 0), SIZE, 0, &mut out)
            .unwrap();
        // H.264 hardware encoders stop at 4096 columns; a 5120x1440 display
        // is for Windows' software encoder.
        let wide = scene(5120, 1440, 0);
        let keyframe = encoder.encode(&wide, (5120, 1440), 33, &mut out).unwrap();
        assert_eq!(encoder.implementation(), Implementation::MediaFoundation);
        assert!(keyframe);
        let mut fresh = Decoder::openh264().unwrap();
        assert_eq!(decode(&mut fresh, &out).unwrap().0, (5120, 1440));
    }

    #[test]
    fn every_encoder_writes_constrained_baseline() {
        for mut encoder in encoders(SETTINGS) {
            let name = encoder.implementation();
            let mut out = Vec::new();
            encoder
                .encode(&scene(SIZE.0, SIZE.1, 0), SIZE, 0, &mut out)
                .unwrap();
            assert_eq!(encoder.implementation(), name);
            let sps = nal_units(&out)
                .find(|unit| nal_type(unit) == 7)
                .expect("parameter sets in front of the keyframe");
            // profile_idc 66 (baseline) with constraint_set1 (constrained).
            assert_eq!(sps[1], 66, "{name}: profile {}", sps[1]);
            assert_ne!(sps[2] & 0x40, 0, "{name}: constraint flags {:#04x}", sps[2]);
        }
    }

    /// The host refreshes a viewer by forcing a keyframe of an unchanged
    /// picture; Game Boost's frame skipping must not swallow it.
    #[test]
    fn keyframes_come_on_request_even_on_a_still_screen() {
        let encoders = [false, true].into_iter().flat_map(|motion| {
            let settings = Settings { motion, ..SETTINGS };
            encoders(settings)
                .into_iter()
                .map(move |encoder| (motion, encoder))
        });
        let still = scene(SIZE.0, SIZE.1, 0);
        for (motion, mut encoder) in encoders {
            let name = encoder.implementation();
            let mut out = Vec::new();
            for i in 0..6 {
                if i == 4 {
                    encoder.force_keyframe();
                }
                let keyframe = encoder.encode(&still, SIZE, i * 33, &mut out).unwrap();
                assert_eq!(
                    keyframe,
                    i == 0 || i == 4,
                    "{name}, motion {motion}, frame {i}"
                );
                if keyframe {
                    // A viewer that just connected starts here.
                    let mut fresh = Decoder::openh264().unwrap();
                    assert!(decode(&mut fresh, &out).is_some(), "{name}, frame {i}");
                }
            }
            assert_eq!(encoder.implementation(), name);
        }
    }

    /// A still screen costs a small part of the bitrate: no encoder may pad
    /// its frames up to it, as some hardware encoders do at a constant
    /// bitrate.
    #[test]
    fn a_still_screen_costs_almost_nothing() {
        let budget = (SETTINGS.bitrate_bps / SETTINGS.fps / 8) as usize;
        let still = scene(SIZE.0, SIZE.1, 0);
        for mut encoder in encoders(SETTINGS) {
            let name = encoder.implementation();
            let mut out = Vec::new();
            for i in 0..8 {
                encoder.encode(&still, SIZE, i * 33, &mut out).unwrap();
                if i > 0 {
                    let size = out.len();
                    assert!(
                        size < budget / 10,
                        "{name}, frame {i}: {size} of {budget} bytes"
                    );
                }
            }
            assert_eq!(encoder.implementation(), name);
        }
    }

    #[test]
    fn pictures_larger_than_decoders_take_are_refused() {
        for mut encoder in encoders(SETTINGS) {
            let name = encoder.implementation();
            // 5K: 57,600 macroblocks. Refused before the pixels are read.
            let error = encoder.encode(&[], (5120, 2880), 0, &mut Vec::new());
            let error = error.unwrap_err().to_string();
            assert!(
                error.contains("larger than H.264 decoders take"),
                "{name}: {error}"
            );
            assert_eq!(encoder.implementation(), name);
        }
    }

    #[test]
    fn keyframes_are_found_by_their_idr_unit() {
        let idr = [
            0, 0, 0, 1, 0x67, 66, 0xC0, 0, 0, 1, 0x68, 1, 0, 0, 1, 0x65, 9, 9,
        ];
        let units: Vec<u8> = nal_units(&idr).map(nal_type).collect();
        assert_eq!(units, [7, 8, 5]);
        assert!(is_keyframe(&idr));
        assert!(!is_keyframe(&[0, 0, 1, 0x41, 7, 7]));
        assert!(!is_keyframe(&[]));
    }

    #[test]
    fn the_choice_picks_the_encoder() {
        let forced = Encoder::best_for(SETTINGS, Some("openh264")).unwrap();
        assert_eq!(forced.implementation(), Implementation::OpenH264);
        #[cfg(windows)]
        if windows_or_skip(SETTINGS).is_some() {
            let software = Encoder::best_for(SETTINGS, Some("software")).unwrap();
            assert_eq!(software.implementation(), Implementation::MediaFoundation);
            let best = Encoder::best_for(SETTINGS, None).unwrap().implementation();
            match hardware_or_skip(SETTINGS) {
                Some(_) => assert_eq!(best, Implementation::Hardware),
                None => assert_eq!(best, Implementation::MediaFoundation),
            }
        }
    }
}
