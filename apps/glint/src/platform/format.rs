use glint_core::history::Local;
use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

/// Local date and time as (`YYYY-MM-DD`, `HH-MM-SS`).
pub(crate) fn date_time() -> (String, String) {
    let t = unsafe { GetLocalTime() };
    (format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay), format!("{:02}-{:02}-{:02}", t.wHour, t.wMinute, t.wSecond))
}

/// Local time as `YYYY-MM-DD_HH-MM-SS`.
pub(crate) fn timestamp() -> String {
    let (date, time) = date_time();
    format!("{date}_{time}")
}

pub(crate) fn local_time(unix: u64) -> Option<Local> {
    // filetime counts 100 ns ticks since 1601-01-01
    let ticks = (unix + 11_644_473_600) * 10_000_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    Some(Local { year: local.wYear, month: local.wMonth, day: local.wDay, hour: local.wHour, minute: local.wMinute })
}

#[cfg(test)]
mod tests {
    use super::*;
    use glint_core::history::month;

    #[test]
    fn local_time_converts() {
        let t = local_time(1_700_000_000).unwrap();
        assert_eq!((t.year, t.month), (2023, 11));
        assert_eq!(month(11), "Nov");
    }
}
