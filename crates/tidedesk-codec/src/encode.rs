//! Encoding: the graphics card's H.264 encoder where there is one, then
//! Windows' own, then OpenH264. All write constrained-baseline Annex B H.264
//! with the parameter sets in front of every keyframe, which every TideDesk
//! viewer decodes.

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::{Result, bail};
use openh264::encoder::{
    BitRate, Complexity, EncoderConfig, FrameRate, FrameType, RateControlMode, UsageType,
};
use openh264::formats::{BgraSliceU8, YUVBuffer, YUVSource};
use openh264::{OpenH264API, Timestamp};

use crate::{CHOICE_ENV, Choice, Implementation, choice};

/// A picture to encode: BGRA pixels, row after row, or on Windows a BGRA
/// (`DXGI_FORMAT_B8G8R8A8_UNORM`) Direct3D 11 texture, as screen capture hands
/// it over. The graphics card's encoder takes a texture without it leaving the
/// card; the others get its pixels copied back. For that, the texture's device
/// must be made with `D3D11_CREATE_DEVICE_VIDEO_SUPPORT`, and the encoder
/// turns on the device's multithread protection, as it uses the device from
/// its own threads too.
#[derive(Clone, Copy)]
pub enum Image<'a> {
    Bgra(&'a [u8]),
    #[cfg(windows)]
    Texture(&'a windows::Win32::Graphics::Direct3D11::ID3D11Texture2D),
}

impl<'a> From<&'a [u8]> for Image<'a> {
    fn from(bgra: &'a [u8]) -> Self {
        Self::Bgra(bgra)
    }
}

impl<'a> From<&'a Vec<u8>> for Image<'a> {
    fn from(bgra: &'a Vec<u8>) -> Self {
        Self::Bgra(bgra)
    }
}

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

/// A picture an encoder finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Encoded {
    /// As given to [`Encoder::send`] with the picture.
    pub timestamp_ms: u64,
    pub size: (usize, usize),
    pub keyframe: bool,
}

/// What [`Encoder::receive`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Received {
    /// The next picture, in the order they were sent.
    Picture(Encoded),
    /// No picture is finished yet, or none was sent.
    Waiting,
    /// The graphics card's encoder failed, and the pictures it held are
    /// lost. Windows' software encoder takes over with the next picture,
    /// starting with a keyframe; should that fail, OpenH264 does.
    Lost,
}

pub struct Encoder {
    inner: Inner,
    keyframe: bool,
    /// Pictures in the graphics card's encoder, oldest first: their
    /// timestamps and sizes.
    flying: VecDeque<(u64, (usize, usize))>,
    /// Pictures finished and not taken yet, oldest first.
    ready: VecDeque<(Encoded, Vec<u8>)>,
    /// The buffer the last picture was taken into, for the next.
    spare: Vec<u8>,
    /// Pixels of textures, for encoders that need them in system memory.
    #[cfg(windows)]
    readback: crate::gpu::Readback,
}

enum Inner {
    #[cfg(windows)]
    MediaFoundation {
        /// Created for the first picture's size, and again when it changes.
        /// Boxed: it holds the card's converter.
        current: Option<Box<crate::mf_encode::Encoder>>,
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
        Ok(Self::with(Inner::OpenH264 {
            encoder: Box::new(encoder),
            yuv: None,
        }))
    }

    fn with(inner: Inner) -> Self {
        Self {
            inner,
            keyframe: false,
            flying: VecDeque::new(),
            ready: VecDeque::new(),
            spare: Vec::new(),
            #[cfg(windows)]
            readback: Default::default(),
        }
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
        Self::with(Inner::MediaFoundation {
            current: None,
            settings,
            hardware,
        })
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

    /// Pictures the encoder gets at once: two for a graphics card's that
    /// works on the next picture while it encodes the last, so that the time
    /// it takes over each does not bound the frame rate. That shows in how
    /// long its pictures take, so it is one for its first pictures, and stays
    /// one where a picture only waits for the one before it; as it is for the
    /// other encoders.
    pub fn depth(&self) -> usize {
        match &self.inner {
            #[cfg(windows)]
            Inner::MediaFoundation {
                current: Some(encoder),
                ..
            } => encoder.depth(),
            _ => 1,
        }
    }

    /// Pictures sent and not received yet.
    pub fn in_flight(&self) -> usize {
        self.flying.len() + self.ready.len()
    }

    /// Encodes one picture, the top-left `size` of `image` (even width and
    /// height), into `out`, which stays empty when the encoder skips the
    /// picture; whether it is a keyframe. This is [`Encoder::send`] and
    /// [`Encoder::receive`] in one, for an encoder with no pictures in
    /// flight.
    pub fn encode<'a>(
        &mut self,
        image: impl Into<Image<'a>>,
        size: (usize, usize),
        timestamp_ms: u64,
        out: &mut Vec<u8>,
    ) -> Result<bool> {
        let image = image.into();
        out.clear();
        self.send(image, size, timestamp_ms)?;
        loop {
            match self.receive(Duration::from_millis(100), out)? {
                Received::Picture(encoded) => return Ok(encoded.keyframe),
                Received::Waiting => {}
                // The encoder that took over encodes it.
                Received::Lost => self.send(image, size, timestamp_ms)?,
            }
        }
    }

