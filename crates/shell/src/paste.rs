//! Taking pictures and text off the clipboard: what Ctrl+V pastes into a screenshot, and the
//! pictures in the clipboard history (Win+V).

use std::path::PathBuf;
use std::time::Duration;

use glint_media::image::decode;
use windows::ApplicationModel::DataTransfer::{Clipboard, ClipboardHistoryItemsResultStatus, StandardDataFormats};
use windows::Storage::Streams::DataReader;
use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::System::DataExchange::{
    CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
use windows::core::w;

/// A picture: `w * h` top-down premultiplied 0xAARRGGBB pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
}

impl Image {
    pub fn decode(bytes: &[u8]) -> Option<Image> {
        decode(bytes).ok().map(|(w, h, px)| Image { w, h, px })
    }
}

/// The clipboard formats a picture can come in, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// The registered "PNG" format: keeps alpha (browsers, Telegram, Office, glint).
    Png,
    DibV5,
    Dib,
    /// Files copied in Explorer: the first one that's a picture.
    Files,
}

const FORMATS: [Format; 4] = [Format::Png, Format::DibV5, Format::Dib, Format::Files];

/// The best picture among a clipboard's formats; `get` hands out a format's bytes, if it's there.
pub fn image_from(get: impl Fn(Format) -> Option<Vec<u8>>) -> Option<Image> {
    FORMATS.iter().find_map(|&f| {
        let bytes = get(f)?;
        match f {
            Format::Png => Image::decode(&bytes),
            Format::DibV5 | Format::Dib => from_dib(&bytes),
            Format::Files => hdrop_paths(&bytes).iter().find_map(|p| Image::decode(&std::fs::read(p).ok()?)),
        }
    })
}

/// The picture on the clipboard, if there is one.
pub fn read_image() -> Option<Image> {
    // copied out while the clipboard is open, decoded after: other apps wait only for the copy
    let found: Vec<(Format, Vec<u8>)> = {
        let _open = Open::new()?;
        let mut found = Vec::new();
        for f in FORMATS {
            if let Some(bytes) = data(format_id(f)?) {
                let done = f != Format::Files;
                found.push((f, bytes));
                if done {
                    break;
                }
            }
        }
        found
    };
    image_from(|f| found.iter().find(|(g, _)| *g == f).map(|(_, b)| b.clone()))
}

/// The text on the clipboard, if there is any.
pub fn read_text() -> Option<String> {
    let bytes = {
        let _open = Open::new()?;
        data(u32::from(CF_UNICODETEXT.0))?
    };
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    Some(String::from_utf16_lossy(&units[..end]))
}

fn format_id(f: Format) -> Option<u32> {
    Some(match f {
        Format::Png => match unsafe { RegisterClipboardFormatW(w!("PNG")) } {
            0 => return None,
            id => id,
        },
        Format::DibV5 => u32::from(CF_DIBV5.0),
        Format::Dib => u32::from(CF_DIB.0),
        Format::Files => u32::from(CF_HDROP.0),
    })
}

/// A copy of the open clipboard's data in `format`.
fn data(format: u32) -> Option<Vec<u8>> {
    unsafe {
        IsClipboardFormatAvailable(format).ok()?;
        let handle = GetClipboardData(format).ok()?;
        let mem = HGLOBAL(handle.0);
        let len = GlobalSize(mem);
        let ptr = GlobalLock(mem).cast::<u8>();
        if ptr.is_null() || len == 0 {
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr, len).to_vec();
        let _ = GlobalUnlock(mem);
        Some(bytes)
    }
}

struct Open;

