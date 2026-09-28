//! Pictures on the graphics card: BGRA to NV12 there, for the card's encoder,
//! and back to system memory for the encoders that need them there.

use std::mem::ManuallyDrop;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_CPU_ACCESS_READ,
    D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_COLOR_SPACE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    ID3D11VideoContext, ID3D11VideoContext1, ID3D11VideoDevice, ID3D11VideoProcessor,
    ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601,
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::core::Interface;

/// Converts BGRA textures to NV12 on the card that holds them, with the
/// colours every viewer expects (BT.601, limited range).
pub(crate) struct Converter {
    size: (usize, usize),
    device: ID3D11Device,
    device_context: ID3D11DeviceContext,
    video: ID3D11VideoDevice,
    context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    output: ID3D11Texture2D,
    output_view: ID3D11VideoProcessorOutputView,
    /// The last picture's texture and its view: capture reuses its texture.
    input: Option<(ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
    /// The top-left `size` of larger pictures. Video processors are to crop
    /// with the source rectangle, but AMD's scales instead.
    cropped: Option<ID3D11Texture2D>,
}

impl Converter {
    pub(crate) fn new(device: &ID3D11Device, size: (usize, usize)) -> Result<Self> {
        let (width, height) = (size.0 as u32, size.1 as u32);
        let video: ID3D11VideoDevice = device.cast().context("the card has no video processor")?;
        let device_context = unsafe { device.GetImmediateContext() }?;
        let context: ID3D11VideoContext = device_context.cast()?;
        let rate = DXGI_RATIONAL {
            Numerator: 60,
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: width,
            InputHeight: height,
            OutputFrameRate: rate,
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let enumerator = unsafe { video.CreateVideoProcessorEnumerator(&content) }?;
        let processor = unsafe { video.CreateVideoProcessor(&enumerator, 0) }?;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut output = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut output)) }
            .context("the card cannot hold NV12 pictures")?;
        let output = output.context("no NV12 texture")?;
        let view = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut output_view = None;
        unsafe {
            video.CreateVideoProcessorOutputView(
                &output,
                &enumerator,
                &view,
                Some(&mut output_view),
            )
        }
        .context("the card cannot convert into NV12")?;
        let output_view = output_view.context("no NV12 view")?;
        let rect = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        unsafe {
            // Full-range RGB in, BT.601 limited range out: the colours
            // OpenH264's conversion gives and every viewer decodes.
            if let Ok(context) = context.cast::<ID3D11VideoContext1>() {
                context.VideoProcessorSetStreamColorSpace1(
                    &processor,
                    0,
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
                context.VideoProcessorSetOutputColorSpace1(
                    &processor,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601,
                );
            } else {
                // RGB_Range 0 (full); YCbCr_Matrix 0 (BT.601) and
                // Nominal_Range 1 (16-235) in bits 4-5.
                let rgb = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
                let yuv = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 1 << 4 };
                context.VideoProcessorSetStreamColorSpace(&processor, 0, &rgb);
                context.VideoProcessorSetOutputColorSpace(&processor, &yuv);
            }
            context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            // No driver enhancements: the picture as captured.
            context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&rect));
            context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&rect));
            context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&rect));
        }
        Ok(Self {
            size,
            device: device.clone(),
            device_context,
            video,
            context,
            enumerator,
            processor,
            output,
            output_view,
            input: None,
            cropped: None,
        })
    }

    /// The top-left `size` of `bgra` as NV12, in a texture the converter
    /// reuses: done with once the encoder has returned its picture.
    pub(crate) fn convert(&mut self, bgra: &ID3D11Texture2D) -> Result<&ID3D11Texture2D> {
        let desc = bgra_description(bgra)?;
        let (width, height) = (self.size.0 as u32, self.size.1 as u32);
        if desc.Width < width || desc.Height < height {
            bail!(
                "a {width}x{height} picture from a {}x{} texture",
                desc.Width,
                desc.Height
            );
        }
        let bgra = if (desc.Width, desc.Height) == (width, height) {
            bgra.clone()
        } else {
            let cropped = match &self.cropped {
                Some(cropped) => cropped.clone(),
                None => {
                    let cropped_desc = D3D11_TEXTURE2D_DESC {
                        Width: width,
                        Height: height,
                        MipLevels: 1,
                        ArraySize: 1,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0)
                            as u32,
                        CPUAccessFlags: 0,
                        MiscFlags: 0,
                        ..desc
                    };
                    let mut cropped = None;
                    unsafe {
                        self.device
                            .CreateTexture2D(&cropped_desc, None, Some(&mut cropped))
                    }?;
                    let cropped = cropped.context("no texture to crop the picture into")?;
                    self.cropped.insert(cropped).clone()
                }
            };
            let region = D3D11_BOX {
                left: 0,
                top: 0,
                front: 0,
                right: width,
                bottom: height,
                back: 1,
            };
            unsafe {
                self.device_context.CopySubresourceRegion(
                    &cropped,
                    0,
                    0,
                    0,
                    0,
                    bgra,
                    0,
                    Some(&region),
                )
            };
            cropped
        };
        let bgra = &bgra;
        let view = match &self.input {
            Some((texture, view)) if texture.as_raw() == bgra.as_raw() => view.clone(),
            _ => {
                let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    FourCC: 0,
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPIV {
                            MipSlice: 0,
                            ArraySlice: 0,
                        },
                    },
                };
                let mut view = None;
                unsafe {
                    self.video.CreateVideoProcessorInputView(
                        bgra,
                        &self.enumerator,
                        &desc,
                        Some(&mut view),
                    )
                }
                .context("the card cannot read the picture")?;
                let view = view.context("no picture view")?;
                self.input = Some((bgra.clone(), view.clone()));
                view
            }
        };
        let mut streams = [D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(view)),
            ..Default::default()
        }];
        let converted = unsafe {
            self.context
                .VideoProcessorBlt(&self.processor, &self.output_view, 0, &streams)
        };
        unsafe { ManuallyDrop::drop(&mut streams[0].pInputSurface) };
        converted.context("the card could not convert the picture")?;
        Ok(&self.output)
    }
}

