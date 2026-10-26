use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use glint_core::config::Config;
use glint_core::history::Kind;
use glint_core::log::log;
use glint_core::upload::custom::Uploaded;
use glint_core::upload::{Destination, Payload};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use glint_shell::net::http;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub(crate) const WM_UPLOADED: u32 = WM_APP + 50;
pub(crate) const WM_UPLOAD_PROGRESS: u32 = WM_APP + 51;

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct Done {
    pub kind: Kind,
    pub path: Option<PathBuf>,
    pub size: u64,
    pub host: String,
    pub result: Result<String, String>,
}

pub(crate) fn upload(dest: &Destination, file: Payload) -> Result<Uploaded, String> {
    upload_with(dest, file, &mut |_, _| {})
}

fn upload_with(dest: &Destination, file: Payload, progress: &mut dyn FnMut(u64, u64)) -> Result<Uploaded, String> {
    let (name, request) = dest.request(file)?;
    let response = http::send(&request, dest.proxy(), progress)?;
    dest.response(&name, &response)
}

pub(crate) fn active() -> bool {
    ACTIVE.load(Ordering::SeqCst) > 0
}

pub(crate) fn start(
    cfg: &Config,
    kind: Kind,
    path: Option<PathBuf>,
    file: Payload,
    notify: HWND,
) -> Result<(), String> {
    let dest = Destination::from_config(cfg)?;
    dest.check_size(&file)?;
    let hwnd = notify.0 as isize;
    ACTIVE.fetch_add(1, Ordering::SeqCst);
    std::thread::Builder::new()
        .name("upload".into())
        .spawn(move || {
            let size = file.size;
            let mut shown = None;
            let mut progress = |sent: u64, total: u64| {
                let percent = (sent * 100).checked_div(total).unwrap_or(100);
                if shown != Some(percent) {
                    shown = Some(percent);
                    let _ = unsafe {
                        PostMessageW(Some(HWND(hwnd as _)), WM_UPLOAD_PROGRESS, WPARAM(percent as usize), LPARAM(0))
                    };
                }
            };
            let result = upload_with(&dest, file, &mut progress).map(|up| {
                if let Some(d) = &up.deletion_url {
                    log(&format!("uploaded {} to {}, delete with {d}", up.url, dest.name));
                }
                up.url
            });
            ACTIVE.fetch_sub(1, Ordering::SeqCst);
            let done = Box::into_raw(Box::new(Done { kind, path, size, host: dest.name, result }));
            unsafe {
                if PostMessageW(Some(HWND(hwnd as _)), WM_UPLOADED, WPARAM(0), LPARAM(done as isize)).is_err() {
                    drop(Box::from_raw(done));
                }
            }
        })
        .map(|_| ())
        .map_err(|e| {
            ACTIVE.fetch_sub(1, Ordering::SeqCst);
            format!("can't start the upload: {e}")
        })
}

pub(crate) fn start_file(cfg: &Config, kind: Kind, path: &Path, notify: HWND) -> Result<(), String> {
    start(cfg, kind, Some(path.to_path_buf()), Payload::file(path)?, notify)
}
