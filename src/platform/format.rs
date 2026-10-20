use windows::Win32::System::SystemInformation::GetLocalTime;

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

pub(crate) fn human_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= 1024.0 * MB {
        format!("{:.2} GB", b / (1024.0 * MB))
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else {
        format!("{:.0} KB", b / 1024.0)
    }
}

pub(crate) fn human_duration(secs: f64) -> String {
    let s = secs.round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
