//! Dragging files out of our windows into whatever takes dropped files: Explorer, browsers,
//! Telegram, Discord. The files go as CF_HDROP in an OLE data object of our own, with a picture
//! under the pointer, and only ever as a copy: a drop never moves or deletes the user's file.

use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::path::PathBuf;

use glint_core::log::log;
use windows::Win32::Foundation::{
    COLORREF, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC, DV_E_TYMED, E_INVALIDARG,
    E_NOTIMPL, E_OUTOFMEMORY, HANDLE, HGLOBAL, HWND, LPARAM, OLE_E_ADVISENOTSUPPORTED, POINT, S_OK, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, HBITMAP, ScreenToClient,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject,
    IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, IStream, STGMEDIUM, STGMEDIUM_0, STREAM_SEEK_SET, TYMED_HGLOBAL,
    TYMED_ISTREAM,
};
use windows::Win32::System::Memory::GMEM_MOVEABLE;
use windows::Win32::System::Ole::{
    CF_HDROP, CLIPBOARD_FORMAT, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, DoDragDrop, IDropSource, IDropSource_Impl,
    OleDuplicateData, OleInitialize, OleUninitialize, ReleaseStgMedium,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MK_RBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_LBUTTON};
use windows::Win32::UI::Shell::{CLSID_DragDropHelper, IDragSourceHelper, SHCreateStdEnumFmtEtc, SHDRAGIMAGE};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, PostMessageW, SM_CXDRAG, SM_CYDRAG, WM_LBUTTONUP, WM_MOUSEMOVE,
};
use windows::core::{BOOL, HRESULT, Ref, implement};

use crate::clipboard::{Global, copy_effect, file_list};

/// The picture that goes along under the pointer: straight-alpha BGRA rows, and the pixel in it
/// the pointer holds.
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
    pub hold: (i32, i32),
}

/// How far the pointer may go from where the button went down, in pixels each way, before it's a
/// drag and no longer a click (the system's setting).
pub fn threshold() -> (u32, u32) {
    let (cx, cy) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
    // a negative setting counts the same on both sides
    (cx.unsigned_abs(), cy.unsigned_abs())
}

/// Whether a press the pointer has since moved `dx`, `dy` pixels away from is a drag, given the
/// [`threshold`].
pub fn started(dx: f32, dy: f32, (cx, cy): (u32, u32)) -> bool {
    dx.abs() > cx as f32 || dy.abs() > cy as f32
}

/// Drags `paths` until they're dropped somewhere or the drag is called off (esc, the right
/// button); true when something took them. Nothing starts unless the left button is down.
///
/// OLE's modal loop runs on this thread meanwhile, so the caller's window doesn't paint until it
/// ends. The loop keeps the pointer's last moves and the button going up from `owner`, so it's
/// told where the pointer is and that the button is up afterwards.
pub fn files(owner: Option<HWND>, paths: &[PathBuf], image: Option<&Image>) -> Result<bool, String> {
    if paths.is_empty() || unsafe { GetKeyState(i32::from(VK_LBUTTON.0)) } >= 0 {
        return Ok(false);
    }
    let _ole = Ole::init()?;
    let data = data_object(paths)?;
    if let Some(image) = image
        && let Err(e) = attach(&data, image)
    {
        log(&format!("drag: no picture: {e}"));
    }
    let source: IDropSource = Source.into();
    let mut effect = DROPEFFECT_NONE;
    let done = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
    if let Some(h) = owner {
        settle(h);
    }
    match done {
        DRAGDROP_S_DROP => Ok(effect != DROPEFFECT_NONE),
        DRAGDROP_S_CANCEL => Ok(false),
        e => Err(format!("DoDragDrop: {}", e.message())),
    }
}

/// An OLE data object with `paths` as CF_HDROP and copy as the preferred effect, the way
/// Explorer offers copied files. It takes any other format set on it (a drag image's).
pub fn data_object(paths: &[PathBuf]) -> Result<IDataObject, String> {
    let mut held = vec![(CF_HDROP.0, Medium::Global(file_list(paths)?))];
    if let Some((format, mem)) = copy_effect()? {
        held.push((format as u16, Medium::Global(mem)));
    }
    Ok(Data { held: RefCell::new(held) }.into())
}

