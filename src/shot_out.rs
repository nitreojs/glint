//! Turns a finished screenshot (raw pixels) into PNG/JPG bytes and puts it on the clipboard.

use std::time::Duration;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPV5HEADER, LCS_GM_IMAGES};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_ContainerFormatPng, GUID_WICPixelFormat24bppBGR,
    IWICImagingFactory, WICBitmapEncoderNoCache,
};
use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, GetHGlobalFromStream, PROPBAG2};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, STATFLAG_NONAME,
    STATSTG,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_DIBV5;
use windows::Win32::System::Variant::{VARIANT, VT_R4};
use windows::core::{Error, PWSTR, w};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpg,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpg => "jpg",
        }
    }
}

/// `pixels`: top-down rows of 0x00RRGGBB (the high byte is garbage/ignored; the image is opaque), `w * h` long.
/// `quality` 1..=100 applies to JPG only.
pub fn encode(pixels: &[u32], w: u32, h: u32, format: ImageFormat, quality: u8) -> Result<Vec<u8>, String> {
    check_size(pixels, w, h)?;
    let _com = Com::init()?;
    encode_wic(pixels, w, h, format, quality)
}

/// Puts the image on the clipboard as CF_DIBV5 (so every app can paste it) and as the registered
/// "PNG" format (what Telegram, browsers, Office prefer). `owner` may be the caller's window.
pub fn copy_to_clipboard(owner: Option<HWND>, pixels: &[u32], w: u32, h: u32) -> Result<(), String> {
    // Everything slow happens before OpenClipboard so other apps are locked out only briefly.
    let png = encode(pixels, w, h, ImageFormat::Png, 100)?;
    let png_format = unsafe { RegisterClipboardFormatW(w!("PNG")) };
    if png_format == 0 {
        return Err(format!("RegisterClipboardFormatW: {}", Error::from_thread()));
    }
    let png_mem = Global::new(png.len(), |buf| buf.copy_from_slice(&png))?;
    let header = BITMAPV5HEADER {
        bV5Size: size_of::<BITMAPV5HEADER>() as u32,
        bV5Width: w as i32,
        // Positive = bottom-up: top-down DIBs are mishandled by some older paste targets.
        bV5Height: h as i32,
        bV5Planes: 1,
        bV5BitCount: 32,
        bV5Compression: BI_RGB,
        bV5SizeImage: w * h * 4,
        bV5CSType: LCS_SRGB,
        bV5Intent: LCS_GM_IMAGES as u32,
        ..Default::default()
    };
    let header_len = size_of::<BITMAPV5HEADER>();
    let dib_mem = Global::new(header_len + pixels.len() * 4, |buf| {
        let (head, body) = buf.split_at_mut(header_len);
        // SAFETY: BITMAPV5HEADER is a plain repr(C) struct of integers, `head` is exactly its size.
        head.copy_from_slice(unsafe { std::slice::from_raw_parts((&raw const header).cast::<u8>(), header_len) });
        for (dst, src) in body.chunks_exact_mut(w as usize * 4).zip(pixels.chunks_exact(w as usize).rev()) {
            for (d, p) in dst.chunks_exact_mut(4).zip(src) {
                // BI_RGB ignores the 4th byte officially, but apps that read it as alpha must see opaque.
                d.copy_from_slice(&(p | 0xFF00_0000).to_le_bytes());
            }
        }
    })?;

    let _clipboard = Clipboard::open(owner)?;
    unsafe { EmptyClipboard() }.map_err(|e| format!("EmptyClipboard: {e}"))?;
    png_mem.give_to_clipboard(png_format)?;
    dib_mem.give_to_clipboard(u32::from(CF_DIBV5.0))
}

/// 'sRGB' (LCS_sRGB lives in UI_ColorSystem, not worth a crate feature for one constant).
const LCS_SRGB: u32 = u32::from_be_bytes(*b"sRGB");

fn check_size(pixels: &[u32], w: u32, h: u32) -> Result<(), String> {
    // DIB sizes and WIC strides are u32 byte counts, so the whole 32bpp image must fit in one.
    let fits = (w as u64 * h as u64 * 4) <= u32::MAX as u64;
    if w == 0 || h == 0 || !fits || pixels.len() as u64 != w as u64 * h as u64 {
        return Err(format!("bad image: {w}x{h} with {} pixels", pixels.len()));
    }
    Ok(())
}

