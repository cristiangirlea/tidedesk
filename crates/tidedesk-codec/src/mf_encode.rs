//! Windows' H.264 encoders, used through Media Foundation's transform
//! interface: Windows' own software encoder, synchronously and from system
//! memory, and the graphics card's encoder, asynchronously and from Direct3D
//! 11 textures.

use std::mem::ManuallyDrop;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device, ID3D11Multithread,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIAdapter1, IDXGIFactory1,
};
use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264EncoderMFT, CODECAPI_AVEncCommonMaxBitRate, CODECAPI_AVEncCommonMeanBitRate,
    CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
    CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode,
    CODECAPI_AVScenarioInfo, ICodecAPI, IMF2DBuffer, IMFActivate, IMFDXGIDeviceManager,
    IMFMediaBuffer, IMFMediaEventGenerator, IMFSample, IMFTransform,
    MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MEError, METransformHaveOutput, METransformNeedInput,
    MF_E_INVALIDMEDIATYPE, MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG_SEQUENCE_HEADER,
    MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC_UNLOCK,
    MFCreateAttributes, MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFT_CATEGORY_VIDEO_ENCODER,
    MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_ENUM_HARDWARE_VENDOR_ID_Attribute, MFT_FRIENDLY_NAME_Attribute,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MFTEnum2, MFVideoFormat_H264, MFVideoFormat_NV12,
    MFVideoInterlace_Progressive, eAVEncCommonRateControlMode_CBR,
    eAVEncCommonRateControlMode_PeakConstrainedVBR, eAVEncH264VProfile_ConstrainedBase,
    eAVScenarioInfo_DisplayRemoting,
};
use windows::Win32::System::Com::{
    APTTYPE, APTTYPE_MTA, APTTYPEQUALIFIER, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    CoCreateInstance, CoGetApartmentType, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_UI4};
use windows::core::{GUID, HRESULT, Interface};

use openh264::formats::{BgraSliceU8, YUVBuffer, YUVSource};

use crate::encode::{Settings, is_keyframe, nal_type, nal_units, start_codes};

/// Seconds between the keyframes the encoder adds on its own. The host asks
/// for one whenever a viewer needs it, so these are only a backstop.
const KEYFRAME_INTERVAL_S: u32 = 3600;

/// How long the graphics card's encoder may take to ask for a picture or to
/// hand one back before it counts as failed.
const HARDWARE_TIMEOUT: Duration = Duration::from_secs(1);

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
    /// Set for the graphics card's encoder.
    hardware: Option<Hardware>,
}

/// What the graphics card's encoder needs besides the transform. It works
/// asynchronously: it asks for pictures and announces output through events.
/// It takes its pictures as Direct3D 11 textures; from system memory, even
/// with a device, each picture takes a timer tick (about 16 ms) longer.
struct Hardware {
    activate: IMFActivate,
    /// The encoder's events (type and status), read on their own thread.
    events: Receiver<Event>,
    reader: JoinHandle<()>,
    device: ID3D11Device,
    /// Hands the device to the encoder; kept alive with it.
    _manager: IMFDXGIDeviceManager,
    /// Pictures the encoder asked for, and outputs it announced, not yet
    /// served.
    wanted: u32,
    announced: u32,
    /// The picture in NV12, uploaded into a texture.
    nv12: Vec<u8>,
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if let Some(hardware) = &self.hardware {
            // An asynchronous transform runs its own threads until shut down,
            // and shutting it down ends the one reading its events.
            let _ = unsafe { hardware.activate.ShutdownObject() };
            let deadline = Instant::now() + Duration::from_millis(200);
            while !hardware.reader.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            if !hardware.reader.is_finished() {
                tracing::warn!("the hardware H.264 encoder's events did not stop");
            }
        }
    }
}

/// An event from the encoder: its type and status.
type Event = windows::core::Result<(u32, HRESULT)>;

enum Output {
    Data,
    NeedMoreInput,
    StreamChanged,
}

/// Media Foundation needs COM on this thread; one already set up is fine.
fn start() -> Result<()> {
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    crate::mf::startup()
}

fn create() -> Result<IMFTransform> {
    start()?;
    unsafe { CoCreateInstance(&CLSID_MSH264EncoderMFT, None, CLSCTX_INPROC_SERVER) }
        .context("Windows has no H.264 encoder (Windows N editions need the Media Feature Pack)")
}