/// OLE on this thread for as long as it's held; a window's thread usually has it already.
struct Ole;

impl Ole {
    fn init() -> Result<Ole, String> {
        unsafe { OleInitialize(None) }.map(|()| Ole).map_err(|e| format!("OLE: {}", e.message()))
    }
}

impl Drop for Ole {
    fn drop(&mut self) {
        unsafe { OleUninitialize() };
    }
}

/// Tells `owner` where the pointer is now and that the left button is up.
fn settle(owner: HWND) {
    let mut at = POINT::default();
    unsafe {
        if GetCursorPos(&mut at).is_err() || !ScreenToClient(owner, &mut at).as_bool() {
            return;
        }
        let lp = LPARAM(((at.y as u16 as u32) << 16 | at.x as u16 as u32) as isize);
        let _ = PostMessageW(Some(owner), WM_MOUSEMOVE, WPARAM(0), lp);
        let _ = PostMessageW(Some(owner), WM_LBUTTONUP, WPARAM(0), lp);
    }
}

/// Puts `image` on `data` for the shell's drag image manager to show under the pointer.
fn attach(data: &IDataObject, image: &Image) -> Result<(), String> {
    let helper: IDragSourceHelper =
        unsafe { CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER) }.map_err(|e| e.message())?;
    let bitmap = premultiplied(image)?;
    let shdi = SHDRAGIMAGE {
        sizeDragImage: SIZE { cx: image.w as i32, cy: image.h as i32 },
        ptOffset: POINT { x: image.hold.0, y: image.hold.1 },
        hbmpDragImage: bitmap,
        // CLR_NONE: the alpha says what's see-through
        crColorKey: COLORREF(0xFFFF_FFFF),
    };
    // the helper owns the bitmap once it took it
    unsafe { helper.InitializeFromBitmap(&shdi, data) }.map_err(|e| {
        let _ = unsafe { DeleteObject(bitmap.into()) };
        e.message()
    })
}

/// `image` as a top-down 32-bit DIB section with premultiplied alpha, as layered windows take it.
fn premultiplied(image: &Image) -> Result<HBITMAP, String> {
    let len = image.w as usize * image.h as usize * 4;
    if len == 0 || image.bgra.len() != len {
        return Err(format!("a {}x{} picture of {} bytes", image.w, image.h, image.bgra.len()));
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: image.w as i32,
            biHeight: -(image.h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let bitmap = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) }.map_err(|e| e.message())?;
    if bits.is_null() {
        let _ = unsafe { DeleteObject(bitmap.into()) };
        return Err("CreateDIBSection gave no pixels".into());
    }
    // SAFETY: the section holds exactly w * h 32-bit pixels.
    let dst = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), len) };
    for (d, s) in dst.chunks_exact_mut(4).zip(image.bgra.chunks_exact(4)) {
        let a = u32::from(s[3]);
        for c in 0..3 {
            d[c] = ((u32::from(s[c]) * a + 127) / 255) as u8;
        }
        d[3] = s[3];
    }
    Ok(bitmap)
}

#[implement(IDataObject)]
struct Data {
    /// Each format it has, and what it's in.
    held: RefCell<Vec<(u16, Medium)>>,
}

/// Data in one format: global memory it owns, or a stream (the drag image manager keeps some of
/// its own in one).
enum Medium {
    Global(Global),
    Stream(IStream),
}

impl Medium {
    fn tymed(&self) -> u32 {
        match self {
            Medium::Global(_) => TYMED_HGLOBAL.0 as u32,
            Medium::Stream(_) => TYMED_ISTREAM.0 as u32,
        }
    }

