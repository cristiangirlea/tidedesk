//! Windows' own H.264 decoder (Media Foundation), used through its transform
//! interface in the synchronous, system-memory mode.

use std::mem::ManuallyDrop;
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, bail};
use openh264::formats::YUVSlices;
use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264DecoderMFT, IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform,
    MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE,
    MF_LOW_LATENCY, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
    MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE, MF_VERSION, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFSTARTUP_NOSOCKET, MFStartup,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFVideoArea, MFVideoFormat_H264, MFVideoFormat_NV12,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};

/// Starts Media Foundation once for the process.
fn startup() -> Result<()> {
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    STARTED
        .get_or_init(|| {
            unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.map_err(|e| e.to_string())
        })
        .clone()
        .map_err(|e| anyhow!("Media Foundation did not start: {e}"))
}

/// Where the picture sits in the decoder's output buffer.
#[derive(Default, Clone, Copy)]
struct Layout {
    stride: usize,
    /// Rows of luma in the buffer, including any padding below the picture.
    coded_height: usize,
    /// The visible picture: top-left corner and size.
    origin: (usize, usize),
    size: (usize, usize),
}

pub struct Decoder {
    transform: IMFTransform,
    layout: Layout,
    /// The sample the decoder writes into, and how large it must be.
    output: Option<(IMFSample, IMFMediaBuffer)>,
    output_size: u32,
    time: i64,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

enum Output {
    Picture,
    NeedMoreInput,
    StreamChanged,
}

impl Decoder {
    pub fn new() -> Result<Self> {
        // Media Foundation needs COM on this thread; one already set up is fine.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        startup()?;
        let transform: IMFTransform =
            unsafe { CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER) }
                .context(
                    "Windows has no H.264 decoder (Windows N editions need the Media Feature Pack)",
                )?;
        unsafe {
            // A picture out for every access unit in, as soon as it is decoded.
            if let Ok(attributes) = transform.GetAttributes() {
                let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            transform
                .SetInputType(0, &input, 0)
                .context("the Windows H.264 decoder refused H.264 input")?;
        }
        let mut decoder = Self {
            transform,
            layout: Layout::default(),
            output: None,
            output_size: 0,
            time: 0,
            y: Vec::new(),
            u: Vec::new(),
            v: Vec::new(),
        };
        decoder.choose_output()?;
        unsafe {
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(decoder)
    }

    /// Decodes one access unit; the picture, if one is ready.
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<YUVSlices<'_>>> {
        if data.is_empty() {
            return Ok(None);
        }
        let sample = input_sample(data, self.time)?;
        // Any increasing time will do: pictures come out in decoding order.
        self.time += 1;
        let mut picture = false;
        if let Err(e) = unsafe { self.transform.ProcessInput(0, &sample, 0) } {
            if e.code() != MF_E_NOTACCEPTING {
                return Err(e).context("the Windows H.264 decoder refused the data");
            }
            // A picture from earlier input is still waiting: take it first.
            picture = self.drain()?;
            unsafe { self.transform.ProcessInput(0, &sample, 0) }
                .context("the Windows H.264 decoder refused the data")?;
        }
        picture |= self.drain()?;
        Ok(picture.then(|| {
            let (width, height) = self.layout.size;
            let chroma = width.div_ceil(2);
            YUVSlices::new(
                (&self.y, &self.u, &self.v),
                (width, height),
                (width, chroma, chroma),
            )
        }))
    }

    /// Takes every picture the decoder has ready, keeping the last; whether
    /// there was one.
    fn drain(&mut self) -> Result<bool> {
        let (mut picture, mut changes) = (false, 0);
        loop {
            match self.take_output()? {
                Output::Picture => picture = true,
                Output::NeedMoreInput => return Ok(picture),
                // One change per new size is normal; more means the decoder
                // does not take the output it asked for.
                Output::StreamChanged if changes < 3 => {
                    changes += 1;
                    self.choose_output()?;
                }
                Output::StreamChanged => {
                    bail!("the Windows H.264 decoder keeps changing its output")
                }
            }
        }
    }