/// The graphics cards' H.264 encoders, best first; with `card`, those Windows
/// lists for that card (vendors that do not say list theirs for every card).
fn hardware_encoders(card: Option<LUID>) -> Result<Vec<IMFActivate>> {
    start()?;
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
    let mut filter = None;
    if let Some(luid) = card {
        unsafe { MFCreateAttributes(&mut filter, 1) }?;
        let bytes = unsafe {
            std::slice::from_raw_parts((&raw const luid).cast::<u8>(), size_of::<LUID>())
        };
        let attributes = filter.as_ref().context("no attributes")?;
        unsafe { attributes.SetBlob(&MFT_ENUM_ADAPTER_LUID, bytes) }?;
    }
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0;
    unsafe {
        MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            Some(&input),
            Some(&output),
            filter.as_ref(),
            &mut list,
            &mut count,
        )
    }?;
    let found = (0..count as usize)
        .filter_map(|i| unsafe { (*list.add(i)).take() })
        .collect();
    unsafe { CoTaskMemFree(Some(list.cast_const().cast())) };
    Ok(found)
}

/// The graphics cards Windows lists: what choosing needs, the card itself,
/// and its name.
fn cards() -> Result<Vec<(Card, IDXGIAdapter1, String)>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
    let mut cards = Vec::new();
    for index in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        let description = unsafe { adapter.GetDesc1() }?;
        let screens = (0..)
            .take_while(|&output| unsafe { adapter.EnumOutputs(output) }.is_ok())
            .count();
        let card = Card {
            vendor: description.VendorId,
            screens,
            software: description.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0,
        };
        let name = String::from_utf16_lossy(&description.Description);
        cards.push((card, adapter, name.trim_end_matches('\0').to_string()));
    }
    Ok(cards)
}

/// Reads the encoder's events on a thread of their own until it shuts down.
/// Waiting on them there costs the encoder nothing; asking for them over and
/// over without waiting slows it down, from 3.5 ms a picture to 20 or more.
fn read_events(events: IMFMediaEventGenerator) -> Result<(Receiver<Event>, JoinHandle<()>)> {
    struct Events(IMFMediaEventGenerator);
    // SAFETY: the encoder lives in the multithreaded apartment (see `start`),
    // whose objects any of its threads may use; the reader joins it.
    unsafe impl Send for Events {}
    let events = Events(events);
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("h264 encoder events".into())
        .spawn(move || {
            let events = events;
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            loop {
                let event = unsafe { events.0.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0)) }
                    .and_then(|event| unsafe { Ok((event.GetType()?, event.GetStatus()?)) });
                // Ends once the encoder shuts down or nobody listens.
                let failed = event.is_err();
                if sender.send(event).is_err() || failed {
                    break;
                }
            }
        })?;
    Ok((receiver, reader))
}

/// Whether this thread is in COM's multithreaded apartment, whose objects
/// other threads of it may use: the hardware encoder's events are read on one.
fn multithreaded() -> bool {
    let (mut apartment, mut qualifier) = (APTTYPE::default(), APTTYPEQUALIFIER::default());
    unsafe { CoGetApartmentType(&mut apartment, &mut qualifier) }.is_ok()
        && apartment == APTTYPE_MTA
}

/// A graphics card, as far as choosing an encoder goes.
#[derive(Debug, Clone, PartialEq)]
struct Card {
    /// PCI vendor ID: 0x1002 AMD, 0x10DE NVIDIA, 0x8086 Intel.
    vendor: u32,
    /// Screens it shows; the captured screen is on one of these cards.
    screens: usize,
    /// A software renderer, which has no encoder.
    software: bool,
}

/// The order to try graphics cards in: those showing a screen first, as the
/// picture comes from there (and a laptop's integrated graphics, which
/// usually shows it, spares the battery), then the others; never software.
fn in_order(cards: &[Card]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..cards.len()).filter(|&i| !cards[i].software).collect();
    // A stable sort keeps Windows' order within each group.
    order.sort_by_key(|&i| cards[i].screens == 0);
    order
}

/// The PCI vendor ID in an encoder's vendor attribute, "VEN_10DE".
fn vendor_id(attribute: &str) -> Option<u32> {
    let prefix = attribute.get(..4)?;
    if !prefix.eq_ignore_ascii_case("VEN_") {
        return None;
    }
    let digits = attribute[4..]
        .split(|c: char| !c.is_ascii_hexdigit())
        .next()?;
    u32::from_str_radix(digits, 16).ok()
}

