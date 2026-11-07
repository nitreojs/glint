use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
    FOLDERID_Documents, FOLDERID_Videos, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, FileOperation,
    IFileOpenDialog, IFileOperation, ILCreateFromPathW, ILFree, IShellItem, KF_FLAG_DEFAULT,
    SHCreateItemFromParsingName, SHGetKnownFolderPath, SHOpenFolderAndSelectItems, SIGDN_FILESYSPATH,
};
use windows::core::{GUID, HSTRING};

/// One of the user's known folders (Documents, Videos…).
pub fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as _));
        (!s.is_empty()).then(|| PathBuf::from(s))
    }
}

/// The user's Documents folder.
pub fn documents() -> Option<PathBuf> {
    known_folder(&FOLDERID_Documents)
}

/// The user's Videos folder.
pub fn videos() -> Option<PathBuf> {
    known_folder(&FOLDERID_Videos)
}

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

/// Moves `path` to the Recycle Bin, without falling back to permanent deletion.
pub fn recycle(owner: HWND, path: &Path) -> Result<(), String> {
    // Shell parsing rejects the verbatim prefix that fs::canonicalize adds on Windows.
    let path = std::path::absolute(path).map_err(|e| e.to_string())?;
    unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| -> windows::core::Result<bool> {
            let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
            op.SetOwnerWindow(owner)?;
            op.SetOperationFlags(FOFX_RECYCLEONDELETE | FOFX_EARLYFAILURE | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT)?;
            let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)?;
            op.DeleteItem(&item, None)?;
            op.PerformOperations()?;
            Ok(op.GetAnyOperationsAborted()?.as_bool())
        })();
        if com {
            CoUninitialize();
        }
        match result {
            Ok(false) => Ok(()),
            Ok(true) => Err("the recycling was cancelled".into()),
            Err(e) => Err(format!("windows couldn't move it to the recycle bin: {e}")),
        }
    }
}
