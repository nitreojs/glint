//! One running copy per app and user session.

use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::HSTRING;

/// Takes `app`'s single-instance mutex for the life of the process. Right after an update the
/// previous process is still exiting, so `wait` keeps trying for a while.
pub fn take(app: &str, wait: bool) -> bool {
    let name = HSTRING::from(format!("Local\\{app}-single-instance"));
    for _ in 0..200 {
        unsafe {
            let mutex = CreateMutexW(None, false, &name);
            if GetLastError() != ERROR_ALREADY_EXISTS {
                return true;
            }
            if let Ok(h) = mutex {
                let _ = CloseHandle(h);
            }
        }
        if !wait {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}
