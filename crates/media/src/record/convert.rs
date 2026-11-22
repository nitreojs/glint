//! BGRA → NV12 on the GPU with the D3D11 video processor, for encoders that don't take ARGB32
//! (Intel Quick Sync, AMD AMF, Windows' software encoder): full-range sRGB in, limited-range
//! YUV out with the stream's matrix (BT.709, or BT.601 where players will read it so).

use std::mem::ManuallyDrop;

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::Interface;

use glint_core::record::negotiate::Matrix;

use super::Ctx;

/// Views the converter made, kept per texture (and array slice) it has seen; the encoder's
/// sample pool is small, so this stays short.
const MAX_VIEWS: usize = 32;

pub(super) struct Converter {
    vdev: ID3D11VideoDevice,
    vctx: ID3D11VideoContext,
    en: ID3D11VideoProcessorEnumerator,
    vp: ID3D11VideoProcessor,
    inputs: Vec<(ID3D11Texture2D, u32, ID3D11VideoProcessorInputView)>,
    outputs: Vec<(ID3D11Texture2D, u32, ID3D11VideoProcessorOutputView)>,
}

impl Converter {
    /// Converts `w`×`h` BGRA images into NV12 ones of the same size on `device`.
    pub(super) unsafe fn new(device: &ID3D11Device, ctx: &ID3D11DeviceContext, (w, h): (u32, u32)) -> Result<Converter, String> {
        let mut converter = unsafe { Self::scaled(
            device, ctx, (w, h), (w, h), DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12,
        )? };
        converter.set_matrix(Matrix::Bt709);

        Ok(converter)
    }

    pub(super) unsafe fn scaled(
        device: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
        input: (u32, u32),
        output: (u32, u32),
        input_format: DXGI_FORMAT,
        output_format: DXGI_FORMAT,
    ) -> Result<Self, String> {
        unsafe {
            let vdev: ID3D11VideoDevice = device.cast().ctx("ID3D11VideoDevice")?;
            let vctx: ID3D11VideoContext = ctx.cast().ctx("ID3D11VideoContext")?;
            let rate = DXGI_RATIONAL { Numerator: 60, Denominator: 1 };
            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: rate,
                InputWidth: input.0,
                InputHeight: input.1,
                OutputFrameRate: rate,
                OutputWidth: output.0,
                OutputHeight: output.1,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let en = vdev.CreateVideoProcessorEnumerator(&desc).ctx("CreateVideoProcessorEnumerator")?;
            let supports = |format, what: D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT| {
                en.CheckVideoProcessorFormat(format).is_ok_and(|s| s & what.0 as u32 != 0)
            };
            if !supports(input_format, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT)
                || !supports(output_format, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT)
            {
                return Err("the GPU's video processor doesn't support the requested formats".into());
            }
            let vp = vdev.CreateVideoProcessor(&en, 0).ctx("CreateVideoProcessor")?;
            let rect = |(w, h): (u32, u32)| RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
            vctx.VideoProcessorSetStreamFrameFormat(&vp, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            vctx.VideoProcessorSetStreamSourceRect(&vp, 0, true, Some(&rect(input)));
            vctx.VideoProcessorSetStreamDestRect(&vp, 0, true, Some(&rect(output)));
            vctx.VideoProcessorSetOutputTargetRect(&vp, true, Some(&rect(output)));
            vctx.VideoProcessorSetStreamAutoProcessingMode(&vp, 0, false);
            Ok(Converter { vdev, vctx, en, vp, inputs: Vec::new(), outputs: Vec::new() })
        }
    }

    /// Makes YUV with `matrix` from now on (limited range either way).
    pub(super) fn set_matrix(&mut self, matrix: Matrix) {
        let (vctx, vp) = (&self.vctx, &self.vp);
        unsafe {
            match vctx.cast::<ID3D11VideoContext1>() {
                Ok(v1) => {
                    let output = match matrix {
                        Matrix::Bt709 => DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                        Matrix::Bt601 => DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601,
                    };
                    v1.VideoProcessorSetStreamColorSpace1(vp, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                    v1.VideoProcessorSetOutputColorSpace1(vp, output);
                }
                Err(_) => {
                    // bits: 1 RGB range (0 = full), 2 YCbCr matrix (1 = BT.709), 4-5 nominal range
                    let bt709 = (matrix == Matrix::Bt709) as u32;
                    let input = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: bt709 << 2 };
                    let output = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                        _bitfield: bt709 << 2 | (D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235.0 as u32) << 4,
                    };
                    vctx.VideoProcessorSetStreamColorSpace(vp, 0, &input);
                    vctx.VideoProcessorSetOutputColorSpace(vp, &output);
                }
            }
        }
    }

