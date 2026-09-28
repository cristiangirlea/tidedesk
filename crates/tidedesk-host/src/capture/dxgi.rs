//! Windows capture through the DXGI Desktop Duplication API.
//!
//! The compositor hands us the desktop as a GPU texture and only when something
//! changed; we copy it into a texture of our own on the same card, where the
//! graphics card's encoder takes it. Encoders that need the pixels in system
//! memory have them copied back there by the codec.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_FLAG, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

use tidedesk_codec::Image;

use super::{Capturer, DisplayInfo, DisplayRect};

fn rect_of(r: RECT) -> DisplayRect {
    DisplayRect {
        left: r.left,
        top: r.top,
        width: r.right - r.left,
        height: r.bottom - r.top,
    }
}

/// All attached outputs across all adapters, in a stable order.
fn outputs() -> Result<Vec<(IDXGIAdapter1, IDXGIOutput)>> {
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.context("creating DXGI factory")?;
    let mut found = Vec::new();
    for a in 0.. {
        let adapter = match unsafe { factory.EnumAdapters1(a) } {
            Ok(x) => x,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(e.into()),
        };
        for o in 0.. {
            match unsafe { adapter.EnumOutputs(o) } {
                Ok(output) => {
                    if unsafe { output.GetDesc() }.is_ok_and(|d| d.AttachedToDesktop.as_bool()) {
                        found.push((adapter.clone(), output));
                    }
                }
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(found)
}

pub fn list_displays() -> Result<Vec<DisplayInfo>> {
    outputs()?
        .iter()
        .enumerate()
        .map(|(index, (_, output))| {
            let d = unsafe { output.GetDesc() }?;
            let len = d
                .DeviceName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(d.DeviceName.len());
            let rect = rect_of(d.DesktopCoordinates);
            Ok(DisplayInfo {
                index,
                name: String::from_utf16_lossy(&d.DeviceName[..len]),
                primary: rect.left == 0 && rect.top == 0,
                rect,
            })
        })
        .collect()
}

pub struct DxgiCapturer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    output: IDXGIOutput1,
    duplication: Option<IDXGIOutputDuplication>,
    /// The latest picture, kept on the card.
    latest: Option<ID3D11Texture2D>,
    /// Its size, cropped to even dimensions.
    size: (usize, usize),
    rect: DisplayRect,
}

impl DxgiCapturer {
    pub fn new(display: usize) -> Result<Self> {
        let all = outputs()?;
        let count = all.len();
        let Some((adapter, output)) = all.into_iter().nth(display) else {
            bail!("display {display} not found ({count} attached)");
        };

        let create = |flags: D3D11_CREATE_DEVICE_FLAG| {
            let (mut device, mut context) = (None, None);
            unsafe {
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    flags,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            }
            .map(|()| (device, context))
        };
        // Video support lets the graphics card's encoder share the device and
        // take the picture where it is; a card without it still captures.
        let video = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
        let (device, context) = create(video)
            .or_else(|_| create(D3D11_CREATE_DEVICE_BGRA_SUPPORT))
            .context("creating Direct3D 11 device")?;

        let output: IDXGIOutput1 = output.cast().context("DXGI 1.2 output required")?;
        let rect = rect_of(unsafe { output.GetDesc() }?.DesktopCoordinates);
        let mut me = Self {
            device: device.context("no D3D11 device")?,
            context: context.context("no D3D11 context")?,
            output,
            duplication: None,
            latest: None,
            size: ((rect.width as usize) & !1, (rect.height as usize) & !1),
            rect,
        };
        me.duplicate().context(
            "starting desktop duplication (is another app already capturing, or is this a remote/virtual session?)",
        )?;
        Ok(me)
    }

    fn duplicate(&mut self) -> Result<()> {
        let dup = unsafe { self.output.DuplicateOutput(&self.device) }?;
        self.rect = rect_of(unsafe { self.output.GetDesc() }?.DesktopCoordinates);
        self.duplication = Some(dup);
        Ok(())
    }

    /// Copies the desktop's texture into our own, on the card: the desktop's
    /// must be handed back before the next frame.
    fn keep(&mut self, texture: &ID3D11Texture2D) -> Result<()> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        let fits = self.latest.as_ref().is_some_and(|latest| {
            let mut kept = D3D11_TEXTURE2D_DESC::default();
            unsafe { latest.GetDesc(&mut kept) };
            (kept.Width, kept.Height, kept.Format) == (desc.Width, desc.Height, desc.Format)
        });
        if !fits {
            let own = D3D11_TEXTURE2D_DESC {
                MipLevels: 1,
                ArraySize: 1,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
                ..desc
            };
            let mut latest = None;
            unsafe { self.device.CreateTexture2D(&own, None, Some(&mut latest)) }?;
            self.latest = Some(latest.context("no texture for the screen")?);
        }
        let latest = self.latest.as_ref().expect("made above");
        unsafe { self.context.CopyResource(latest, texture) };
        self.size = ((desc.Width as usize) & !1, (desc.Height as usize) & !1);
        Ok(())
    }
}

impl Capturer for DxgiCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<bool> {
        let Some(dup) = self.duplication.clone() else {
            // Duplication is lost across mode changes and while the secure
            // desktop (UAC, lock screen) is up; keep retrying quietly.
            if self.duplicate().is_err() {
                std::thread::sleep(timeout.max(Duration::from_millis(50)));
            }
            return Ok(false);
        };

        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        match unsafe { dup.AcquireNextFrame(timeout_ms, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                self.duplication = None;
                return Ok(false);
            }
            Err(e) => return Err(e).context("AcquireNextFrame"),
        }

        // Every successful acquire must be paired with ReleaseFrame.
        let result = (|| -> Result<bool> {
            // Zero present time means only the pointer moved.
            if info.LastPresentTime == 0 {
                return Ok(false);
            }
            let texture: ID3D11Texture2D = resource.context("no desktop resource")?.cast()?;
            self.keep(&texture)?;
            Ok(true)
        })();
        let _ = unsafe { dup.ReleaseFrame() };
        result
    }

    fn image(&self) -> Image<'_> {
        match &self.latest {
            Some(latest) => Image::Texture(latest),
            None => Image::Bgra(&[]),
        }
    }

    fn size(&self) -> (usize, usize) {
        self.size
    }

    fn rect(&self) -> DisplayRect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use windows::Win32::Graphics::Direct3D11::ID3D11VideoDevice;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

    use super::*;

    /// The screen, captured, stays on the card on a device the graphics
    /// card's encoder can share. Skipped where there is no desktop to capture
    /// (CI), unless `TIDEDESK_REQUIRE_HW` is set.
    #[test]
    fn the_screen_stays_on_the_card() {
        let mut capturer = match DxgiCapturer::new(0) {
            Ok(capturer) => capturer,
            Err(e) if std::env::var_os("TIDEDESK_REQUIRE_HW").is_none() => {
                eprintln!("skipping: no desktop to capture ({e:#})");
                return;
            }
            Err(e) => panic!("TIDEDESK_REQUIRE_HW is set, but {e:#}"),
        };
        // The first frame is the whole screen.
        let deadline = Instant::now() + Duration::from_secs(3);
        while !capturer.next_frame(Duration::from_millis(100)).unwrap() {
            assert!(Instant::now() < deadline, "no frame from the screen");
        }
        let (width, height) = capturer.size();
        assert!(width > 0 && height > 0 && width % 2 == 0 && height % 2 == 0);
        let Image::Texture(texture) = capturer.image() else {
            panic!("the picture left the card");
        };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        assert_eq!(desc.Format, DXGI_FORMAT_B8G8R8A8_UNORM);
        assert!(desc.Width as usize >= width && desc.Height as usize >= height);
        let device = unsafe { texture.GetDevice() }.unwrap();
        assert!(
            device.cast::<ID3D11VideoDevice>().is_ok(),
            "no video support"
        );
    }
}