/// Microsoft's PCI vendor ID, as its Direct3D 12 encoder wrapper names it:
/// that encoder runs on whichever card's device it is given.
const MICROSOFT: u32 = 0x1414;

/// Whether an encoder from `encoder` (as it names itself, if it does) may be
/// tried on a card from `card`.
fn fits(encoder: Option<u32>, card: u32) -> bool {
    encoder.is_none_or(|vendor| vendor == card || vendor == MICROSOFT)
}

/// An encoder turning down a card's Direct3D device: it belongs to another
/// card, which retrying will not change.
#[derive(Debug)]
struct OtherCard;

impl std::fmt::Display for OtherCard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the encoder does not take this card's Direct3D 11 device")
    }
}

/// Whether `error` will not pass: the encoder refused the picture's size or
/// the card, rather than, say, all its sessions being in use or the device
/// being lost.
fn lasting(error: &anyhow::Error) -> bool {
    error.downcast_ref::<OtherCard>().is_some()
        || error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<windows::core::Error>())
            .any(|cause| cause.code() == MF_E_INVALIDMEDIATYPE)
}

/// The vendor an encoder says it is from, if it says.
fn encoder_vendor(activate: &IMFActivate) -> Option<u32> {
    let mut text = [0u16; 64];
    let mut length = 0;
    let attribute = &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute;
    unsafe { activate.GetString(attribute, &mut text, Some(&mut length)) }.ok()?;
    vendor_id(&String::from_utf16_lossy(&text[..length as usize]))
}

/// Sizes every graphics card's encoder turned down for good (see
/// [`lasting`]), not to be tried again in this process: a new encoder is made
/// for every Game Boost change, and trying every card takes a while.
fn refused(size: (usize, usize)) -> bool {
    REFUSED.lock().is_ok_and(|sizes| sizes.contains(&size))
}

fn refuse(size: (usize, usize)) {
    if let Ok(mut sizes) = REFUSED.lock()
        && !sizes.contains(&size)
    {
        sizes.push(size);
    }
}

static REFUSED: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

fn friendly_name(activate: &IMFActivate) -> String {
    let mut name = [0u16; 128];
    let mut length = 0;
    match unsafe { activate.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut name, Some(&mut length)) }
    {
        Ok(()) => String::from_utf16_lossy(&name[..length as usize]),
        Err(_) => "an unnamed hardware encoder".into(),
    }
}

/// A Direct3D 11 device on `card`, and the manager that hands it to a Media
/// Foundation transform.
fn direct3d(card: &IDXGIAdapter1) -> Result<(ID3D11Device, IMFDXGIDeviceManager)> {
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            card,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }
    .context("no Direct3D 11 device for video")?;
    let device = device.context("no Direct3D 11 device for video")?;
    // The encoder uses the device from its own threads.
    let multithread: ID3D11Multithread = device.cast()?;
    let _ = unsafe { multithread.SetMultithreadProtected(true) };
    let mut token = 0;
    let mut manager = None;
    unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) }?;
    let manager = manager.context("no Direct3D device manager")?;
    unsafe { manager.ResetDevice(&device, token) }?;
    Ok((device, manager))
}

/// Width and height, or numerator and denominator, as Media Foundation packs them.
fn pair(high: usize, low: usize) -> u64 {
    ((high as u64) << 32) | low as u64
}

/// Puts SPS and PPS in front of a keyframe written without them, as some
/// encoders do, so that a viewer can start decoding there.
fn with_parameter_sets(keyframe: &mut Vec<u8>, parameter_sets: &[u8]) {
    if nal_units(keyframe).any(|unit| nal_type(unit) == 7) {
        return;
    }
    // After the access unit delimiter, which comes first.
    let at = start_codes(keyframe)
        .find(|&(_, unit)| nal_type(&keyframe[unit..]) != 9)
        .map_or(keyframe.len(), |(code, _)| code);
    keyframe.splice(at..at, parameter_sets.iter().copied());
}