/// A texture's description, if it holds BGRA pixels, the only kind read here.
fn bgra_description(texture: &ID3D11Texture2D) -> Result<D3D11_TEXTURE2D_DESC> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };
    if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
        bail!("the picture is not BGRA but DXGI format {}", desc.Format.0);
    }
    Ok(desc)
}

/// Copies textures back into system memory.
#[derive(Default)]
pub(crate) struct Readback {
    /// A texture the CPU can read: on the picture's device (by address) and
    /// of its size.
    staging: Option<(ID3D11Texture2D, usize, (u32, u32))>,
    pixels: Vec<u8>,
}

impl Readback {
    /// The top-left `size` of a BGRA texture, row after row.
    pub(crate) fn read(
        &mut self,
        bgra: &ID3D11Texture2D,
        (width, height): (usize, usize),
    ) -> Result<&[u8]> {
        let device = unsafe { bgra.GetDevice() }?;
        let desc = bgra_description(bgra)?;
        if width > desc.Width as usize || height > desc.Height as usize {
            bail!(
                "a {width}x{height} picture from a {}x{} texture",
                desc.Width,
                desc.Height
            );
        }
        let key = (device.as_raw() as usize, (desc.Width, desc.Height));
        let staging = match &self.staging {
            Some((staging, device, size)) if (*device, *size) == key => staging.clone(),
            _ => {
                let staging_desc = D3D11_TEXTURE2D_DESC {
                    MipLevels: 1,
                    ArraySize: 1,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                    ..desc
                };
                let mut staging = None;
                unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) }?;
                let staging = staging.context("no texture to read the picture back")?;
                self.staging = Some((staging.clone(), key.0, key.1));
                staging
            }
        };
        let context = unsafe { device.GetImmediateContext() }?;
        unsafe { context.CopyResource(&staging, bgra) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }?;
        let pitch = mapped.RowPitch as usize;
        // SAFETY: the mapping covers the texture's rows, `pitch` bytes each.
        let all = unsafe {
            std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * desc.Height as usize)
        };
        self.pixels.clear();
        for row in all.chunks_exact(pitch).take(height) {
            self.pixels.extend_from_slice(&row[..width * 4]);
        }
        unsafe { context.Unmap(&staging, 0) };
        Ok(&self.pixels)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use openh264::formats::{BgraSliceU8, YUVBuffer, YUVSource};
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ,
        D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11CreateDevice,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC,
    };

    /// A Direct3D 11 device on the default graphics card, as capture makes
    /// one, or `None` where there is no card (as on CI).
    pub(crate) fn device() -> Option<ID3D11Device> {
        let made = (|| {
            let mut device = None;
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
                    None,
                )
            }?;
            device.ok_or_else(|| anyhow::anyhow!("no device"))
        })();
        crate::or_skip("Direct3D 11 video device", "TIDEDESK_REQUIRE_HW", made)
    }

    /// A device that can convert pictures on the card, or `None` where there
    /// is none: CI's software renderer makes devices, but without a video
    /// processor.
    pub(crate) fn video_device() -> Option<ID3D11Device> {
        let device = device()?;
        let made = Converter::new(&device, (64, 32)).map(|_| device);
        crate::or_skip("Direct3D 11 video processor", "TIDEDESK_REQUIRE_HW", made)
    }

    fn texture(
        device: &ID3D11Device,
        desc: &D3D11_TEXTURE2D_DESC,
        data: Option<(&[u8], usize)>,
    ) -> ID3D11Texture2D {
        let init = data.map(|(bytes, pitch)| D3D11_SUBRESOURCE_DATA {
            pSysMem: bytes.as_ptr().cast(),
            SysMemPitch: pitch as u32,
            SysMemSlicePitch: 0,
        });
        let mut texture = None;
        unsafe {
            device.CreateTexture2D(
                desc,
                init.as_ref().map(|i| i as *const _),
                Some(&mut texture),
            )
        }
        .unwrap();
        texture.unwrap()
    }

    fn desc(
        format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
        (width, height): (usize, usize),
        staging: bool,
    ) -> D3D11_TEXTURE2D_DESC {
        D3D11_TEXTURE2D_DESC {
            Width: width as u32,
            Height: height as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: if staging {
                D3D11_USAGE_STAGING
            } else {
                D3D11_USAGE_DEFAULT
            },
            BindFlags: if staging {
                0
            } else {
                (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32
            },
            CPUAccessFlags: if staging {
                D3D11_CPU_ACCESS_READ.0 as u32
            } else {
                0
            },
            MiscFlags: 0,
        }
    }

    /// A BGRA picture as a texture, the way desktop duplication hands it over.
    pub(crate) fn upload(
        device: &ID3D11Device,
        bgra: &[u8],
        size: (usize, usize),
    ) -> ID3D11Texture2D {
        texture(
            device,
            &desc(DXGI_FORMAT_B8G8R8A8_UNORM, size, false),
            Some((bgra, size.0 * 4)),
        )
    }

    /// An NV12 texture's planes, read back.
    fn nv12(
        device: &ID3D11Device,
        source: &ID3D11Texture2D,
        (width, height): (usize, usize),
    ) -> (Vec<u8>, Vec<u8>) {
        let staging = texture(device, &desc(DXGI_FORMAT_NV12, (width, height), true), None);
        let context = unsafe { device.GetImmediateContext() }.unwrap();
        unsafe { context.CopyResource(&staging, source) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.unwrap();
        let pitch = mapped.RowPitch as usize;
        let all = unsafe {
            std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * height * 3 / 2)
        };
        let rows = |from: usize, count: usize| -> Vec<u8> {
            (0..count)
                .flat_map(|r| all[(from + r) * pitch..][..width].to_vec())
                .collect()
        };
        let planes = (rows(0, height), rows(height, height / 2));
        unsafe { context.Unmap(&staging, 0) };
        planes
    }

    /// A desktop-like picture in colour: gradients, text stripes, a window.
    pub(crate) fn picture((width, height): (usize, usize)) -> Vec<u8> {
        let mut bgra = vec![0u8; width * height * 4];
        for (i, pixel) in bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i % width, i / width);
            let text = y % 12 < 8 && (x / 5 + y / 12) % 4 != 0 && x % 5 < 3;
            *pixel = if text {
                [40, 30, 30, 255]
            } else {
                [(x * 255 / width) as u8, (y * 255 / height) as u8, 200, 255]
            };
        }
        for y in height / 3..height / 3 + 24 {
            for x in width / 4..width / 4 + 48 {
                bgra[(y * width + x) * 4..][..4].copy_from_slice(&[200, 90, 40, 255]);
            }
        }
        bgra
    }

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let error: f64 = a
            .iter()
            .zip(b)
            .map(|(a, b)| (f64::from(*a) - f64::from(*b)).powi(2))
            .sum::<f64>()
            / a.len() as f64;
        10.0 * (255.0 * 255.0 / error.max(1e-9)).log10()
    }

    /// The card's conversion: the picture as NV12, and the same picture as
    /// OpenH264 converts it.
    fn both(
        device: &ID3D11Device,
        texture: &ID3D11Texture2D,
        bgra: &[u8],
        size: (usize, usize),
    ) -> [(Vec<u8>, Vec<u8>); 3] {
        let mut converter = Converter::new(device, size).unwrap();
        let converted = converter.convert(texture).unwrap().clone();
        let (y, uv) = nv12(device, &converted, size);
        let (u, v): (Vec<u8>, Vec<u8>) = uv.as_chunks::<2>().0.iter().map(|p| (p[0], p[1])).unzip();
        let mut cpu = YUVBuffer::new(size.0, size.1);
        cpu.read_bgra8(BgraSliceU8::new(bgra, size));
        [
            (y, cpu.y().to_vec()),
            (u, cpu.u().to_vec()),
            (v, cpu.v().to_vec()),
        ]
    }

    fn most_apart(card: &[u8], cpu: &[u8]) -> u8 {
        card.iter()
            .zip(cpu)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn the_card_converts_colours_like_openh264() {
        let Some(device) = video_device() else {
            return;
        };
        let size = (64, 32);
        // Each a flat colour, so how chroma is subsampled does not matter.
        for colour in [
            [200, 90, 40, 255],
            [30, 200, 60, 255],
            [240, 240, 240, 255],
            [16, 16, 16, 255],
            [0, 0, 255, 255],
            [255, 0, 0, 255],
        ] {
            let bgra: Vec<u8> = std::iter::repeat_n(colour, size.0 * size.1)
                .flatten()
                .collect();
            let texture = upload(&device, &bgra, size);
            for (plane, (card, cpu)) in ["Y", "U", "V"]
                .iter()
                .zip(both(&device, &texture, &bgra, size))
            {
                // The card rounds where OpenH264 truncates.
                assert!(
                    most_apart(&card, &cpu) <= 1,
                    "{colour:?} {plane}: {} vs {}",
                    card[0],
                    cpu[0]
                );
            }
        }
    }

    #[test]
    fn the_card_crops_larger_pictures_without_scaling() {
        let Some(device) = video_device() else {
            return;
        };
        let size = (256, 144);
        let bgra = picture(size);
        // Capture can hand over a texture larger than the even size encoded:
        // the rest, here a magenta border, must not show or stretch it.
        let padded_size = (size.0 + 1, size.1 + 1);
        let mut padded = vec![0u8; padded_size.0 * padded_size.1 * 4];
        for pixel in padded.as_chunks_mut::<4>().0 {
            *pixel = [255, 0, 255, 255];
        }
        for row in 0..size.1 {
            padded[row * padded_size.0 * 4..][..size.0 * 4]
                .copy_from_slice(&bgra[row * size.0 * 4..][..size.0 * 4]);
        }
        let texture = upload(&device, &padded, padded_size);
        let [(y, cpu_y), (u, cpu_u), (v, cpu_v)] = both(&device, &texture, &bgra, size);
        assert!(
            most_apart(&y, &cpu_y) <= 1,
            "Y apart by {}",
            most_apart(&y, &cpu_y)
        );
        // Chroma of one-pixel text is subsampled a little differently.
        for (plane, card, cpu) in [("U", &u, &cpu_u), ("V", &v, &cpu_v)] {
            let quality = psnr(card, cpu);
            assert!(quality > 25.0, "{plane}: {quality:.1} dB");
        }
    }

    #[test]
    fn only_bgra_textures_are_taken() {
        let Some(device) = video_device() else {
            return;
        };
        let nv12 = texture(&device, &desc(DXGI_FORMAT_NV12, (64, 32), false), None);
        let error = Readback::default().read(&nv12, (64, 32)).unwrap_err();
        assert!(error.to_string().contains("not BGRA"), "{error:#}");
        let mut converter = Converter::new(&device, (64, 32)).unwrap();
        assert!(converter.convert(&nv12).is_err());
    }

    #[test]
    fn readback_gives_the_pixels_back() {
        let Some(device) = device() else {
            return;
        };
        // Capture hands over odd sizes; encoders take the even part.
        let (full, even) = ((131, 97), (130, 96));
        let bgra = picture(full);
        let texture = upload(&device, &bgra, full);
        let mut readback = Readback::default();
        let pixels = readback.read(&texture, even).unwrap();
        let expected: Vec<u8> = (0..even.1)
            .flat_map(|r| bgra[r * full.0 * 4..][..even.0 * 4].to_vec())
            .collect();
        assert!(pixels == expected.as_slice());
    }
}
