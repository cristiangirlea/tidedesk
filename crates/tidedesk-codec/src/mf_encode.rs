//! Windows' own H.264 encoder (Media Foundation), used through its transform
//! interface in the synchronous, system-memory mode.

use std::mem::ManuallyDrop;

use anyhow::{Context, Result, bail};
use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264EncoderMFT, CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode,
    CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode,
    CODECAPI_AVScenarioInfo, ICodecAPI, IMFMediaBuffer, IMFSample, IMFTransform, MF_E_NOTACCEPTING,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY,
    MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFVideoFormat_H264, MFVideoFormat_NV12,
    MFVideoInterlace_Progressive, eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_Base,
    eAVEncH264VProfile_ConstrainedBase, eAVScenarioInfo_DisplayRemoting,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_UI4};
use windows::core::{GUID, Interface};

use openh264::formats::{BgraSliceU8, YUVBuffer, YUVSource};

use crate::encode::{Settings, nal_type, nal_units, start_codes};

/// Pictures between the keyframes the encoder adds on its own. The host asks
/// for one whenever a viewer needs it, so these are only a backstop.
const KEYFRAME_INTERVAL_S: u32 = 3600;

pub struct Encoder {
    transform: IMFTransform,
    codec: ICodecAPI,
    size: (usize, usize),
    /// The picture in I420, by OpenH264's fast conversion (the same colours
    /// as when OpenH264 encodes), before it is interleaved into NV12.
    yuv: YUVBuffer,
    /// SPS and PPS, for a keyframe the encoder writes without them.
    parameter_sets: Vec<u8>,
    /// The sample the encoder writes into, unless it brings its own.
    output: Option<(IMFSample, IMFMediaBuffer)>,
    output_size: u32,
    provides_samples: bool,
    frame_duration: i64,
}

enum Output {
    Data,
    NeedMoreInput,
    StreamChanged,
}

fn create() -> Result<IMFTransform> {
    // Media Foundation needs COM on this thread; one already set up is fine.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    crate::mf::startup()?;
    unsafe { CoCreateInstance(&CLSID_MSH264EncoderMFT, None, CLSCTX_INPROC_SERVER) }
        .context("Windows has no H.264 encoder (Windows N editions need the Media Feature Pack)")
}

/// Width and height, or numerator and denominator, as Media Foundation packs them.
fn pair(high: usize, low: usize) -> u64 {
    ((high as u64) << 32) | low as u64
}

/// Puts SPS and PPS in front of a keyframe written without them, as some
/// encoders do, so that a viewer can start decoding there.
fn with_parameter_sets(out: &mut Vec<u8>, parameter_sets: &[u8]) {
    let has = |kind| nal_units(out).any(|unit| nal_type(unit) == kind);
    if has(5) && !has(7) {
        // After the access unit delimiter, which comes first.
        let at = start_codes(out)
            .find(|&(_, unit)| nal_type(&out[unit..]) != 9)
            .map_or(out.len(), |(code, _)| code);
        out.splice(at..at, parameter_sets.iter().copied());
    }
}

fn number(value: u32) -> VARIANT {
    let mut variant = VARIANT::default();
    unsafe {
        let data = &mut variant.Anonymous.Anonymous;
        data.vt = VT_UI4;
        data.Anonymous.ulVal = value;
    }
    variant
}

fn yes() -> VARIANT {
    let mut variant = VARIANT::default();
    unsafe {
        let data = &mut variant.Anonymous.Anonymous;
        data.vt = VT_BOOL;
        data.Anonymous.boolVal = true.into();
    }
    variant
}

impl Encoder {
    pub fn check_available() -> Result<()> {
        create().map(drop)
    }