fn encode_wic(pixels: &[u32], w: u32, h: u32, format: ImageFormat, quality: u8) -> Result<Vec<u8>, String> {
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("WIC CoCreateInstance: {e}"))?;
        let container = match format {
            ImageFormat::Png => GUID_ContainerFormatPng,
            ImageFormat::Jpg => GUID_ContainerFormatJpeg,
        };
        let encoder =
            factory.CreateEncoder(&container, std::ptr::null()).map_err(|e| format!("WIC CreateEncoder: {e}"))?;
        let stream =
            CreateStreamOnHGlobal(HGLOBAL::default(), true).map_err(|e| format!("CreateStreamOnHGlobal: {e}"))?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache).map_err(|e| format!("WIC encoder Initialize: {e}"))?;
        let (mut frame, mut props) = (None, None);
        encoder.CreateNewFrame(&mut frame, &mut props).map_err(|e| format!("WIC CreateNewFrame: {e}"))?;
        let frame = frame.ok_or("WIC CreateNewFrame returned no frame")?;
        if format == ImageFormat::Jpg {
            let props = props.as_ref().ok_or("WIC CreateNewFrame returned no encoder options")?;
            let name = PROPBAG2 { pstrName: PWSTR(w!("ImageQuality").as_ptr().cast_mut()), ..Default::default() };
            let mut value = VARIANT::default();
            (*value.Anonymous.Anonymous).vt = VT_R4;
            (*value.Anonymous.Anonymous).Anonymous.fltVal = f32::from(quality.clamp(1, 100)) / 100.0;
            props.Write(1, &name, &value).map_err(|e| format!("WIC ImageQuality: {e}"))?;
        }
        frame.Initialize(props.as_ref()).map_err(|e| format!("WIC frame Initialize: {e}"))?;
        frame.SetSize(w, h).map_err(|e| format!("WIC SetSize: {e}"))?;
        // 24bpp is native to both encoders and leaves no alpha channel in the file, so the result is opaque.
        let mut pixel_format = GUID_WICPixelFormat24bppBGR;
        frame.SetPixelFormat(&mut pixel_format).map_err(|e| format!("WIC SetPixelFormat: {e}"))?;
        if pixel_format != GUID_WICPixelFormat24bppBGR {
            return Err(format!("WIC SetPixelFormat: encoder wants {pixel_format:?} instead of 24bpp BGR"));
        }

        // Converted in ~1 MiB bands so a 4K shot doesn't need a second full-size buffer.
        let stride = w as usize * 3;
        let band_rows = ((1 << 20) / stride).max(1);
        let mut band = Vec::with_capacity(band_rows.min(h as usize) * stride);
        for rows in pixels.chunks(w as usize * band_rows) {
            band.clear();
            band.extend(rows.iter().flat_map(|p| {
                let [b, g, r, _] = p.to_le_bytes();
                [b, g, r]
            }));
            frame
                .WritePixels((rows.len() / w as usize) as u32, stride as u32, &band)
                .map_err(|e| format!("WIC WritePixels: {e}"))?;
        }
        frame.Commit().map_err(|e| format!("WIC frame Commit: {e}"))?;
        encoder.Commit().map_err(|e| format!("WIC encoder Commit: {e}"))?;

        let mut stat = STATSTG::default();
        stream.Stat(&mut stat, STATFLAG_NONAME).map_err(|e| format!("IStream Stat: {e}"))?;
        let hglobal = GetHGlobalFromStream(&stream).map_err(|e| format!("GetHGlobalFromStream: {e}"))?;
        let ptr = GlobalLock(hglobal).cast::<u8>();
        if ptr.is_null() {
            return Err(format!("GlobalLock: {}", Error::from_thread()));
        }
        let bytes = std::slice::from_raw_parts(ptr, stat.cbSize as usize).to_vec();
        // Reports "failure" with NO_ERROR once the lock count reaches zero, which is the expected outcome.
        let _ = GlobalUnlock(hglobal);
        Ok(bytes)
    }
}

/// Keeps COM initialized for the current call; the tray's GUI thread never initializes it itself.
struct Com {
    uninit: bool,
}

impl Com {
    fn init() -> Result<Com, String> {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr.is_ok() {
            // S_OK and S_FALSE both take a reference that must be released.
            Ok(Com { uninit: true })
        } else if hr == RPC_E_CHANGED_MODE {
            // Already initialized as MTA by someone else; WIC works there too.
            Ok(Com { uninit: false })
        } else {
            Err(format!("CoInitializeEx: {}", Error::from_hresult(hr)))
        }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.uninit {
            unsafe { CoUninitialize() };
        }
    }
}