    pub(super) fn to_rgb(&self, nv12: bool, matrix: Matrix) {
        if let Ok(ctx) = self.vctx.cast::<ID3D11VideoContext1>() {
            let rgb = DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709;
            let input = match (nv12, matrix) {
                (false, _) => rgb,
                (true, Matrix::Bt709) => DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                (true, Matrix::Bt601) => DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601,
            };
            unsafe {
                ctx.VideoProcessorSetStreamColorSpace1(&self.vp, 0, input);
                ctx.VideoProcessorSetOutputColorSpace1(&self.vp, rgb);
            }
        } else {
            let bt709 = (matrix == Matrix::Bt709) as u32;
            let range = if nv12 {
                D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235
            } else {
                D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_0_255
            };
            unsafe {
                self.vctx.VideoProcessorSetStreamColorSpace(&self.vp, 0,
                    &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: bt709 << 2 | (range.0 as u32) << 4 });
                self.vctx.VideoProcessorSetOutputColorSpace(&self.vp,
                    &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 });
            }
        }
    }

    /// Queues the conversion of `src` (BGRA) into slice `sub` of `dst` (NV12) on the device's
    /// immediate context; the caller holds the device's multithread lock.
    pub(super) unsafe fn convert(&mut self, src: &ID3D11Texture2D, dst: &ID3D11Texture2D, sub: u32) -> Result<(), String> {
        unsafe { self.blit(src, 0, dst, sub) }
    }

    pub(super) unsafe fn blit(
        &mut self,
        src: &ID3D11Texture2D,
        src_sub: u32,
        dst: &ID3D11Texture2D,
        dst_sub: u32,
    ) -> Result<(), String> {
        unsafe {
            let input = self.input_view(src, src_sub)?;
            let output = self.output_view(dst, dst_sub)?;
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(input)),
                ..Default::default()
            };
            let streams = [stream];
            let r = self.vctx.VideoProcessorBlt(&self.vp, &output, 0, &streams);
            let [stream] = streams;
            drop(ManuallyDrop::into_inner(stream.pInputSurface));
            r.ctx("VideoProcessorBlt")
        }
    }

    unsafe fn input_view(&mut self, tex: &ID3D11Texture2D, sub: u32) -> Result<ID3D11VideoProcessorInputView, String> {
        if let Some((_, _, v)) = self.inputs.iter().find(|(t, s, _)| t.as_raw() == tex.as_raw() && *s == sub) {
            return Ok(v.clone());
        }
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: sub } },
        };
        let mut view = None;
        unsafe { self.vdev.CreateVideoProcessorInputView(tex, &self.en, &desc, Some(&mut view)) }
            .ctx("CreateVideoProcessorInputView")?;
        let view = view.ok_or("CreateVideoProcessorInputView: no view")?;
        if self.inputs.len() >= MAX_VIEWS {
            self.inputs.clear();
        }
        self.inputs.push((tex.clone(), sub, view.clone()));
        Ok(view)
    }

    unsafe fn output_view(&mut self, tex: &ID3D11Texture2D, sub: u32) -> Result<ID3D11VideoProcessorOutputView, String> {
        if let Some((_, _, v)) = self.outputs.iter().find(|(t, s, _)| t.as_raw() == tex.as_raw() && *s == sub) {
            return Ok(v.clone());
        }
        let mut td = D3D11_TEXTURE2D_DESC::default();
        unsafe { tex.GetDesc(&mut td) };
        let desc = if td.ArraySize > 1 {
            D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2DARRAY,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2DArray: D3D11_TEX2D_ARRAY_VPOV { MipSlice: 0, FirstArraySlice: sub, ArraySize: 1 },
                },
            }
        } else {
            D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            }
        };
        let mut view = None;
        unsafe { self.vdev.CreateVideoProcessorOutputView(tex, &self.en, &desc, Some(&mut view)) }
            .ctx("CreateVideoProcessorOutputView")?;
        let view = view.ok_or("CreateVideoProcessorOutputView: no view")?;
        if self.outputs.len() >= MAX_VIEWS {
            self.outputs.clear();
        }
        self.outputs.push((tex.clone(), sub, view.clone()));
        Ok(view)
    }
}