/// Writes the I420 picture into `nv12` as NV12: the luma plane, then the
/// chroma planes interleaved.
fn write_nv12(yuv: &YUVBuffer, nv12: &mut [u8]) {
    let (luma, chroma) = nv12.split_at_mut(yuv.y().len());
    luma.copy_from_slice(yuv.y());
    let pairs = chroma.as_chunks_mut::<2>().0;
    for (pair, (u, v)) in pairs.iter_mut().zip(yuv.u().iter().zip(yuv.v())) {
        *pair = [*u, *v];
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

    pub fn check_hardware_available() -> Result<()> {
        if hardware_encoders(None)?.is_empty() {
            bail!("no graphics card here has an H.264 encoder");
        }
        Ok(())
    }

    /// Windows' own encoder, or with `hardware` a graphics card's encoder
    /// that takes this size: cards showing a screen first, and on each card
    /// its own vendor's encoders (and those naming no vendor, or Microsoft's
    /// wrapper). A size every encoder turned down for good is not tried again
    /// in this process.
    pub fn new(size: (usize, usize), settings: Settings, hardware: bool) -> Result<Self> {
        if !hardware {
            return Self::with(create()?, size, settings, None);
        }
        let (width, height) = size;
        if refused(size) {
            bail!("no graphics card took {width}x{height} before");
        }
        start()?;
        if !multithreaded() {
            bail!("the hardware H.264 encoder needs COM's multithreaded apartment");
        }
        let cards = cards()?;
        let listed: Vec<Card> = cards.iter().map(|(card, ..)| card.clone()).collect();
        let (mut tried, mut lasting_only) = (Vec::new(), true);
        for index in in_order(&listed) {
            let (card, adapter, card_name) = &cards[index];
            let luid = unsafe { adapter.GetDesc1() }?.AdapterLuid;
            // Drivers list the same encoder several times; one refusal is
            // theirs all.
            let mut refused_here = Vec::new();
            let mut encoders = hardware_encoders(Some(luid))?;
            if encoders.is_empty() {
                // The card filter may not match how a vendor registers.
                encoders = hardware_encoders(None)?;
            }
            for activate in encoders {
                let name = friendly_name(&activate);
                if !fits(encoder_vendor(&activate), card.vendor) || refused_here.contains(&name) {
                    continue;
                }
                match Self::on_graphics_card(&activate, adapter, size, settings) {
                    Ok(encoder) => {
                        tracing::info!("hardware H.264 encoder: {name} on {card_name}");
                        return Ok(encoder);
                    }
                    Err(e) => {
                        let _ = unsafe { activate.ShutdownObject() };
                        lasting_only &= lasting(&e);
                        tried.push(format!("{name} on {card_name}: {e:#}"));
                        refused_here.push(name);
                    }
                }
            }
        }
        if tried.is_empty() {
            bail!("no graphics card here has an H.264 encoder of its own vendor");
        }
        if lasting_only {
            refuse(size);
        }
        bail!(
            "no graphics card took {width}x{height} ({})",
            tried.join("; ")
        )
    }

    fn on_graphics_card(
        activate: &IMFActivate,
        card: &IDXGIAdapter1,
        size: (usize, usize),
        settings: Settings,
    ) -> Result<Self> {
        let transform: IMFTransform = unsafe { activate.ActivateObject() }?;
        let attributes = unsafe { transform.GetAttributes() }?;
        unsafe { attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }?;
        let (device, manager) = direct3d(card)?;
        unsafe { transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize) }
            .map_err(|e| anyhow::Error::from(e).context(OtherCard))?;
        let (events, reader) = read_events(transform.cast()?)?;
        let hardware = Hardware {
            activate: activate.clone(),
            events,
            reader,
            device,
            _manager: manager,
            wanted: 0,
            announced: 0,
            nv12: Vec::new(),
        };
        Self::with(transform, size, settings, Some(hardware))
    }

    fn with(
        transform: IMFTransform,
        size: (usize, usize),
        settings: Settings,
        hardware: Option<Hardware>,
    ) -> Result<Self> {
        let (width, height) = size;
        let codec: ICodecAPI = transform
            .cast()
            .context("the H.264 encoder has no settings")?;
        let set = |name: &str, api: &GUID, value: VARIANT| {
            if let Err(e) = unsafe { codec.SetValue(api, &value) } {
                tracing::debug!("the H.264 encoder ignored {name}: {e}");
            }
        };
        // Read when the output type is set, so they come first.
        set("low latency", &CODECAPI_AVLowLatencyMode, yes());
        if hardware.is_some() {
            // Graphics cards pad every picture up to a constant bitrate; the
            // peak keeps a still screen almost free and motion within bounds.
            set(
                "peak-constrained bitrate",
                &CODECAPI_AVEncCommonRateControlMode,
                number(eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32),
            );
            set(
                "peak bitrate",
                &CODECAPI_AVEncCommonMaxBitRate,
                number(settings.bitrate_bps),
            );
        } else {
            set(
                "constant bitrate",
                &CODECAPI_AVEncCommonRateControlMode,
                number(eAVEncCommonRateControlMode_CBR.0 as u32),
            );
        }
        set(
            "bitrate",
            &CODECAPI_AVEncCommonMeanBitRate,
            number(settings.bitrate_bps),
        );
        set(
            "no B-frames",
            &CODECAPI_AVEncMPVDefaultBPictureCount,
            number(0),
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
            // Constrained baseline, which every H.264 decoder takes.
            output.SetUINT32(
                &MF_MT_MPEG2_PROFILE,
                eAVEncH264VProfile_ConstrainedBase.0 as u32,
            )?;
            transform
                .SetOutputType(0, &output, 0)
                .with_context(|| format!("the H.264 encoder refused {width}x{height}"))?;
            transform
                .SetInputType(0, &video(&MFVideoFormat_NV12)?, 0)
                .context("the H.264 encoder refused NV12 input")?;
        }
        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        let mut encoder = Self {
            transform,
            codec,
            size,
            yuv: YUVBuffer::new(width, height),
            parameter_sets: Vec::new(),
            output: None,
            output_size: 0,
            provides_samples: false,
            frame_duration: 10_000_000 / i64::from(settings.fps.max(1)),
            hardware,
        };
        encoder.read_output()?;
        Ok(encoder)
    }

    /// Reads how the encoder hands out its data, and its parameter sets: at
    /// the start, and again whenever it changes its output.
    fn read_output(&mut self) -> Result<()> {
        let (width, height) = self.size;
        let info = unsafe { self.transform.GetOutputStreamInfo(0) }?;
        let output_size = info.cbSize.max((width * height * 3 / 2) as u32);
        if output_size != self.output_size {
            self.output_size = output_size;
            self.output = None;
        }
        self.provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
        let current = unsafe { self.transform.GetOutputCurrentType(0) }?;
        let length = unsafe { current.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) }.unwrap_or(0);
        self.parameter_sets.resize(length as usize, 0);
        let read =
            unsafe { current.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut self.parameter_sets, None) };
        if read.is_err() {
            self.parameter_sets.clear();
        }
        Ok(())
    }

    /// Takes the output type the encoder offers after changing its output.
    fn renegotiate(&mut self) -> Result<()> {
        unsafe {
            let offered = self.transform.GetOutputAvailableType(0, 0)?;
            self.transform.SetOutputType(0, &offered, 0)?;
        }
        self.read_output()
    }

    pub fn size(&self) -> (usize, usize) {
        self.size
    }

    /// Encodes one BGRA picture of this encoder's size into `out`; whether
    /// it is a keyframe.
    pub fn encode(
        &mut self,
        bgra: &[u8],
        timestamp_ms: u64,
        keyframe: bool,
        out: &mut Vec<u8>,
    ) -> Result<bool> {
        self.yuv.read_bgra8(BgraSliceU8::new(bgra, self.size));
        let sample = match &mut self.hardware {
            Some(hardware) => hardware.texture_sample(&self.yuv, self.size)?,
            None => self.memory_sample()?,
        };
        unsafe {
            sample.SetSampleTime(timestamp_ms as i64 * 10_000)?;
            sample.SetSampleDuration(self.frame_duration)?;
        }
        if keyframe {
            unsafe {
                self.codec
                    .SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &number(1))
            }
            .context("the H.264 encoder cannot start a keyframe")?;
        }
        if self.hardware.is_some() {
            self.encode_on_graphics_card(&sample, out)?;
        } else {
            self.encode_in_software(&sample, out)?;
        }
        let keyframe = is_keyframe(out);
        if keyframe {
            with_parameter_sets(out, &self.parameter_sets);
        }
        Ok(keyframe)
    }

    fn encode_in_software(&mut self, sample: &IMFSample, out: &mut Vec<u8>) -> Result<()> {
        if let Err(e) = unsafe { self.transform.ProcessInput(0, sample, 0) } {
            if e.code() != MF_E_NOTACCEPTING {
                return Err(e).context("the Windows H.264 encoder refused the picture");
            }
            // Output from earlier input is still waiting: take it first.
            self.drain(out)?;
            unsafe { self.transform.ProcessInput(0, sample, 0) }
                .context("the Windows H.264 encoder refused the picture")?;
        }
        self.drain(out)
    }

    /// Hands the picture over once the encoder asks for one, then takes the
    /// encoded picture once it is announced.
    fn encode_on_graphics_card(&mut self, sample: &IMFSample, out: &mut Vec<u8>) -> Result<()> {
        self.wait_for(|hardware| hardware.wanted > 0, "ask for a picture")?;
        self.hardware_mut().wanted -= 1;
        unsafe { self.transform.ProcessInput(0, sample, 0) }
            .context("the hardware H.264 encoder refused the picture")?;
        let mut changes = 0;
        loop {
            self.wait_for(|hardware| hardware.announced > 0, "encode the picture")?;
            self.hardware_mut().announced -= 1;
            match self.take_output(out)? {
                Output::Data => return self.take_announced(out),
                Output::NeedMoreInput => {}
                Output::StreamChanged if changes < 3 => {
                    changes += 1;
                    self.renegotiate()?;
                }
                Output::StreamChanged => {
                    bail!("the hardware H.264 encoder keeps changing its output")
                }
            }
        }
    }

    /// Takes whatever else the encoder has already announced: it belongs to
    /// this picture, as the next one is not in yet.
    fn take_announced(&mut self, out: &mut Vec<u8>) -> Result<()> {
        loop {
            let hardware = self.hardware_mut();
            while let Ok(event) = hardware.events.try_recv() {
                hardware.count(event)?;
            }
            if hardware.announced == 0 {
                return Ok(());
            }
            hardware.announced -= 1;
            if let Output::StreamChanged = self.take_output(out)? {
                self.renegotiate()?;
            }
        }
    }

    fn hardware_mut(&mut self) -> &mut Hardware {
        self.hardware.as_mut().expect("a hardware encoder")
    }

    /// Takes the encoder's events until `done`; an error if that takes
    /// longer than [`HARDWARE_TIMEOUT`] or the encoder reports a failure.
    fn wait_for(&mut self, done: fn(&Hardware) -> bool, what: &str) -> Result<()> {
        let hardware = self.hardware_mut();
        let deadline = Instant::now() + HARDWARE_TIMEOUT;
        while !done(hardware) {
            let left = deadline.saturating_duration_since(Instant::now());
            match hardware.events.recv_timeout(left) {
                Ok(event) => hardware.count(event)?,
                Err(RecvTimeoutError::Timeout) => {
                    bail!("the hardware H.264 encoder did not {what} in time")
                }
                Err(RecvTimeoutError::Disconnected) => bail!("the hardware H.264 encoder stopped"),
            }
        }
        Ok(())
    }

    fn memory_sample(&mut self) -> Result<IMFSample> {
        let (width, height) = self.size;
        let length = width * height * 3 / 2;
        unsafe {
            // A fresh buffer each time: the encoder may still hold the last one.
            let buffer = MFCreateMemoryBuffer(length as u32)?;
            let mut target = std::ptr::null_mut();
            buffer.Lock(&mut target, None, None)?;
            write_nv12(&self.yuv, std::slice::from_raw_parts_mut(target, length));
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
                    self.renegotiate()?;
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
                let sample = sample.context("the H.264 encoder returned no data")?;
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
            Err(e) => Err(e).context("the H.264 encoder failed"),
        }
    }
}

