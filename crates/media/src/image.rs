//! Pictures to PNG / JPG bytes through WIC: screenshots, thumbnails, what goes on the clipboard.

use windows::Win32::Foundation::{HGLOBAL, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_ContainerFormatPng, GUID_WICPixelFormat24bppBGR,
    GUID_WICPixelFormat32bppBGRA, IWICImagingFactory, WICBitmapEncoderNoCache,
};
use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, GetHGlobalFromStream, PROPBAG2};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, STATFLAG_NONAME,
    STATSTG,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
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

/// `pixels`: top-down rows of 0xAARRGGBB, `w * h` long; without `alpha` the high byte is garbage and the image
/// is opaque. With `alpha`, PNG keeps it and JPG is composited onto black. `quality` 1..=100 applies to JPG only.
pub fn encode(
    pixels: &[u32],
    w: u32,
    h: u32,
    alpha: bool,
    format: ImageFormat,
    quality: u8,
) -> Result<Vec<u8>, String> {
    check_size(pixels, w, h)?;
    let _com = Com::init()?;
    encode_wic(pixels, w, h, alpha, format, quality)
}

/// Straight alpha as PNG and CF_DIBV5 store it; fully transparent pixels lose their colour, which would
/// otherwise still show whatever was on the screen there.
pub fn straight(p: u32) -> u32 {
    if p >> 24 == 0 { 0 } else { p }
}

/// Composited onto black, opaque: for what can't keep alpha.
pub fn on_black(p: u32) -> u32 {
    let a = p >> 24;
    let scale = |shift: u32| ((((p >> shift) & 0xFF) * a + 127) / 255) << shift;
    0xFF00_0000 | scale(16) | scale(8) | scale(0)
}

fn check_size(pixels: &[u32], w: u32, h: u32) -> Result<(), String> {
    // DIB sizes and WIC strides are u32 byte counts, so the whole 32bpp image must fit in one.
    let fits = (w as u64 * h as u64 * 4) <= u32::MAX as u64;
    if w == 0 || h == 0 || !fits || pixels.len() as u64 != w as u64 * h as u64 {
        return Err(format!("bad image: {w}x{h} with {} pixels", pixels.len()));
    }
    Ok(())
}

fn encode_wic(
    pixels: &[u32],
    w: u32,
    h: u32,
    alpha: bool,
    format: ImageFormat,
    quality: u8,
) -> Result<Vec<u8>, String> {
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
        // 32bppBGRA (straight, unlike PBGRA) is native to the PNG encoder and gives colour type 6.
        let keep_alpha = alpha && format == ImageFormat::Png;
        let wanted = if keep_alpha { GUID_WICPixelFormat32bppBGRA } else { GUID_WICPixelFormat24bppBGR };
        let mut pixel_format = wanted;
        frame.SetPixelFormat(&mut pixel_format).map_err(|e| format!("WIC SetPixelFormat: {e}"))?;
        if pixel_format != wanted {
            return Err(format!("WIC SetPixelFormat: encoder wants {pixel_format:?} instead of {wanted:?}"));
        }

        // Converted in ~1 MiB bands so a 4K shot doesn't need a second full-size buffer.
        let stride = w as usize * if keep_alpha { 4 } else { 3 };
        let band_rows = ((1 << 20) / stride).max(1);
        let mut band = Vec::with_capacity(band_rows.min(h as usize) * stride);
        for rows in pixels.chunks(w as usize * band_rows) {
            band.clear();
            if keep_alpha {
                band.extend(rows.iter().flat_map(|&p| straight(p).to_le_bytes()));
            } else {
                band.extend(rows.iter().flat_map(|&p| {
                    let [b, g, r, _] = if alpha { on_black(p) } else { p }.to_le_bytes();
                    [b, g, r]
                }));
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Imaging::{
        GUID_WICPixelFormat32bppBGR, IWICPalette, WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
        WICDecodeMetadataCacheOnDemand,
    };
    use windows::core::GUID;

    fn decode(bytes: &[u8]) -> (u32, u32, Vec<u32>) {
        let (w, h, px) = decode_as(bytes, &GUID_WICPixelFormat32bppBGR);
        (w, h, px.into_iter().map(|p| p & 0x00FF_FFFF).collect())
    }

    fn decode_as(bytes: &[u8], format: &GUID) -> (u32, u32, Vec<u32>) {
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
                    format,
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
            (w, h, buf.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
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

        let png = encode(&pixels, w, h, false, ImageFormat::Png, 1).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert_eq!(decode(&png), (w, h, expected.clone()));

        let jpg = encode(&pixels, w, h, false, ImageFormat::Jpg, 90).unwrap();
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
        assert!(encode(&pixels, w, h, false, ImageFormat::Jpg, 10).unwrap().len() < jpg.len());
    }

    #[test]
    fn alpha_survives_png_and_turns_black_in_jpg() {
        let (w, h) = (4, 3);
        // Opaque, half, and fully transparent pixels over a colour that must not leak into the file.
        let pixels: Vec<u32> =
            (0..w * h).map(|i| [0xFF00_0000, 0x8000_0000, 0][i as usize % 3] | 0x00C0_6020 | i).collect();
        let _com = Com::init().unwrap();

        let png = encode(&pixels, w, h, true, ImageFormat::Png, 100).unwrap();
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(png[25], 6, "colour type: truecolour with alpha");
        let expected: Vec<u32> = pixels.iter().map(|&p| if p >> 24 == 0 { 0 } else { p }).collect();
        assert_eq!(decode_as(&png, &GUID_WICPixelFormat32bppBGRA), (w, h, expected));

        let opaque = encode(&pixels, w, h, false, ImageFormat::Png, 100).unwrap();
        assert_eq!(opaque[25], 2, "colour type: truecolour");

        let jpg = encode(&pixels, w, h, true, ImageFormat::Jpg, 100).unwrap();
        let (_, _, decoded) = decode(&jpg);
        let green = |p: u32| (p >> 8) & 0xFF;
        assert!(green(decoded[2]) < 8 && green(decoded[0]) > 0x50, "{decoded:08X?}");
        assert_eq!(on_black(0x8040_80FF), 0xFF20_4080);
    }
}