    fn take_output(&mut self) -> Result<Output> {
        let (sample, buffer) = match &self.output {
            Some(output) => output.clone(),
            None => {
                let buffer = unsafe { MFCreateMemoryBuffer(self.output_size) }?;
                let sample = unsafe { MFCreateSample() }?;
                unsafe { sample.AddBuffer(&buffer) }?;
                self.output = Some((sample.clone(), buffer.clone()));
                (sample, buffer)
            }
        };
        let mut outputs = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(Some(sample)),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0;
        // The decoder writes after the buffer's current contents: empty it,
        // or the reused buffer overflows and the picture lands past the start.
        unsafe { buffer.SetCurrentLength(0) }?;
        let result = unsafe { self.transform.ProcessOutput(0, &mut outputs, &mut status) };
        // Release the references the call held or handed back.
        unsafe {
            ManuallyDrop::drop(&mut outputs[0].pSample);
            ManuallyDrop::drop(&mut outputs[0].pEvents);
        }
        match result {
            Ok(()) => {
                self.copy_planes(&buffer)?;
                Ok(Output::Picture)
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Output::NeedMoreInput),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => Ok(Output::StreamChanged),
            Err(e) => Err(e).context("the Windows H.264 decoder failed"),
        }
    }

    /// Chooses NV12 output and reads the picture layout it describes. Called
    /// at the start and whenever the stream's size changes.
    fn choose_output(&mut self) -> Result<()> {
        let chosen = (0..)
            .map_while(|index| unsafe { self.transform.GetOutputAvailableType(0, index) }.ok())
            .find(|t| unsafe { t.GetGUID(&MF_MT_SUBTYPE) }.is_ok_and(|s| s == MFVideoFormat_NV12))
            .context("the Windows H.264 decoder offers no NV12 output")?;
        unsafe { self.transform.SetOutputType(0, &chosen, 0) }?;
        self.layout = layout(&chosen);
        let size = unsafe { self.transform.GetOutputStreamInfo(0) }?.cbSize;
        if size != self.output_size {
            self.output_size = size;
            self.output = None;
        }
        Ok(())
    }

    /// Copies the visible picture out of the decoder's NV12 buffer as I420.
    fn copy_planes(&mut self, buffer: &IMFMediaBuffer) -> Result<()> {
        let Layout {
            stride,
            coded_height,
            origin: (left, top),
            size: (width, height),
        } = self.layout;
        let mut data = std::ptr::null_mut();
        let mut length = 0;
        unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }?;
        let bytes = unsafe { std::slice::from_raw_parts(data, length as usize) };
        let chroma_base = stride * coded_height;
        let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
        let needed = chroma_base + stride * (top / 2 + chroma_height);
        let fits = length as usize >= needed && stride >= left + width;
        if fits {
            self.y.clear();
            for row in top..top + height {
                self.y
                    .extend_from_slice(&bytes[row * stride + left..][..width]);
            }
            self.u.clear();
            self.v.clear();
            for row in top / 2..top / 2 + chroma_height {
                let pairs = &bytes[chroma_base + row * stride + left..][..chroma_width * 2];
                for pair in pairs.as_chunks::<2>().0 {
                    self.u.push(pair[0]);
                    self.v.push(pair[1]);
                }
            }
        }
        unsafe { buffer.Unlock() }?;
        if !fits {
            bail!(
                "the Windows H.264 decoder returned {length} bytes for a {width}x{height} picture"
            );
        }
        Ok(())
    }
}

/// The picture layout an NV12 output type describes.
fn layout(output: &IMFMediaType) -> Layout {
    let frame = unsafe { output.GetUINT64(&MF_MT_FRAME_SIZE) }.unwrap_or(0);
    let coded = ((frame >> 32) as usize, (frame & 0xFFFF_FFFF) as usize);
    let stride = unsafe { output.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
        .map(|s| (s as i32).unsigned_abs() as usize)
        .unwrap_or(coded.0)
        .max(coded.0);
    let mut area = MFVideoArea::default();
    let aperture = unsafe {
        output.GetBlob(
            &MF_MT_MINIMUM_DISPLAY_APERTURE,
            std::slice::from_raw_parts_mut((&raw mut area).cast::<u8>(), size_of::<MFVideoArea>()),
            None,
        )
    };
    let (origin, size) = match aperture {
        Ok(()) if area.Area.cx > 0 && area.Area.cy > 0 => (
            (
                area.OffsetX.value.max(0) as usize,
                area.OffsetY.value.max(0) as usize,
            ),
            (area.Area.cx as usize, area.Area.cy as usize),
        ),
        _ => ((0, 0), coded),
    };
    Layout {
        stride,
        coded_height: coded.1,
        origin,
        size,
    }
}

fn input_sample(data: &[u8], time: i64) -> Result<IMFSample> {
    unsafe {
        let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
        let mut target = std::ptr::null_mut();
        buffer.Lock(&mut target, None, None)?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), target, data.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(data.len() as u32)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(time)?;
        Ok(sample)
    }
}