impl Hardware {
    /// Counts a request for a picture or an announced output; an error if the
    /// encoder reports a failure.
    fn count(&mut self, event: Event) -> Result<()> {
        let (kind, status) = event.context("the hardware H.264 encoder failed")?;
        if status.is_err() || kind == MEError.0 as u32 {
            let error = windows::core::Error::from(status);
            bail!("the hardware H.264 encoder failed: {error}");
        }
        if kind == METransformNeedInput.0 as u32 {
            self.wanted += 1;
        } else if kind == METransformHaveOutput.0 as u32 {
            self.announced += 1;
        }
        Ok(())
    }

    /// The picture as a sample holding an NV12 texture on the encoder's device.
    fn texture_sample(
        &mut self,
        yuv: &YUVBuffer,
        (width, height): (usize, usize),
    ) -> Result<IMFSample> {
        self.nv12.resize(width * height * 3 / 2, 0);
        write_nv12(yuv, &mut self.nv12);
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width as u32,
            Height: height as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            // Only the encoder reads it: no binding, which every card allows.
            BindFlags: 0,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let data = D3D11_SUBRESOURCE_DATA {
            pSysMem: self.nv12.as_ptr().cast(),
            SysMemPitch: width as u32,
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        // A new texture each time: the encoder may still be reading the last.
        unsafe {
            self.device
                .CreateTexture2D(&desc, Some(&data), Some(&mut texture))
        }
        .context("no texture for the picture")?;
        let texture: ID3D11Texture2D = texture.context("no texture for the picture")?;
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &texture, 0, false)?;
            let length = buffer.cast::<IMF2DBuffer>()?.GetContiguousLength()?;
            buffer.SetCurrentLength(length)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            Ok(sample)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{E_INVALIDARG, E_OUTOFMEMORY};

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
    }

