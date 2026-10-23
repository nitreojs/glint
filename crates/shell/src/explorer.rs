use std::path::Path;

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::{ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems};
use windows::core::HSTRING;

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