    /// A copy of it in `format` for a caller that releases it.
    fn hand_out(&self, format: u16) -> windows::core::Result<STGMEDIUM> {
        let u = match self {
            Medium::Global(mem) => STGMEDIUM_0 { hGlobal: duplicate(mem.0, format)? },
            Medium::Stream(stream) => {
                // its own seek position, from the start
                let clone = unsafe { stream.Clone() }?;
                unsafe { clone.Seek(0, STREAM_SEEK_SET, None) }?;
                STGMEDIUM_0 { pstm: ManuallyDrop::new(Some(clone)) }
            }
        };
        Ok(STGMEDIUM { tymed: self.tymed(), u, pUnkForRelease: ManuallyDrop::new(None) })
    }
}

/// Where in `held` the asked-for format is.
fn lookup(held: &[(u16, Medium)], format: *const FORMATETC) -> Result<usize, HRESULT> {
    let format = unsafe { format.as_ref() }.ok_or(E_INVALIDARG)?;
    if format.dwAspect != DVASPECT_CONTENT.0 {
        return Err(DV_E_FORMATETC);
    }
    let at = held.iter().position(|(cf, _)| *cf == format.cfFormat).ok_or(DV_E_FORMATETC)?;
    if format.tymed & held[at].1.tymed() == 0 {
        return Err(DV_E_TYMED);
    }
    Ok(at)
}

/// A copy of `mem` the caller owns.
fn duplicate(mem: HGLOBAL, format: u16) -> windows::core::Result<HGLOBAL> {
    let copy = unsafe { OleDuplicateData(HANDLE(mem.0), CLIPBOARD_FORMAT(format), GMEM_MOVEABLE) };
    if copy.is_invalid() { Err(E_OUTOFMEMORY.into()) } else { Ok(HGLOBAL(copy.0)) }
}

impl IDataObject_Impl for Data_Impl {
    fn GetData(&self, format: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        let held = self.held.borrow();
        let (cf, medium) = &held[lookup(&held, format)?];
        medium.hand_out(*cf)
    }

    fn GetDataHere(&self, _format: *const FORMATETC, _medium: *mut STGMEDIUM) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, format: *const FORMATETC) -> HRESULT {
        lookup(&self.held.borrow(), format).map_or_else(|e| e, |_| S_OK)
    }

    fn GetCanonicalFormatEtc(&self, _format: *const FORMATETC, _out: *mut FORMATETC) -> HRESULT {
        E_NOTIMPL
    }

    fn SetData(&self, format: *const FORMATETC, medium: *const STGMEDIUM, release: BOOL) -> windows::core::Result<()> {
        let (Some(f), Some(m)) = (unsafe { format.as_ref() }, unsafe { medium.as_ref() }) else {
            return Err(E_INVALIDARG.into());
        };
        let kept = if m.tymed == TYMED_HGLOBAL.0 as u32 {
            Medium::Global(Global(duplicate(unsafe { m.u.hGlobal }, f.cfFormat)?))
        } else if m.tymed == TYMED_ISTREAM.0 as u32 {
            let stream: Option<IStream> = unsafe { (*m.u.pstm).clone() };
            Medium::Stream(stream.ok_or(E_INVALIDARG)?)
        } else {
            return Err(DV_E_TYMED.into());
        };
        if release.as_bool() {
            unsafe { ReleaseStgMedium(medium.cast_mut()) };
        }
        let mut held = self.held.borrow_mut();
        held.retain(|(cf, _)| *cf != f.cfFormat);
        held.push((f.cfFormat, kept));
        Ok(())
    }

    fn EnumFormatEtc(&self, direction: u32) -> windows::core::Result<IEnumFORMATETC> {
        if direction != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        let formats: Vec<FORMATETC> = self
            .held
            .borrow()
            .iter()
            .map(|(cf, medium)| FORMATETC {
                cfFormat: *cf,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: medium.tymed(),
            })
            .collect();
        unsafe { SHCreateStdEnumFmtEtc(&formats) }
    }

    fn DAdvise(&self, _format: *const FORMATETC, _advf: u32, _sink: Ref<IAdviseSink>) -> windows::core::Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _connection: u32) -> windows::core::Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