    #[test]
    fn encoder_vendors_are_read_from_their_attribute() {
        assert_eq!(vendor_id("VEN_10DE"), Some(0x10DE));
        assert_eq!(vendor_id("VEN_8086"), Some(0x8086));
        assert_eq!(vendor_id("ven_1002"), Some(0x1002));
        assert_eq!(vendor_id(""), None);
        assert_eq!(vendor_id("VEN_"), None);
        assert_eq!(vendor_id("NVIDIA"), None);
    }

    #[test]
    fn encoders_are_tried_on_cards_of_their_own_vendor() {
        let (amd, nvidia, intel) = (0x1002, 0x10DE, 0x8086);
        assert!(fits(Some(nvidia), nvidia));
        assert!(!fits(Some(nvidia), intel));
        assert!(!fits(Some(intel), amd));
        // Naming no vendor, or Microsoft's wrapper: any card.
        assert!(fits(None, intel));
        assert!(fits(Some(MICROSOFT), amd));
    }

    #[test]
    fn only_lasting_refusals_are_remembered() {
        let error = |code| anyhow::Error::from(windows::core::Error::from(code));
        assert!(lasting(
            &error(MF_E_INVALIDMEDIATYPE).context("refused 5120x1440")
        ));
        assert!(lasting(&error(E_INVALIDARG).context(OtherCard)));
        // All of an encoder's sessions in use: try again next time.
        assert!(!lasting(&error(E_OUTOFMEMORY).context("refused 1920x1080")));
        assert!(!lasting(&anyhow::anyhow!(
            "did not ask for a picture in time"
        )));
    }

