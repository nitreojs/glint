//! Pictures off the UI thread: WIC reads screenshots and gifs' first frames, scaled while it
//! decodes, and the thumbnail cache's files. The calling thread needs COM.

use std::path::Path;

use eframe::egui::{Color32, ColorImage};
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat32bppBGRA, IWICBitmapDecoder, IWICBitmapSource,
    IWICImagingFactory, IWICPalette, WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::{HSTRING, Interface};

/// Straight-alpha BGRA rows.
pub(super) struct Pixels {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

impl Pixels {
    pub fn alpha(&self) -> bool {
        self.bgra.chunks_exact(4).any(|p| p[3] != 255)
    }

    pub fn image(&self) -> ColorImage {
        let px = self.bgra.chunks_exact(4).map(|p| Color32::from_rgba_unmultiplied(p[2], p[1], p[0], p[3])).collect();
        ColorImage::new([self.w as usize, self.h as usize], px)
    }

    /// As 0xAARRGGBB, what the encoder and the clipboard take.
    pub fn argb(&self) -> Vec<u32> {
        self.bgra.chunks_exact(4).map(|p| u32::from_le_bytes([p[0], p[1], p[2], p[3]])).collect()
    }

    /// Frames decoded as BGRX (a video's): the fourth byte is padding.
    pub fn opaque(w: u32, h: u32, mut bgrx: Vec<u8>) -> Pixels {
        bgrx.chunks_exact_mut(4).for_each(|p| p[3] = 255);
        Pixels { w, h, bgra: bgrx }
    }
}

pub(super) struct Wic(IWICImagingFactory);

impl Wic {
    pub fn new() -> Result<Wic, String> {
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .map(Wic)
            .map_err(|e| format!("WIC: {e}"))
    }

    /// `path`'s first frame at `fit(its size)`, and its own size.
    pub fn file(&self, path: &Path, fit: impl FnOnce(u32, u32) -> (u32, u32)) -> Result<(Pixels, (u32, u32)), String> {
        let name = HSTRING::from(path.as_os_str());
        let decoder = unsafe {
            self.0.CreateDecoderFromFilename(&name, None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
        }
        .map_err(|e| format!("can't read {}: {e}", path.display()))?;
        self.frame(&decoder, fit)
    }

    /// An encoded image (a cache file's).
    pub fn memory(&self, bytes: &[u8]) -> Result<Pixels, String> {
        unsafe {
            let stream = self.0.CreateStream().map_err(|e| e.to_string())?;
            stream.InitializeFromMemory(bytes).map_err(|e| e.to_string())?;
            let decoder = self
                .0
                .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
                .map_err(|e| e.to_string())?;
            self.frame(&decoder, |w, h| (w, h)).map(|(p, _)| p)
        }
    }

    fn frame(&self, decoder: &IWICBitmapDecoder, fit: impl FnOnce(u32, u32) -> (u32, u32)) -> Result<(Pixels, (u32, u32)), String> {
        let err = |e: windows::core::Error| e.message();
        unsafe {
            let frame = decoder.GetFrame(0).map_err(err)?;
            let (mut w, mut h) = (0, 0);
            frame.GetSize(&mut w, &mut h).map_err(err)?;
            let (tw, th) = fit(w, h);
            let frame: IWICBitmapSource = frame.cast().map_err(err)?;
            let scale = |src: &IWICBitmapSource| -> Result<IWICBitmapSource, String> {
                if (tw, th) == (w, h) {
                    return Ok(src.clone());
                }
                let scaler = self.0.CreateBitmapScaler().map_err(err)?;
                scaler.Initialize(src, tw, th, WICBitmapInterpolationModeFant).map_err(err)?;
                scaler.cast().map_err(err)
            };
            let convert = |src: &IWICBitmapSource| -> Result<IWICBitmapSource, String> {
                let conv = self.0.CreateFormatConverter().map_err(err)?;
                let palette = None::<&IWICPalette>;
                conv.Initialize(src, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, palette, 0.0, WICBitmapPaletteTypeCustom)
                    .map_err(err)?;
                conv.cast().map_err(err)
            };
            // jpeg decodes straight to a smaller size when the scaler sits right on it
            let jpeg = decoder.GetContainerFormat().is_ok_and(|f| f == GUID_ContainerFormatJpeg);
            let out = if jpeg { convert(&scale(&frame)?)? } else { scale(&convert(&frame)?)? };
            let mut bgra = vec![0u8; tw as usize * th as usize * 4];
            out.CopyPixels(std::ptr::null(), tw * 4, &mut bgra).map_err(err)?;
            Ok((Pixels { w: tw, h: th, bgra }, (w, h)))
        }
    }
}