/// What tells OLE's loop when the drag ends: the left button let go drops, esc or the right
/// button calls it off.
#[implement(IDropSource)]
struct Source;

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() || keys.0 & MK_RBUTTON.0 != 0 {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::Foundation::{CLIPBRD_E_CANT_OPEN, E_ACCESSDENIED};
    use windows::Win32::System::Com::IEnumFORMATETC;
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard, RegisterClipboardFormatW};
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::{OleFlushClipboard, OleGetClipboard, OleSetClipboard};
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    use windows::core::w;

    use super::*;

    fn asking(cf: u16) -> FORMATETC {
        FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
    }

    /// The files a CF_HDROP block lists.
    fn listed(drop: HGLOBAL) -> Vec<PathBuf> {
        let drop = HDROP(drop.0);
        let n = unsafe { DragQueryFileW(drop, u32::MAX, None) };
        (0..n)
            .map(|i| {
                let mut buf = vec![0u16; unsafe { DragQueryFileW(drop, i, None) } as usize + 1];
                let len = unsafe { DragQueryFileW(drop, i, Some(&mut buf)) } as usize;
                PathBuf::from(String::from_utf16_lossy(&buf[..len]))
            })
            .collect()
    }

    /// The files `data` gives as CF_HDROP.
    fn dropped(data: &IDataObject) -> Vec<PathBuf> {
        try_dropped(data).expect("CF_HDROP")
    }

    fn try_dropped(data: &IDataObject) -> windows::core::Result<Vec<PathBuf>> {
        let mut medium = unsafe { data.GetData(&asking(CF_HDROP.0)) }?;
        assert_eq!(medium.tymed, TYMED_HGLOBAL.0 as u32);
        let files = listed(unsafe { medium.u.hGlobal });
        unsafe { ReleaseStgMedium(&mut medium) };
        Ok(files)
    }

    fn formats(data: &IDataObject) -> Vec<u16> {
        let list: IEnumFORMATETC = unsafe { data.EnumFormatEtc(DATADIR_GET.0 as u32) }.expect("formats");
        let mut out = Vec::new();
        loop {
            let mut f = [FORMATETC::default()];
            let mut n = 0;
            if unsafe { list.Next(&mut f, Some(&mut n)) } != S_OK || n == 0 {
                return out;
            }
            out.push(f[0].cfFormat);
        }
    }

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("glint-drag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, b"not really a picture").unwrap();
        path
    }

    #[test]
    fn a_drag_starts_once_the_pointer_leaves_the_box_around_the_press() {
        let box_ = (4, 4);
        assert!(!started(0.0, 0.0, box_));
        assert!(!started(4.0, -4.0, box_));
        assert!(started(4.5, 0.0, box_));
        assert!(started(0.0, -5.0, box_));
        assert!(started(-30.0, 2.0, (4, 6)));
        assert!(!started(3.0, 6.0, (4, 6)));
        let (cx, cy) = threshold();
        assert!(cx > 0 && cy > 0, "the system's drag box is {cx}x{cy}");
    }

    #[test]
    fn the_data_object_offers_the_files_as_hdrop_and_asks_for_a_copy() {
        let _ole = Ole::init().unwrap();
        let (a, b) = (temp_file("a shot.png"), temp_file("клип.mp4"));
        let data = data_object(&[a.clone(), b.clone()]).unwrap();
        assert_eq!(unsafe { data.QueryGetData(&asking(CF_HDROP.0)) }, S_OK);
        assert_eq!(dropped(&data), vec![a.clone(), b]);
        // asked again, it hands out another copy
        assert_eq!(dropped(&data).len(), 2);

        let effect = unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) } as u16;
        let mut medium = unsafe { data.GetData(&asking(effect)) }.expect("Preferred DropEffect");
        let mem = unsafe { medium.u.hGlobal };
        let value = unsafe { *GlobalLock(mem).cast::<u32>() };
        let _ = unsafe { GlobalUnlock(mem) };
        unsafe { ReleaseStgMedium(&mut medium) };
        assert_eq!(value, DROPEFFECT_COPY.0);

        assert_eq!(formats(&data), vec![CF_HDROP.0, effect]);
        assert_eq!(unsafe { data.QueryGetData(&asking(1)) }, DV_E_FORMATETC); // CF_TEXT
        let stream = FORMATETC { tymed: 4, ..asking(CF_HDROP.0) }; // TYMED_ISTREAM
        assert_eq!(unsafe { data.QueryGetData(&stream) }, DV_E_TYMED);
        let _ = std::fs::remove_file(&a);
    }

    #[test]
    fn the_drag_image_goes_onto_the_data_object() {
        let _ole = Ole::init().unwrap();
        let data = data_object(&[temp_file("pic.png")]).unwrap();
        let before = formats(&data).len();
        let (w, h) = (64, 40);
        let bgra = (0..w * h).flat_map(|i| [(i % 255) as u8, 80, 200, if i % 7 == 0 { 0 } else { 255 }]).collect();
        attach(&data, &Image { w, h, bgra, hold: (32, 20) }).unwrap();
        let after = formats(&data);
        assert!(after.len() > before, "the drag image manager set nothing: {after:?}");
        // it's still the files underneath
        assert_eq!(dropped(&data).len(), 1);
        // a picture of the wrong size isn't taken
        assert!(attach(&data, &Image { w: 3, h: 3, bgra: vec![0; 8], hold: (0, 0) }).is_err());
    }

    /// `f` again while another app (a clipboard manager, the clipboard history) holds the
    /// clipboard for a moment.
    fn patiently<T>(mut f: impl FnMut() -> windows::core::Result<T>) -> windows::core::Result<T> {
        for _ in 0..80 {
            match f() {
                Err(e) if e.code() == CLIPBRD_E_CANT_OPEN || e.code() == E_ACCESSDENIED => {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                done => return done,
            }
        }
        f()
    }

    /// Everything on the clipboard now that's in global memory, copied into a data object of ours;
    /// an error when any of it can't be read, so nothing is replaced that can't be put back.
    fn snapshot() -> windows::core::Result<IDataObject> {
        let copy: IDataObject = Data { held: RefCell::new(Vec::new()) }.into();
        let clip = patiently(|| unsafe { OleGetClipboard() })?;
        let list = patiently(|| unsafe { clip.EnumFormatEtc(DATADIR_GET.0 as u32) })?;
        loop {
            let mut f = [FORMATETC::default()];
            let mut n = 0;
            if unsafe { list.Next(&mut f, Some(&mut n)) } != S_OK || n == 0 {
                return Ok(copy);
            }
            if f[0].tymed & TYMED_HGLOBAL.0 as u32 == 0 {
                continue;
            }
            let f = FORMATETC { tymed: TYMED_HGLOBAL.0 as u32, ..f[0] };
            let mut m = patiently(|| unsafe { clip.GetData(&f) })?;
            if unsafe { copy.SetData(&f, &m, true) }.is_err() {
                unsafe { ReleaseStgMedium(&mut m) };
            }
        }
    }

    /// The real clipboard (what any app pastes) with the drag's data object on it: run on its own
    /// (`--ignored`), it puts back what was there before checking anything.
    #[test]
    #[ignore = "uses the desktop's clipboard"]
    fn what_the_drag_offers_reads_back_through_the_clipboard() {
        let _ole = Ole::init().unwrap();
        let path = temp_file("from the gallery.png");
        let saved = snapshot().expect("the clipboard as it is, to put back");
        let data = data_object(std::slice::from_ref(&path)).unwrap();
        let set = patiently(|| unsafe { OleSetClipboard(&data) });
        let through_ole = patiently(|| unsafe { OleGetClipboard() }.and_then(|clip| try_dropped(&clip)));
        // as an app that doesn't know OLE reads it
        let through_win32 = patiently(|| unsafe {
            OpenClipboard(None)?;
            let files = GetClipboardData(u32::from(CF_HDROP.0)).map(|h| listed(HGLOBAL(h.0)));
            let _ = CloseClipboard();
            files
        });
        let restored = patiently(|| unsafe { OleSetClipboard(&saved) }).and_then(|()| patiently(|| unsafe { OleFlushClipboard() }));
        restored.expect("the clipboard put back");
        set.expect("OleSetClipboard");
        assert_eq!(through_ole.unwrap(), vec![path.clone()]);
        assert_eq!(through_win32.unwrap(), vec![path]);
    }
}
