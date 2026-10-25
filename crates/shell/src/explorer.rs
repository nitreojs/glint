use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS,
    FileOpenDialog, IFileOpenDialog, ILCreateFromPathW, ILFree, IShellItem, SHCreateItemFromParsingName,
    SHFILEOPSTRUCTW, SHFileOperationW, SHOpenFolderAndSelectItems, SIGDN_FILESYSPATH,
};
use windows::core::{HSTRING, PCWSTR};

/// The folder picked in Windows' folder dialog (opened at `start`, over `owner`); `None` when
/// cancelled. The calling thread needs COM.
pub fn pick_folder(start: &Path, owner: Option<HWND>) -> Option<PathBuf> {
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dialog.GetOptions().ok()?;
        dialog.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start.as_os_str()), None) {
            let _ = dialog.SetFolder(&item);
        }
        dialog.Show(owner).ok()?; // Err when cancelled
        let path = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as _));
        s.map(PathBuf::from)
    }
}

/// Opens an Explorer window on `path`'s folder with it selected (`explorer /select,` often only
/// focuses an already open window of that folder without selecting).
pub fn reveal(path: &Path) {
    let path = path.to_string_lossy().replace('/', "\\");
    unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let item = ILCreateFromPathW(&HSTRING::from(path.as_str()));
        let shown = !item.is_null() && SHOpenFolderAndSelectItems(item, None, 0).is_ok();
        if !item.is_null() {
            ILFree(Some(item));
        }
        if com {
            CoUninitialize();
        }
        if !shown && let Some(dir) = Path::new(&path).parent() {
            let _ = std::process::Command::new("explorer.exe").arg(dir).spawn();
        }
    }
}

/// Moves `path` to the Recycle Bin.
pub fn recycle(owner: HWND, path: &Path) -> Result<(), String> {
    let from: Vec<u16> = path.as_os_str().encode_wide().chain([0, 0]).collect();
    let mut op = SHFILEOPSTRUCTW {
        hwnd: owner,
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT).0 as u16,
        ..Default::default()
    };
    match unsafe { SHFileOperationW(&mut op) } {
        0 if !op.fAnyOperationsAborted.as_bool() => Ok(()),
        0 => Err("the deletion was cancelled".into()),
        code => Err(format!("windows couldn't move it to the recycle bin (error {code:#x})")),
    }
}