    #[test]
    fn cards_showing_a_screen_come_first() {
        let card = |vendor, screens, software| Card {
            vendor,
            screens,
            software,
        };
        // A hybrid laptop: NVIDIA listed first, Intel showing the screen.
        let laptop = [
            card(0x10DE, 0, false),
            card(0x8086, 1, false),
            card(0x1414, 0, true),
        ];
        assert_eq!(in_order(&laptop), [1, 0]);
        // A desktop with its card showing the screen and integrated graphics
        // unused: listed order kept within each group.
        let desktop = [
            card(0x1002, 1, false),
            card(0x1002, 0, false),
            card(0x1002, 2, false),
        ];
        assert_eq!(in_order(&desktop), [0, 2, 1]);
        assert!(in_order(&[card(0x1414, 0, true)]).is_empty());
    }

    #[test]
    fn sizes_no_card_takes_are_remembered() {
        let settings = Settings {
            fps: 30,
            bitrate_bps: 4_000_000,
            motion: false,
        };
        // Wider than any H.264 hardware encoder goes (4096 columns).
        let size = (5120, 1440);
        if crate::or_skip(
            "hardware H.264 encoder",
            "TIDEDESK_REQUIRE_HW",
            Encoder::check_hardware_available(),
        )
        .is_none()
        {
            return;
        }
        assert!(Encoder::new(size, settings, true).is_err());
        assert!(refused(size));
        let again = Instant::now();
        assert!(Encoder::new(size, settings, true).is_err());
        assert!(
            again.elapsed() < Duration::from_millis(5),
            "{:?}",
            again.elapsed()
        );
    }

    #[test]
    fn the_sequence_header_is_sps_and_pps() {
        let settings = Settings {
            fps: 30,
            bitrate_bps: 4_000_000,
            motion: false,
        };
        let made = Encoder::new((128, 96), settings, false);
        let Some(encoder) = crate::or_skip("Windows H.264 encoder", "TIDEDESK_REQUIRE_MF", made)
        else {
            return;
        };
        let units: Vec<u8> = nal_units(&encoder.parameter_sets).map(nal_type).collect();
        assert_eq!(units, [7, 8]);
    }
}