    pub fn new(size: (usize, usize), settings: Settings) -> Result<Self> {
        let (width, height) = size;
        let transform = create()?;
        let codec: ICodecAPI = transform
            .cast()
            .context("the Windows H.264 encoder has no settings")?;
        let set = |name: &str, api: &GUID, value: VARIANT| {
            if let Err(e) = unsafe { codec.SetValue(api, &value) } {
                tracing::debug!("the Windows H.264 encoder ignored {name}: {e}");
            }
        };
        // Read when the output type is set, so they come first.
        set("low latency", &CODECAPI_AVLowLatencyMode, yes());
        set(
            "constant bitrate",
            &CODECAPI_AVEncCommonRateControlMode,
            number(eAVEncCommonRateControlMode_CBR.0 as u32),
        );
        set(
            "bitrate",
            &CODECAPI_AVEncCommonMeanBitRate,
            number(settings.bitrate_bps),
        );
        set(
            "keyframe interval",
            &CODECAPI_AVEncMPVGOPSize,
            number(settings.fps * KEYFRAME_INTERVAL_S),
        );
        set(
            "display remoting",
            &CODECAPI_AVScenarioInfo,
            number(eAVScenarioInfo_DisplayRemoting.0 as u32),
        );
        unsafe {
            // A picture out for every picture in, as soon as it is encoded.
            if let Ok(attributes) = transform.GetAttributes() {
                let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            let video = |subtype: &GUID| -> Result<_> {
                let media = MFCreateMediaType()?;
                media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                media.SetGUID(&MF_MT_SUBTYPE, subtype)?;
                media.SetUINT64(&MF_MT_FRAME_SIZE, pair(width, height))?;
                media.SetUINT64(&MF_MT_FRAME_RATE, pair(settings.fps as usize, 1))?;
                media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pair(1, 1))?;
                media.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                Ok(media)
            };
            // The output type comes first; the input type must then match it.
            let output = video(&MFVideoFormat_H264)?;
            output.SetUINT32(&MF_MT_AVG_BITRATE, settings.bitrate_bps)?;
            // Constrained baseline, which every H.264 decoder takes; older
            // encoders only name plain baseline.
            output.SetUINT32(
                &MF_MT_MPEG2_PROFILE,
                eAVEncH264VProfile_ConstrainedBase.0 as u32,
            )?;
            if transform.SetOutputType(0, &output, 0).is_err() {
                output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)?;
                transform.SetOutputType(0, &output, 0).with_context(|| {
                    format!("the Windows H.264 encoder refused {width}x{height}")
                })?;
            }
            transform
                .SetInputType(0, &video(&MFVideoFormat_NV12)?, 0)
                .context("the Windows H.264 encoder refused NV12 input")?;
        }
        let parameter_sets = unsafe {
            let current = transform.GetOutputCurrentType(0)?;
            let mut blob = vec![
                0;
                current
                    .GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER)
                    .unwrap_or(0) as usize
            ];
            if current
                .GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None)
                .is_err()
            {
                blob.clear();
            }
            blob
        };
        let info = unsafe { transform.GetOutputStreamInfo(0) }?;
        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(Self {
            transform,
            codec,
            size,
            yuv: YUVBuffer::new(width, height),
            parameter_sets,
            output: None,
            output_size: info.cbSize.max((width * height * 3 / 2) as u32),
            provides_samples: info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
            frame_duration: 10_000_000 / i64::from(settings.fps.max(1)),
        })
    }

    pub fn size(&self) -> (usize, usize) {
        self.size
    }

    /// Encodes one BGRA picture of this encoder's size into `out`.
    pub fn encode(
        &mut self,
        bgra: &[u8],
        timestamp_ms: u64,
        keyframe: bool,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let sample = self.input_sample(bgra)?;
        unsafe {
            sample.SetSampleTime(timestamp_ms as i64 * 10_000)?;
            sample.SetSampleDuration(self.frame_duration)?;
        }
        if keyframe {
            unsafe {
                self.codec
                    .SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &number(1))
            }
            .context("the Windows H.264 encoder cannot start a keyframe")?;
        }
        if let Err(e) = unsafe { self.transform.ProcessInput(0, &sample, 0) } {
            if e.code() != MF_E_NOTACCEPTING {
                return Err(e).context("the Windows H.264 encoder refused the picture");
            }
            // Output from earlier input is still waiting: take it first.
            self.drain(out)?;
            unsafe { self.transform.ProcessInput(0, &sample, 0) }
                .context("the Windows H.264 encoder refused the picture")?;
        }
        self.drain(out)?;
        with_parameter_sets(out, &self.parameter_sets);
        Ok(())
    }

    fn input_sample(&mut self, bgra: &[u8]) -> Result<IMFSample> {
        let (width, height) = self.size;
        self.yuv.read_bgra8(BgraSliceU8::new(bgra, self.size));
        let length = width * height * 3 / 2;
        unsafe {
            // A fresh buffer each time: the encoder may still hold the last one.
            let buffer = MFCreateMemoryBuffer(length as u32)?;
            let mut target = std::ptr::null_mut();
            buffer.Lock(&mut target, None, None)?;
            let nv12 = std::slice::from_raw_parts_mut(target, length);
            let (luma, chroma) = nv12.split_at_mut(width * height);
            luma.copy_from_slice(self.yuv.y());
            let pairs = chroma.as_chunks_mut::<2>().0;
            for (pair, (u, v)) in pairs.iter_mut().zip(self.yuv.u().iter().zip(self.yuv.v())) {
                *pair = [*u, *v];
            }
            buffer.Unlock()?;
            buffer.SetCurrentLength(length as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            Ok(sample)
        }
    }

    /// Appends everything the encoder has ready to `out`.
    fn drain(&mut self, out: &mut Vec<u8>) -> Result<()> {
        let mut changes = 0;
        loop {
            match self.take_output(out)? {
                Output::Data => {}
                Output::NeedMoreInput => return Ok(()),
                // The encoder settles its output type; take what it offers.
                Output::StreamChanged if changes < 3 => {
                    changes += 1;
                    unsafe {
                        let offered = self.transform.GetOutputAvailableType(0, 0)?;
                        self.transform.SetOutputType(0, &offered, 0)?;
                    }
                }
                Output::StreamChanged => {
                    bail!("the Windows H.264 encoder keeps changing its output")
                }
            }
        }
    }

    fn take_output(&mut self, out: &mut Vec<u8>) -> Result<Output> {
        let ours = if self.provides_samples {
            None
        } else {
            let (sample, buffer) = match &self.output {
                Some(output) => output.clone(),
                None => {
                    let buffer = unsafe { MFCreateMemoryBuffer(self.output_size) }?;
                    let sample = unsafe { MFCreateSample() }?;
                    unsafe { sample.AddBuffer(&buffer) }?;
                    self.output.insert((sample, buffer)).clone()
                }
            };
            // The encoder writes after the buffer's current contents.
            unsafe { buffer.SetCurrentLength(0) }?;
            Some(sample)
        };
        let mut outputs = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(ours),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0;
        let result = unsafe { self.transform.ProcessOutput(0, &mut outputs, &mut status) };
        // Take back the references the call held or handed over.
        let sample = unsafe { ManuallyDrop::take(&mut outputs[0].pSample) };
        unsafe { ManuallyDrop::drop(&mut outputs[0].pEvents) };
        match result {
            Ok(()) => {
                let sample = sample.context("the Windows H.264 encoder returned no data")?;
                let buffer = unsafe { sample.ConvertToContiguousBuffer() }?;
                let mut data = std::ptr::null_mut();
                let mut length = 0;
                unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }?;
                out.extend_from_slice(unsafe { std::slice::from_raw_parts(data, length as usize) });
                unsafe { buffer.Unlock() }?;
                Ok(Output::Data)
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Output::NeedMoreInput),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => Ok(Output::StreamChanged),
            Err(e) => Err(e).context("the Windows H.264 encoder failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframes_get_parameter_sets_once() {
        let sets = [0, 0, 0, 1, 0x67, 66, 0xC0, 0, 0, 1, 0x68, 1];
        let mut idr = vec![0, 0, 1, 0x09, 0x10, 0, 0, 1, 0x65, 9];
        with_parameter_sets(&mut idr, &sets);
        assert_eq!(
            nal_units(&idr).map(nal_type).collect::<Vec<_>>(),
            [9, 7, 8, 5]
        );
        let complete = idr.clone();
        with_parameter_sets(&mut idr, &sets);
        assert_eq!(idr, complete);
        let mut predicted = vec![0, 0, 1, 0x41, 7];
        with_parameter_sets(&mut predicted, &sets);
        assert_eq!(predicted, [0, 0, 1, 0x41, 7]);
    }

    #[test]
    fn the_sequence_header_is_sps_and_pps() {
        let settings = Settings {
            fps: 30,
            bitrate_bps: 4_000_000,
            motion: false,
        };
        let Some(encoder) = crate::or_skip("encoder", Encoder::new((128, 96), settings)) else {
            return;
        };
        let units: Vec<u8> = nal_units(&encoder.parameter_sets).map(nal_type).collect();
        assert_eq!(units, [7, 8]);
    }
}