impl Open {
    fn new() -> Option<Open> {
        // clipboard managers hold it for a few ms at a time
        for _ in 0..10 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Some(Open);
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        None
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;

/// A packed DIB (a BITMAPINFOHEADER or a later one, maybe colour masks, then the rows), 24 or 32
/// bits per pixel. 32-bit pixels keep their alpha when a mask says where it is, or when a plain
/// one has any (an all-zero alpha byte means opaque).
pub fn from_dib(b: &[u8]) -> Option<Image> {
    let size = u32_at(b, 0)? as usize;
    let (w, h) = (u32_at(b, 4)? as i32, u32_at(b, 8)? as i32);
    let (bits, compression) = (u16_at(b, 14)?, u32_at(b, 16)?);
    if size < 40 || w <= 0 || h == 0 || !matches!(bits, 24 | 32) || !matches!(compression, BI_RGB | BI_BITFIELDS) {
        return None;
    }
    let (w, rows) = (w as usize, h.unsigned_abs() as usize);
    let colors = u32_at(b, 32)? as usize;
    let masks_after = size == 40 && compression == BI_BITFIELDS;
    let masks = if compression == BI_BITFIELDS {
        let at = if masks_after { 40 } else { 40.min(size) };
        let alpha = if size >= 56 { u32_at(b, 52)? } else { 0 };
        Some([u32_at(b, at)?, u32_at(b, at + 4)?, u32_at(b, at + 8)?, alpha])
    } else {
        None
    };
    let start = size + if masks_after { 12 } else { 0 } + colors * 4;
    let stride = (w * bits as usize).div_ceil(32) * 4;
    let body = b.get(start..start + stride * rows)?;
    let mut px = Vec::with_capacity(w * rows);
    for y in 0..rows {
        let row = if h > 0 { rows - 1 - y } else { y };
        let line = &body[row * stride..row * stride + stride];
        match bits {
            24 => px.extend(line.chunks_exact(3).take(w).map(|c| 0xFF00_0000 | u32::from_le_bytes([c[0], c[1], c[2], 0]))),
            _ => {
                let raw = line.chunks_exact(4).take(w).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                match masks {
                    Some(m) => px.extend(raw.map(|p| {
                        let [r, g, bl, a] = m.map(|mask| channel(p, mask));
                        let a = if m[3] == 0 { 255 } else { a };
                        u32::from_be_bytes([a, r, g, bl])
                    })),
                    None => px.extend(raw),
                }
            }
        }
    }
    if bits == 32 && masks.is_none() && px.iter().all(|p| p >> 24 == 0) {
        px.iter_mut().for_each(|p| *p |= 0xFF00_0000);
    }
    px.iter_mut().for_each(|p| *p = premultiply(*p));
    Some(Image { w: w as u32, h: rows as u32, px })
}

/// The 8-bit channel `mask` picks out of `p`.
fn channel(p: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let v = (p & mask) >> mask.trailing_zeros();
    let max = mask >> mask.trailing_zeros();
    (v * 255 / max) as u8
}

fn premultiply(p: u32) -> u32 {
    let a = p >> 24;
    if a == 255 {
        return p;
    }
    let scale = |shift: u32| ((((p >> shift) & 0xFF) * a + 127) / 255) << shift;
    (a << 24) | scale(16) | scale(8) | scale(0)
}

/// The paths in CF_HDROP data: a DROPFILES header, then NUL-ended names (wide or ANSI) and one more NUL.
pub fn hdrop_paths(b: &[u8]) -> Vec<PathBuf> {
    let (Some(start), Some(wide)) = (u32_at(b, 0), u32_at(b, 16)) else { return Vec::new() };
    let Some(names) = b.get(start as usize..) else { return Vec::new() };
    let text = if wide != 0 {
        let units: Vec<u16> = names.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        names.iter().map(|&c| c as char).collect()
    };
    text.split('\0').take_while(|s| !s.is_empty()).map(PathBuf::from).collect()
}

/// Whether Windows keeps a clipboard history (Win+V); it's off until the user turns it on.
pub fn history_enabled() -> bool {
    Clipboard::IsHistoryEnabled().unwrap_or(false)
}

/// The pictures in the clipboard history, newest first, as the bytes of their files (PNG or BMP);
/// at most `max`. Blocks on WinRT calls: run it off the UI thread, in a COM apartment.
pub fn history_images(max: usize) -> Vec<Vec<u8>> {
    fn read(max: usize) -> windows::core::Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        if !Clipboard::IsHistoryEnabled()? {
            return Ok(out);
        }
        let result = Clipboard::GetHistoryItemsAsync()?.join()?;
        if result.Status()? != ClipboardHistoryItemsResultStatus::Success {
            return Ok(out);
        }
        let bitmap = StandardDataFormats::Bitmap()?;
        let items = result.Items()?;
        for i in 0..items.Size()? {
            if out.len() >= max {
                break;
            }
            let content = items.GetAt(i)?.Content()?;
            if !content.Contains(&bitmap)? {
                continue;
            }
            let stream = content.GetBitmapAsync()?.join()?.OpenReadAsync()?.join()?;
            let len = stream.Size()?.min(u32::MAX as u64) as u32;
            let reader = DataReader::CreateDataReader(&stream)?;
            let loaded = reader.LoadAsync(len)?.join()?;
            let mut bytes = vec![0; loaded as usize];
            reader.ReadBytes(&mut bytes)?;
            out.push(bytes);
        }
        Ok(out)
    }
    read(max).unwrap_or_else(|e| {
        glint_core::log::log(&format!("clipboard history: {e}"));
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use glint_media::image::{ImageFormat, encode};

    fn header(size: u32, w: i32, h: i32, bits: u16, compression: u32) -> Vec<u8> {
        let mut b = vec![0u8; size as usize];
        b[0..4].copy_from_slice(&size.to_le_bytes());
        b[4..8].copy_from_slice(&w.to_le_bytes());
        b[8..12].copy_from_slice(&h.to_le_bytes());
        b[12..14].copy_from_slice(&1u16.to_le_bytes());
        b[14..16].copy_from_slice(&bits.to_le_bytes());
        b[16..20].copy_from_slice(&compression.to_le_bytes());
        b
    }

    #[test]
    fn dibs_of_every_common_kind_read_right_side_up() {
        // 2x2, bottom-up: the bottom row comes first
        let mut v5 = header(124, 2, 2, 32, BI_BITFIELDS);
        for (at, mask) in [(40, 0x00FF_0000u32), (44, 0x0000_FF00), (48, 0x0000_00FF), (52, 0xFF00_0000)] {
            v5[at..at + 4].copy_from_slice(&mask.to_le_bytes());
        }
        for p in [0xFF00_00FFu32, 0x8000_FF00, 0xFFFF_0000, 0x0012_3456] {
            v5.extend(p.to_le_bytes());
        }
        let img = from_dib(&v5).unwrap();
        assert_eq!((img.w, img.h), (2, 2));
        assert_eq!(img.px, [0xFFFF_0000, 0, 0xFF00_00FF, 0x8000_8000], "flipped, premultiplied, transparent emptied");

        // 24-bit rows are padded to 4 bytes; top-down when the height is negative
        let mut rgb = header(40, 3, -1, 24, BI_RGB);
        rgb.extend([1, 2, 3, 4, 5, 6, 7, 8, 9, 0, 0, 0]);
        assert_eq!(from_dib(&rgb).unwrap().px, [0xFF03_0201, 0xFF06_0504, 0xFF09_0807]);

        // a plain 32-bit DIB with an empty alpha byte is opaque
        let mut plain = header(40, 1, 1, 32, BI_RGB);
        plain.extend(0x0011_2233u32.to_le_bytes());
        assert_eq!(from_dib(&plain).unwrap().px, [0xFF11_2233]);

        // a v3 header with BI_BITFIELDS has its three masks after it
        let mut masked = header(40, 1, 1, 32, BI_BITFIELDS);
        for m in [0x0000_00FFu32, 0x0000_FF00, 0x00FF_0000] {
            masked.extend(m.to_le_bytes());
        }
        masked.extend(0x0011_2233u32.to_le_bytes());
        assert_eq!(from_dib(&masked).unwrap().px, [0xFF33_2211], "red and blue swapped by the masks");

        assert_eq!(from_dib(&header(40, 4, 4, 8, BI_RGB)), None, "palettes aren't pictures we read");
        assert_eq!(from_dib(&v5[..130]), None, "truncated");
    }

    #[test]
    fn the_best_format_wins_and_files_are_read() {
        let png = encode(&[0xFF10_2030; 6], 3, 2, false, ImageFormat::Png, 100).unwrap();
        let mut dib = header(40, 1, 1, 32, BI_RGB);
        dib.extend(0x00AB_CDEFu32.to_le_bytes());
        let both = |f: Format| match f {
            Format::Png => Some(png.clone()),
            Format::Dib => Some(dib.clone()),
            _ => None,
        };
        let img = image_from(both).unwrap();
        assert_eq!((img.w, img.h, img.px[0]), (3, 2, 0xFF10_2030), "PNG beats the DIB");
        let dib_only = |f: Format| (f == Format::Dib).then(|| dib.clone());
        assert_eq!(image_from(dib_only).unwrap().px, [0xFFAB_CDEF]);
        assert_eq!(image_from(|f| (f == Format::Png).then(|| b"not a png".to_vec())), None);

        let dir = std::env::temp_dir().join(format!("glint-paste-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (text, pic) = (dir.join("notes.txt"), dir.join("pic.png"));
        std::fs::write(&text, "hello").unwrap();
        std::fs::write(&pic, &png).unwrap();
        let mut drop = vec![0u8; 20];
        drop[0..4].copy_from_slice(&20u32.to_le_bytes());
        drop[16..20].copy_from_slice(&1u32.to_le_bytes());
        let names = format!("{}\0{}\0\0", text.display(), pic.display());
        drop.extend(names.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(hdrop_paths(&drop), [text.clone(), pic.clone()]);
        let files = |f: Format| (f == Format::Files).then(|| drop.clone());
        assert_eq!(image_from(files).unwrap().w, 3, "the first file that's a picture");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
