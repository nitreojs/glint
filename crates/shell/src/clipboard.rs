//! Putting things on the clipboard: a picture (as PNG and DIB, so every app can paste it), text,
//! or files (as Explorer copies them).

use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Duration;

use glint_media::image::{ImageFormat, encode, on_black, straight};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Gdi::{BI_BITFIELDS, BI_RGB, BITMAPINFOHEADER, BITMAPV5HEADER, LCS_GM_IMAGES};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT, DROPEFFECT_COPY};
use windows::Win32::UI::Shell::DROPFILES;
use windows::core::{Error, w};

/// Puts the image on the clipboard as CF_DIBV5 (so every app can paste it) and as the registered
/// "PNG" format (what Telegram, browsers, Office prefer). `owner` may be the caller's window.
/// With `alpha` a CF_DIB composited onto black goes along for apps that can't read alpha.
pub fn copy_image(
    owner: Option<HWND>,
    pixels: &[u32],
    w: u32,
    h: u32,
    alpha: bool,
) -> Result<(), String> {
    // Everything slow happens before OpenClipboard so other apps are locked out only briefly.
    let png = encode(pixels, w, h, alpha, ImageFormat::Png, 100)?;
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
        bV5Compression: if alpha { BI_BITFIELDS } else { BI_RGB },
        bV5SizeImage: w * h * 4,
        bV5RedMask: 0x00FF_0000,
        bV5GreenMask: 0x0000_FF00,
        bV5BlueMask: 0x0000_00FF,
        bV5AlphaMask: if alpha { 0xFF00_0000 } else { 0 },
        bV5CSType: LCS_SRGB,
        bV5Intent: LCS_GM_IMAGES as u32,
        ..Default::default()
    };
    // BI_RGB ignores the 4th byte officially, but apps that read it as alpha must see opaque.
    let dib_mem = dib(&header, pixels, w, |p| if alpha { straight(p) } else { p | 0xFF00_0000 })?;
    let fallback = if alpha {
        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: h as i32,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: w * h * 4,
            ..Default::default()
        };
        Some(dib(&header, pixels, w, on_black)?)
    } else {
        None
    };

    let _clipboard = Clipboard::open(owner)?;
    unsafe { EmptyClipboard() }.map_err(|e| format!("EmptyClipboard: {e}"))?;
    png_mem.give_to_clipboard(png_format)?;
    dib_mem.give_to_clipboard(u32::from(CF_DIBV5.0))?;
    match fallback {
        Some(mem) => mem.give_to_clipboard(u32::from(CF_DIB.0)),
        None => Ok(()),
    }
}

pub fn copy_text(owner: Option<HWND>, text: &str) -> Result<(), String> {
    let utf16: Vec<u8> = text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
    let mem = Global::new(utf16.len(), |buf| buf.copy_from_slice(&utf16))?;
    let _clipboard = Clipboard::open(owner)?;
    unsafe { EmptyClipboard() }.map_err(|e| format!("EmptyClipboard: {e}"))?;
    mem.give_to_clipboard(u32::from(CF_UNICODETEXT.0))
}

pub fn copy_files(owner: Option<HWND>, paths: &[PathBuf]) -> Result<(), String> {
    let header = size_of::<DROPFILES>();
    let names: Vec<u8> = paths
        .iter()
        .flat_map(|p| p.as_os_str().encode_wide().chain([0]))
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    let files = DROPFILES { pFiles: header as u32, fWide: true.into(), ..Default::default() };
    let list = Global::new(header + names.len(), |buf| {
        let (head, body) = buf.split_at_mut(header);
        head.copy_from_slice(unsafe { std::slice::from_raw_parts((&files as *const DROPFILES).cast::<u8>(), header) });
        body.copy_from_slice(&names);
    })?;
    let effect = Global::new(4, |buf| buf.copy_from_slice(&DROPEFFECT_COPY.0.to_le_bytes()))?;
    let effect_format = unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) };
    let _clipboard = Clipboard::open(owner)?;
    unsafe { EmptyClipboard() }.map_err(|e| format!("EmptyClipboard: {e}"))?;
    list.give_to_clipboard(u32::from(CF_HDROP.0))?;
    if effect_format != 0 {
        effect.give_to_clipboard(effect_format)?;
    }
    Ok(())
}

/// A packed bottom-up DIB: `header`, then the rows, each pixel through `px`.
fn dib<H>(header: &H, pixels: &[u32], w: u32, px: impl Fn(u32) -> u32) -> Result<Global, String> {
    let header_len = size_of::<H>();
    Global::new(header_len + pixels.len() * 4, |buf| {
        let (head, body) = buf.split_at_mut(header_len);
        // SAFETY: the bitmap headers are plain repr(C) structs of integers, `head` is exactly their size.
        head.copy_from_slice(unsafe { std::slice::from_raw_parts((header as *const H).cast::<u8>(), header_len) });
        for (dst, src) in body.chunks_exact_mut(w as usize * 4).zip(pixels.chunks_exact(w as usize).rev()) {
            for (d, p) in dst.chunks_exact_mut(4).zip(src) {
                d.copy_from_slice(&px(*p).to_le_bytes());
            }
        }
    })
}

/// 'sRGB' (LCS_sRGB lives in UI_ColorSystem, not worth a crate feature for one constant).
const LCS_SRGB: u32 = u32::from_be_bytes(*b"sRGB");

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
        // GlobalUnlock reports "failure" with NO_ERROR once the lock count reaches zero, which is expected.
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
