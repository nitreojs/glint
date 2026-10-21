use std::mem::ManuallyDrop;

use glint_core::clip::crop::Crop;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::D3D11_BIND_RENDER_TARGET;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::Interface;

use super::media::{Gpu, attributes, pack};
use crate::record::Ctx;

/// The media foundation video processor cutting `crop` out of each frame and scaling it, on the gpu when given.
pub(super) struct Processor {
    mft: IMFTransform,
    alloc: Option<IMFVideoSampleAllocatorEx>,
    provides: bool,
    out_bytes: u32,
}

fn nv12(size: (u32, u32), fps: f64) -> Result<IMFMediaType, String> {
    unsafe {
        let t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ctx("mt")?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).ctx("mt")?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).ctx("mt")?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(size.0, size.1)).ctx("mt")?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack((fps * 1000.0).round() as u32, 1000)).ctx("mt")?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).ctx("mt")?;
        Ok(t)
    }
}

impl Processor {
    pub(super) fn new(gpu: Option<&Gpu>, input: (u32, u32), crop: Crop, out: (u32, u32), fps: f64) -> Result<Self, String> {
        unsafe {
            let mft: IMFTransform =
                CoCreateInstance(&CLSID_VideoProcessorMFT, None, CLSCTX_INPROC_SERVER).ctx("video processor")?;
            if let Some(g) = gpu {
                mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, g.mgr.as_raw() as usize).ctx("video processor d3d")?;
            }
            mft.SetInputType(0, &nv12(input, fps)?, 0).ctx("video processor input type")?;
            let out_t = nv12(out, fps)?;
            mft.SetOutputType(0, &out_t, 0).ctx("video processor output type")?;
            let ctl: IMFVideoProcessorControl = mft.cast().ctx("IMFVideoProcessorControl")?;
            let src = RECT {
                left: crop.x as i32,
                top: crop.y as i32,
                right: (crop.x + crop.w) as i32,
                bottom: (crop.y + crop.h) as i32,
            };
            ctl.SetSourceRectangle(Some(&src)).ctx("SetSourceRectangle")?;
            let dst = RECT { left: 0, top: 0, right: out.0 as i32, bottom: out.1 as i32 };
            ctl.SetDestinationRectangle(Some(&dst)).ctx("SetDestinationRectangle")?;
            let info = mft.GetOutputStreamInfo(0).ctx("GetOutputStreamInfo")?;
            let own = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
            let provides = info.dwFlags & own != 0;
            let alloc = match gpu {
                Some(g) if !provides => {
                    let mut raw = std::ptr::null_mut();
                    MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut raw)
                        .ctx("MFCreateVideoSampleAllocatorEx")?;
                    let a = IMFVideoSampleAllocatorEx::from_raw(raw);
                    a.SetDirectXManager(&g.mgr).ctx("SetDirectXManager")?;
                    let attrs = attributes(1)?;
                    attrs.SetUINT32(&MF_SA_D3D11_BINDFLAGS, D3D11_BIND_RENDER_TARGET.0 as u32).ctx("attr")?;
                    a.InitializeSampleAllocatorEx(4, 32, &attrs, &out_t).ctx("InitializeSampleAllocatorEx")?;
                    Some(a)
                }
                _ => None,
            };
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).ctx("video processor start")?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).ctx("video processor start")?;
            Ok(Processor { mft, alloc, provides, out_bytes: info.cbSize.max(out.0 * out.1 * 3 / 2) })
        }
    }

    fn output(&self) -> Result<IMFSample, windows::core::Error> {
        unsafe {
            let sample = if self.provides {
                None
            } else if let Some(a) = &self.alloc {
                Some(a.AllocateSample()?)
            } else {
                let s = MFCreateSample()?;
                s.AddBuffer(&MFCreateMemoryBuffer(self.out_bytes)?)?;
                Some(s)
            };
            let mut buf = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0;
            let res = self.mft.ProcessOutput(0, &mut buf, &mut status);
            let out = ManuallyDrop::take(&mut buf[0].pSample);
            drop(ManuallyDrop::take(&mut buf[0].pEvents));
            res?;
            out.ok_or_else(|| windows::core::Error::from(MF_E_TRANSFORM_NEED_MORE_INPUT))
        }
    }

    /// One frame in, one out; the processor lets go of its input only once asked for more output.
    pub(super) fn run(&self, input: &IMFSample) -> Result<IMFSample, String> {
        unsafe {
            self.mft.ProcessInput(0, input, 0).ctx("video processor ProcessInput")?;
        }
        let out = self.output().ctx("video processor ProcessOutput")?;
        while self.output().is_ok() {}
        Ok(out)
    }
}

impl Drop for Processor {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
    }
}