    /// Hands over one picture, the top-left `size` of `image` (even width
    /// and height), for [`Encoder::receive`] to give back encoded. The
    /// graphics card's encoder takes it without waiting for the one before,
    /// up to [`Encoder::depth`] at once; the others encode it here. Should
    /// the graphics card's encoder fail, Windows' software encoder takes over
    /// from this picture on, starting with a keyframe, and the pictures the
    /// card held are lost; should that fail, OpenH264 does. Pictures larger
    /// than H.264 decoders take are refused.
    pub fn send<'a>(
        &mut self,
        image: impl Into<Image<'a>>,
        size: (usize, usize),
        timestamp_ms: u64,
    ) -> Result<()> {
        let image = image.into();
        if let Image::Bgra(bgra) = image
            && bgra.len() != size.0 * size.1 * 4
        {
            bail!("{} bytes for a {}x{} picture", bgra.len(), size.0, size.1);
        }
        if size.0.div_ceil(16) * size.1.div_ceil(16) > MAX_MACROBLOCKS {
            bail!(
                "a {}x{} picture is larger than H.264 decoders take (4096x2304 or 5120x1440 at most)",
                size.0,
                size.1
            );
        }
        if self.in_flight() >= self.depth() {
            bail!("the encoder holds {} pictures already", self.in_flight());
        }
        let keyframe = std::mem::take(&mut self.keyframe);
        let mut data = std::mem::take(&mut self.spare);
        data.clear();
        match self.hand_over(image, size, (timestamp_ms, keyframe), &mut data) {
            Ok(Some(keyframe)) => {
                let encoded = Encoded {
                    timestamp_ms,
                    size,
                    keyframe,
                };
                self.ready.push_back((encoded, data));
                Ok(())
            }
            Ok(None) => {
                self.spare = data;
                self.flying.push_back((timestamp_ms, size));
                Ok(())
            }
            Err(e) => {
                self.fall_back(e)?;
                self.send(image, size, timestamp_ms)
            }
        }
    }

    /// The next picture finished, in `out` (empty when the encoder skipped
    /// it), waiting up to `wait` for the graphics card's encoder to finish
    /// it.
    pub fn receive(&mut self, wait: Duration, out: &mut Vec<u8>) -> Result<Received> {
        out.clear();
        if let Some((encoded, data)) = self.ready.pop_front() {
            self.spare = std::mem::replace(out, data);
            return Ok(Received::Picture(encoded));
        }
        #[cfg(not(windows))]
        let _ = wait;
        #[cfg(windows)]
        if let Inner::MediaFoundation {
            current: Some(encoder),
            ..
        } = &mut self.inner
            && let Some(&(timestamp_ms, size)) = self.flying.front()
        {
            match encoder.receive(wait, out) {
                Ok(Some((_, keyframe))) => {
                    self.flying.pop_front();
                    return Ok(Received::Picture(Encoded {
                        timestamp_ms,
                        size,
                        keyframe,
                    }));
                }
                Ok(None) => {}
                Err(e) => {
                    out.clear();
                    self.fall_back(e)?;
                    return Ok(Received::Lost);
                }
            }
        }
        Ok(Received::Waiting)
    }

    /// Hands the picture to the encoder in use. One that finishes it at once
    /// writes it into `data`: whether it is a keyframe.
    fn hand_over(
        &mut self,
        image: Image<'_>,
        size: (usize, usize),
        (timestamp_ms, keyframe): (u64, bool),
        data: &mut Vec<u8>,
    ) -> Result<Option<bool>> {
        match &mut self.inner {
            #[cfg(windows)]
            Inner::MediaFoundation {
                current,
                settings,
                hardware,
            } => {
                use crate::mf_encode::Encoder as Windows;
                let (settings, hardware) = (*settings, *hardware);
                let texture = match image {
                    Image::Texture(texture) if hardware => Some(texture),
                    _ => None,
                };
                let fits = current.as_ref().is_some_and(|encoder| {
                    encoder.size() == size
                        && texture.is_none_or(|t| !encoder.zero_copy() || encoder.same_device(t))
                });
                if !fits {
                    // The pictures the old encoder holds come out first.
                    while let (Some(old), Some(&(timestamp_ms, size))) =
                        (current.as_mut(), self.flying.front())
                    {
                        let mut finished = Vec::new();
                        let Some((_, keyframe)) =
                            old.receive(Duration::from_secs(1), &mut finished)?
                        else {
                            bail!("the hardware H.264 encoder did not finish its pictures");
                        };
                        let encoded = Encoded {
                            timestamp_ms,
                            size,
                            keyframe,
                        };
                        self.flying.pop_front();
                        self.ready.push_back((encoded, finished));
                    }
                    // A new encoder, for a new size or card, starts with a
                    // keyframe.
                    *current = None;
                    let made = match texture {
                        Some(texture) => {
                            Windows::on_texture_card(texture, size, settings).or_else(|e| {
                                tracing::debug!(
                                    "the picture leaves the graphics card to be encoded: {e:#}"
                                );
                                Windows::new(size, settings, hardware)
                            })
                        }
                        None => Windows::new(size, settings, hardware),
                    };
                    *current = Some(Box::new(made?));
                }
                let encoder = current.as_mut().expect("made above");
                // A texture stays on its card when that card's encoder takes
                // it; otherwise its pixels are copied back.
                let bgra = match image {
                    Image::Texture(texture) if encoder.zero_copy() => {
                        encoder.send_texture(texture, timestamp_ms, keyframe)?;
                        return Ok(None);
                    }
                    Image::Texture(texture) => self.readback.read(texture, size)?,
                    Image::Bgra(bgra) => bgra,
                };
                if hardware {
                    encoder.send(bgra, timestamp_ms, keyframe)?;
                    return Ok(None);
                }
                encoder.encode(bgra, timestamp_ms, keyframe, data).map(Some)
            }
            Inner::OpenH264 { encoder, yuv } => {
                let bgra = match image {
                    Image::Bgra(bgra) => bgra,
                    #[cfg(windows)]
                    Image::Texture(texture) => self.readback.read(texture, size)?,
                };
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
                encoded.write_vec(data);
                Ok(Some(keyframe))
            }
        }
    }

    /// Replaces a Windows encoder that failed with `error` by the next one
    /// down; the pictures it held are lost. OpenH264 has none to follow it.
    fn fall_back(&mut self, error: anyhow::Error) -> Result<()> {
        match self.inner {
            #[cfg(windows)]
            Inner::MediaFoundation {
                settings, hardware, ..
            } => {
                let next = if hardware {
                    tracing::warn!("using Windows' software encoder from here on: {error:#}");
                    Self::media_foundation(settings).or_else(|e| {
                        tracing::warn!("using OpenH264 from here on: {e:#}");
                        Self::openh264(settings)
                    })?
                } else {
                    tracing::warn!("using OpenH264 from here on: {error:#}");
                    Self::openh264(settings)?
                };
                self.flying.clear();
                self.inner = next.inner;
                Ok(())
            }
            Inner::OpenH264 { .. } => Err(error),
        }
    }

    /// Whether the last picture was encoded without leaving the card.
    #[cfg(all(test, windows))]
    pub(crate) fn on_card(&self) -> bool {
        matches!(&self.inner, Inner::MediaFoundation { current: Some(encoder), .. } if encoder.zero_copy())
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
    use std::time::Duration;

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

    /// How check_encoder hands pictures over.
    #[derive(Clone, Copy)]
    enum Input<'a> {
        Pixels,
        #[cfg(windows)]
        Textures(&'a windows::Win32::Graphics::Direct3D11::ID3D11Device),
        #[cfg(not(windows))]
        #[allow(dead_code)]
        Never(std::marker::PhantomData<&'a ()>),
    }

    fn check_encoder(encoder: Encoder, sizes: &[(usize, usize)], force: Option<usize>) {
        check_encoder_with(Input::Pixels, encoder, sizes, force)
    }

    /// Encodes `frames` pictures, forcing a keyframe at `force` if given, and
    /// checks what every viewer needs from the result.
    fn check_encoder_with(
        input: Input<'_>,
        mut encoder: Encoder,
        sizes: &[(usize, usize)],
        force: Option<usize>,
    ) {
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
            let keyframe = match input {
                #[cfg(windows)]
                Input::Textures(device) => {
                    let texture = crate::gpu::tests::upload(device, &pixels, size);
                    encoder.encode(Image::Texture(&texture), size, i as u64 * 33, &mut out)
                }
                _ => encoder.encode(&pixels, size, i as u64 * 33, &mut out),
            }
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

    /// Screen capture hands pictures over as textures; every encoder takes
    /// them, the card's own without the picture leaving the card.
    #[cfg(windows)]
    #[test]
    fn every_encoder_takes_textures() {
        let Some(device) = crate::gpu::tests::device() else {
            return;
        };
        for encoder in encoders(SETTINGS) {
            check_encoder_with(Input::Textures(&device), encoder, &[SIZE; 4], Some(2));
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_cards_encoder_keeps_textures_on_the_card() {
        let (Some(device), Some(mut encoder)) =
            (crate::gpu::tests::device(), hardware_or_skip(SETTINGS))
        else {
            return;
        };
        let texture = crate::gpu::tests::upload(&device, &scene(SIZE.0, SIZE.1, 0), SIZE);
        let mut out = Vec::new();
        assert!(
            encoder
                .encode(Image::Texture(&texture), SIZE, 0, &mut out)
                .unwrap()
        );
        assert!(encoder.on_card());
        // Pixels in system memory still work, uploaded to the card.
        let pixels = scene(SIZE.0, SIZE.1, 1);
        encoder.encode(&pixels, SIZE, 33, &mut out).unwrap();
        assert_eq!(encoder.implementation(), Implementation::Hardware);
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
            let pixels = vec![0; 5120 * 2880 * 4];
            let error = encoder.encode(&pixels, (5120, 2880), 0, &mut Vec::new());
            let error = error.unwrap_err().to_string();
            assert!(
                error.contains("larger than H.264 decoders take"),
                "{name}: {error}"
            );
            assert_eq!(encoder.implementation(), name);
        }
    }

    #[test]
    fn a_picture_of_the_wrong_size_is_an_error() {
        for mut encoder in encoders(SETTINGS) {
            let name = encoder.implementation();
            let error = encoder
                .encode(Image::Bgra(&[]), SIZE, 0, &mut Vec::new())
                .unwrap_err();
            assert!(
                error.to_string().contains("0 bytes for a 256x144"),
                "{name}: {error}"
            );
        }
    }

    /// Takes the next picture back from `encoder`, which has one in flight,
    /// and checks that a viewer decodes it: its timestamp.
    fn take_back(encoder: &mut Encoder, decoder: &mut Decoder, out: &mut Vec<u8>) -> u64 {
        let name = encoder.implementation();
        match encoder.receive(Duration::from_secs(2), out).unwrap() {
            Received::Picture(encoded) => {
                assert_eq!(encoded.size, SIZE, "{name}");
                assert_eq!(encoded.keyframe, is_keyframe(out), "{name}");
                let (size, _) = decode(decoder, out).expect("a viewer decodes it");
                assert_eq!(size, SIZE, "{name}");
                encoded.timestamp_ms
            }
            Received::Waiting => panic!("{name} did not finish its picture"),
            Received::Lost => panic!("{name} lost its pictures"),
        }
    }

    /// Pictures handed over come back in the order they went in, each with
    /// its own timestamp, from every encoder: the host sends them on as they
    /// come.
    #[test]
    fn pictures_sent_come_back_in_order_with_their_timestamps() {
        for mut encoder in encoders(SETTINGS) {
            let name = encoder.implementation();
            let mut decoder = Decoder::openh264().unwrap();
            let (mut out, mut back) = (Vec::new(), Vec::new());
            assert!(
                matches!(
                    encoder.receive(Duration::ZERO, &mut out).unwrap(),
                    Received::Waiting
                ),
                "{name}: nothing went in yet"
            );
            // Enough for the graphics card's encoder to get two at once.
            for i in 0..20 {
                while encoder.in_flight() >= encoder.depth() {
                    back.push(take_back(&mut encoder, &mut decoder, &mut out));
                }
                encoder
                    .send(&scene(SIZE.0, SIZE.1, i), SIZE, i as u64 * 33)
                    .unwrap();
                assert!(encoder.in_flight() <= encoder.depth(), "{name}");
            }
            while encoder.in_flight() > 0 {
                back.push(take_back(&mut encoder, &mut decoder, &mut out));
            }
            let sent: Vec<u64> = (0..20).map(|i| i * 33).collect();
            assert_eq!(back, sent, "{name}");
            assert_eq!(encoder.implementation(), name);
        }
    }

    /// Encodes pictures one at a time until the graphics card's encoder
    /// knows how long one takes, and gets two at once: how many it took.
    #[cfg(windows)]
    fn until_two_at_once(
        encoder: &mut Encoder,
        mut send: impl FnMut(&mut Encoder, usize),
    ) -> usize {
        let mut out = Vec::new();
        for i in 0..20 {
            if encoder.depth() == 2 {
                return i;
            }
            send(encoder, i);
            let received = encoder.receive(Duration::from_secs(2), &mut out).unwrap();
            assert!(matches!(received, Received::Picture(_)), "picture {i}");
        }
        panic!("the encoder never got two pictures at once");
    }

    /// The graphics card's encoder gets the next picture while it is still
    /// encoding the last, so that the frame rate is not bound by the time a
    /// picture takes, once it is known how long that is; the others finish
    /// each picture before the next.
    #[test]
    fn only_the_cards_encoder_holds_two_pictures_at_once() {
        for mut encoder in encoders(SETTINGS) {
            let name = encoder.implementation();
            assert_eq!(encoder.depth(), 1, "{name}");
            #[cfg(windows)]
            if name == Implementation::Hardware {
                let first = until_two_at_once(&mut encoder, |encoder, i| {
                    let pixels = scene(SIZE.0, SIZE.1, i);
                    encoder.send(&pixels, SIZE, i as u64 * 33).unwrap();
                });
                let mut decoder = Decoder::openh264().unwrap();
                let mut out = Vec::new();
                encoder.force_keyframe();
                // Two in, back to back, before anything is taken out.
                for i in first..first + 2 {
                    encoder
                        .send(&scene(SIZE.0, SIZE.1, i), SIZE, i as u64 * 33)
                        .unwrap();
                }
                assert_eq!(encoder.in_flight(), 2);
                for i in first..first + 2 {
                    let timestamp = take_back(&mut encoder, &mut decoder, &mut out);
                    assert_eq!(timestamp, i as u64 * 33);
                }
                assert_eq!(encoder.in_flight(), 0);
                assert_eq!(encoder.implementation(), name);
                continue;
            }
            // A second picture is refused, not queued.
            let pixels = scene(SIZE.0, SIZE.1, 0);
            encoder.send(&pixels, SIZE, 0).unwrap();
            let error = encoder.send(&pixels, SIZE, 33).unwrap_err().to_string();
            assert!(
                error.contains("holds 1 pictures already"),
                "{name}: {error}"
            );
            assert_eq!(encoder.implementation(), name);
        }
    }

    /// Textures too: on the card each picture in flight has a texture of its
    /// own, so the one being encoded is not overwritten by the next.
    #[cfg(windows)]
    #[test]
    fn textures_in_flight_keep_their_own_picture() {
        let (Some(device), Some(mut encoder)) =
            (crate::gpu::tests::device(), hardware_or_skip(SETTINGS))
        else {
            return;
        };
        let first = until_two_at_once(&mut encoder, |encoder, i| {
            let texture = crate::gpu::tests::upload(&device, &scene(SIZE.0, SIZE.1, i), SIZE);
            let at = i as u64 * 33;
            encoder.send(Image::Texture(&texture), SIZE, at).unwrap();
        });
        let mut decoder = Decoder::openh264().unwrap();
        let mut out = Vec::new();
        encoder.force_keyframe();
        let pictures: Vec<Vec<u8>> = (0..6).map(|i| scene(SIZE.0, SIZE.1, i * 9)).collect();
        let mut decoded = Vec::new();
        let mut take = |encoder: &mut Encoder, decoded: &mut Vec<Planes>| {
            let Received::Picture(_) = encoder.receive(Duration::from_secs(2), &mut out).unwrap()
            else {
                panic!("the card did not finish its picture");
            };
            decoded.push(decode(&mut decoder, &out).expect("a viewer decodes it").1);
        };
        let mut most = 0;
        for (i, pixels) in pictures.iter().enumerate() {
            while encoder.in_flight() >= encoder.depth() {
                take(&mut encoder, &mut decoded);
            }
            let texture = crate::gpu::tests::upload(&device, pixels, SIZE);
            let at = (first + i) as u64 * 33;
            encoder.send(Image::Texture(&texture), SIZE, at).unwrap();
            most = most.max(encoder.in_flight());
        }
        while encoder.in_flight() > 0 {
            take(&mut encoder, &mut decoded);
        }
        assert_eq!(most, 2);
        assert!(encoder.on_card());
        assert_eq!(decoded.len(), pictures.len());
        for (i, (decoded, pixels)) in decoded.iter().zip(&pictures).enumerate() {
            let quality = psnr(&decoded[0], &source_planes(pixels, SIZE)[0]);
            assert!(
                quality > 30.0,
                "picture {i} is not its own: {quality:.1} dB"
            );
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