/// A GMEM_MOVEABLE block that's freed unless the clipboard takes ownership of it.
struct Global(HGLOBAL);

impl Global {
    fn new(len: usize, fill: impl FnOnce(&mut [u8])) -> Result<Global, String> {
        let mem = Global(unsafe { GlobalAlloc(GMEM_MOVEABLE, len) }.map_err(|e| format!("GlobalAlloc: {e}"))?);
        let ptr = unsafe { GlobalLock(mem.0) }.cast::<u8>();
        if ptr.is_null() {
            return Err(format!("GlobalLock: {}", Error::from_thread()));
        }
        fill(unsafe { std::slice::from_raw_parts_mut(ptr, len) });
        // Same NO_ERROR "failure" as in encode_wic.
        let _ = unsafe { GlobalUnlock(mem.0) };
        Ok(mem)
    }

    fn give_to_clipboard(self, format: u32) -> Result<(), String> {
        unsafe { SetClipboardData(format, Some(HANDLE(self.0.0))) }.map_err(|e| format!("SetClipboardData: {e}"))?;
        std::mem::forget(self);
        Ok(())
    }
}

impl Drop for Global {
    fn drop(&mut self) {
        let _ = unsafe { GlobalFree(Some(self.0)) };
    }
}

struct Clipboard;

impl Clipboard {
    fn open(owner: Option<HWND>) -> Result<Clipboard, String> {
        // Clipboard managers and other apps hold it for a few ms at a time; wait them out briefly.
        let mut attempts = 10;
        loop {
            match unsafe { OpenClipboard(owner) } {
                Ok(()) => return Ok(Clipboard),
                Err(e) if attempts == 0 => return Err(format!("OpenClipboard: {e}")),
                Err(_) => attempts -= 1,
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Imaging::{
        GUID_WICPixelFormat32bppBGR, IWICPalette, WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
        WICDecodeMetadataCacheOnDemand,
    };

    fn decode(bytes: &[u8]) -> (u32, u32, Vec<u32>) {
        unsafe {
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
            let stream = factory.CreateStream().unwrap();
            stream.InitializeFromMemory(bytes).unwrap();
            let decoder =
                factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).unwrap();
            let converter = factory.CreateFormatConverter().unwrap();
            converter
                .Initialize(
                    &decoder.GetFrame(0).unwrap(),
                    &GUID_WICPixelFormat32bppBGR,
                    WICBitmapDitherTypeNone,
                    None::<&IWICPalette>,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
                .unwrap();
            let (mut w, mut h) = (0, 0);
            converter.GetSize(&mut w, &mut h).unwrap();
            let mut buf = vec![0u8; (w * h * 4) as usize];
            converter.CopyPixels(std::ptr::null(), w * 4, &mut buf).unwrap();
            (w, h, buf.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], 0])).collect())
        }
    }

    #[test]
    fn png_and_jpg_round_trip() {
        let (w, h) = (7, 5);
        // Smooth gradient: every pixel distinct, yet gentle enough for JPEG's chroma subsampling.
        let expected: Vec<u32> =
            (0..h).flat_map(|y| (0..w).map(move |x| (x * 24) << 16 | (y * 30) << 8 | ((x + y) * 12))).collect();
        // Garbage in the high byte must not leak into the output.
        let pixels: Vec<u32> = expected.iter().map(|p| p | 0x5A00_0000).collect();
        let _com = Com::init().unwrap();

        let png = encode(&pixels, w, h, ImageFormat::Png, 1).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert_eq!(decode(&png), (w, h, expected.clone()));

        let jpg = encode(&pixels, w, h, ImageFormat::Jpg, 90).unwrap();
        assert_eq!(&jpg[..2], &[0xFF, 0xD8]);
        let (jw, jh, decoded) = decode(&jpg);
        assert_eq!((jw, jh), (w, h));
        let worst = expected
            .iter()
            .zip(&decoded)
            .flat_map(|(a, b)| {
                (0..3).map(move |i| (((a >> (i * 8)) & 0xFF) as i32 - ((b >> (i * 8)) & 0xFF) as i32).abs())
            })
            .max()
            .unwrap();
        // 4:2:0 chroma subsampling alone smears neighbouring colours by up to about half their difference.
        assert!(worst <= 24, "JPEG drifted by {worst}");
        // Proves ImageQuality reaches the encoder.
        assert!(encode(&pixels, w, h, ImageFormat::Jpg, 10).unwrap().len() < jpg.len());
    }
}
