//! Windows capture through the DXGI Desktop Duplication API.
//!
//! The compositor hands us the desktop as a GPU texture and only when something
//! changed; we copy it to a CPU-readable staging texture once per new frame.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

use super::{Capturer, DisplayInfo, DisplayRect, Frame};

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
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    rect: DisplayRect,
    frame: Frame,
}

impl DxgiCapturer {
    pub fn new(display: usize) -> Result<Self> {
        let all = outputs()?;
        let count = all.len();
        let Some((adapter, output)) = all.into_iter().nth(display) else {
            bail!("display {display} not found ({count} attached)");
        };

        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .context("creating Direct3D 11 device")?;

        let output: IDXGIOutput1 = output.cast().context("DXGI 1.2 output required")?;
        let rect = rect_of(unsafe { output.GetDesc() }?.DesktopCoordinates);
        let mut me = Self {
            device: device.context("no D3D11 device")?,
            context: context.context("no D3D11 context")?,
            output,
            duplication: None,
            staging: None,
            frame: Frame {
                width: (rect.width as usize) & !1,
                height: (rect.height as usize) & !1,
                bgra: Vec::new(),
            },
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

    fn copy_to_cpu(&mut self, texture: &ID3D11Texture2D) -> Result<()> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };

        if !matches!(&self.staging, Some((_, w, h)) if *w == desc.Width && *h == desc.Height) {
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: desc.Width,
                Height: desc.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut tex = None;
            unsafe {
                self.device
                    .CreateTexture2D(&staging_desc, None, Some(&mut tex))
            }?;
            self.staging = Some((tex.context("no staging texture")?, desc.Width, desc.Height));
        }
        let (staging, _, _) = self.staging.as_ref().unwrap();

        unsafe { self.context.CopyResource(staging, texture) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }?;

        let width = (desc.Width as usize) & !1;
        let height = (desc.Height as usize) & !1;
        let stride = mapped.RowPitch as usize;
        let row_bytes = width * 4;
        self.frame.width = width;
        self.frame.height = height;
        self.frame.bgra.resize(width * height * 4, 0);
        // SAFETY: the mapping covers `desc.Height` rows of `RowPitch` bytes each.
        let src = unsafe {
            std::slice::from_raw_parts(mapped.pData as *const u8, stride * desc.Height as usize)
        };
        for (dst_row, src_row) in self
            .frame
            .bgra
            .chunks_exact_mut(row_bytes)
            .zip(src.chunks_exact(stride))
        {
            dst_row.copy_from_slice(&src_row[..row_bytes]);
        }
        unsafe { self.context.Unmap(staging, 0) };
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
            self.copy_to_cpu(&texture)?;
            Ok(true)
        })();
        let _ = unsafe { dup.ReleaseFrame() };
        result
    }

    fn frame(&self) -> &Frame {
        &self.frame
    }

    fn rect(&self) -> DisplayRect {
        self.rect
    }
}
