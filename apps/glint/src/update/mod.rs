//! Updates from GitHub releases. A check reads the release list, picks the newest signed
//! release above this build, downloads its exe into `%APPDATA%\glint\update` and keeps it only
//! if it matches SHA256SUMS and carries a minisign signature from glint's release key made for
//! exactly that release. [`apply`] swaps it in.

use std::path::{Path, PathBuf};

use glint_core::log::log;
use glint_core::paths::app_dir;
use glint_core::update::release::{self, Asset, Candidate, SUMS, asset_name};
use glint_core::update::version::Version;
use glint_core::upload::http::{Request, Response};
use glint_core::upload::proxy::Setting;
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::upload::http;

pub(crate) mod apply;
mod verify;

/// The public half of glint's release signing key (minisign, ed25519, key id 7EC36F307DC07BB7).
pub(crate) const PUBLIC_KEY: &str = "RWS3e8B9MG/DfriGVzlxJqGcJ1GUR6vEqX8+9nW2keufnCNTmzznWq0I";
const RELEASES_API: &str = "https://api.github.com/repos/nitreojs/glint/releases?per_page=100";
/// Release list pages read per check; other apps' releases share the list.
const MAX_PAGES: usize = 5;
/// Passed to the new exe after a swap, with the version it replaced.
pub(crate) const UPDATED_ARG: &str = "--updated";
/// A finished check: a `Box<Checked>` in LPARAM.
pub(crate) const WM_UPDATE_DONE: u32 = WM_APP + 60;
const MAX_EXE: u64 = 256 * 1024 * 1024;
const STATUS: &str = "status.json";

/// The version this build was made from.
pub(crate) fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml has a semver version")
}

/// `%APPDATA%\glint\update`.
pub(crate) fn dir() -> PathBuf {
    app_dir().join("update")
}

/// Where releases come from and who must have signed them.
pub(crate) struct Source {
    pub api: String,
    pub key: String,
}

impl Source {
    fn github() -> Source {
        Source { api: RELEASES_API.into(), key: PUBLIC_KEY.into() }
    }
}

/// A verified update waiting in the download folder.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Staged {
    pub version: Version,
    pub tag: String,
    pub exe: PathBuf,
}

pub(crate) struct Job {
    pub beta: bool,
    /// Download a newer release (instead of only reporting it).
    pub download: bool,
    /// Someone asked: failures are shown, not only logged.
    pub manual: bool,
    /// Restart into the update once it's downloaded.
    pub install: bool,
    /// Already found by an earlier check; skips the release list.
    pub candidate: Option<Candidate>,
    pub proxy: Setting,
}

pub(crate) enum Found {
    UpToDate,
    Available(Candidate),
    Ready(Staged),
}

pub(crate) struct Checked {
    pub manual: bool,
    pub install: bool,
    pub result: Result<Found, String>,
}

/// What the tray app last learned, for the settings window (`status.json`).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct Status {
    /// Local time of the last finished check, `2026-10-21 14:32`.
    #[serde(default)]
    pub checked: Option<String>,
    #[serde(default)]
    pub state: State,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum State {
    #[default]
    Unknown,
    Checking,
    UpToDate,
    Available {
        version: String,
    },
    Downloading {
        version: String,
    },
    Ready {
        version: String,
    },
    Failed {
        error: String,
    },
}

impl Status {
    pub(crate) fn path() -> PathBuf {
        dir().join(STATUS)
    }

    pub(crate) fn load_from(path: &Path) -> Status {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub(crate) fn load() -> Status {
        Status::load_from(&Status::path())
    }

    pub(crate) fn save(&self) {
        let path = Status::path();
        let tmp = path.with_extension("json.tmp");
        let saved = std::fs::create_dir_all(dir())
            .and_then(|_| std::fs::write(&tmp, serde_json::to_string(self).unwrap_or_default()))
            .and_then(|_| std::fs::rename(&tmp, &path));
        if let Err(e) = saved {
            log(&format!("can't write {}: {e}", path.display()));
        }
    }

    /// Records `state`, stamping the time when it ends a check.
    pub(crate) fn set(state: State) {
        let mut status = Status::load();
        if !matches!(state, State::Checking | State::Downloading { .. }) {
            let t = unsafe { GetLocalTime() };
            status.checked =
                Some(format!("{:04}-{:02}-{:02} {:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute));
        }
        status.state = state;
        status.save();
    }
}

fn get(url: &str, accept: &str) -> Request {
    Request { method: "GET", url: url.into(), headers: vec![("Accept".into(), accept.into())], body: Vec::new() }
}

fn refused(what: &str, r: &Response) -> String {
    match r.status {
        403 | 429 if r.header("x-ratelimit-remaining") == Some("0") => {
            "github's limit on update checks from this network was reached; glint tries again later".into()
        }
        404 => format!("{what} wasn't found on github (404)"),
        s => format!("github answered {s} for {what}"),
    }
}

/// The newest release above `current` (pre-releases too with `beta`), if there is one.
pub(crate) fn latest(src: &Source, current: &Version, beta: bool, proxy: &Setting) -> Result<Option<Candidate>, String> {
    let mut releases = Vec::new();
    let mut url = Some(src.api.clone());
    for _ in 0..MAX_PAGES {
        let Some(page) = url.take() else { break };
        let mut req = get(&page, "application/vnd.github+json");
        req.headers.push(("X-GitHub-Api-Version".into(), "2022-11-28".into()));
        let r = http::send(&req, proxy, &mut |_, _| {})?;
        if !r.ok() {
            return Err(refused("the release list", &r));
        }
        releases.extend(release::parse(&r.text)?);
        url = release::next_page(r.header("link"));
    }
    Ok(release::pick(&releases, current, beta))
}

fn fetch_text(asset: &Asset, proxy: &Setting) -> Result<String, String> {
    let r = http::send(&get(&asset.browser_download_url, "application/octet-stream"), proxy, &mut |_, _| {})?;
    if r.ok() { Ok(r.text) } else { Err(refused(&asset.name, &r)) }
}

/// Downloads and verifies `c` into `dir`; an already verified copy is reused.
pub(crate) fn download(src: &Source, c: &Candidate, current: &Version, proxy: &Setting, dir: &Path) -> Result<Staged, String> {
    if let Some(s) = staged_in(dir, &src.key, current)
        && s.version == c.version
    {
        return Ok(s);
    }
    clear(dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let sig = fetch_text(&c.sig, proxy)?;
    let sums = fetch_text(&c.sums, proxy)?;
    let name = asset_name(&c.tag);
    let part = dir.join(format!("{name}.part"));
    let max = if c.exe.size > 0 { c.exe.size.min(MAX_EXE) } else { MAX_EXE };
    let r = http::download(&get(&c.exe.browser_download_url, "application/octet-stream"), proxy, &part, max)?;
    if !r.ok() {
        return Err(refused(&name, &r));
    }
    let stored = |e: std::io::Error| format!("{}: {e}", dir.display());
    let exe = std::fs::read(&part).map_err(stored)?;
    if let Err(e) = verify::verify(&exe, &sums, &sig, &src.key, &c.tag, current) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    let path = dir.join(&name);
    std::fs::write(dir.join(format!("{name}.minisig")), &sig)
        .and_then(|_| std::fs::write(dir.join(SUMS), &sums))
        .and_then(|_| std::fs::rename(&part, &path))
        .map_err(stored)?;
    Ok(Staged { version: c.version.clone(), tag: c.tag.clone(), exe: path })
}

/// Checks (and, for `job.download`, downloads) one update.
pub(crate) fn run(job: &Job, src: &Source, current: &Version, dir: &Path) -> Result<Found, String> {
    let candidate = match &job.candidate {
        Some(c) => Some(c.clone()),
        None => latest(src, current, job.beta, &job.proxy)?,
    };
    match candidate {
        None => Ok(Found::UpToDate),
        Some(c) if job.download => download(src, &c, current, &job.proxy, dir).map(Found::Ready),
        Some(c) => Ok(Found::Available(c)),
    }
}

/// Runs `job` on its own thread and posts [`WM_UPDATE_DONE`] to `notify`.
pub(crate) fn start(job: Job, notify: HWND) -> Result<(), String> {
    let hwnd = notify.0 as isize;
    std::thread::Builder::new()
        .name("update".into())
        .spawn(move || {
            let result = run(&job, &Source::github(), &current(), &dir());
            let done = Box::into_raw(Box::new(Checked { manual: job.manual, install: job.install, result }));
            unsafe {
                if PostMessageW(Some(HWND(hwnd as _)), WM_UPDATE_DONE, WPARAM(0), LPARAM(done as isize)).is_err() {
                    drop(Box::from_raw(done));
                }
            }
        })
        .map(|_| ())
        .map_err(|e| format!("can't start the update check: {e}"))
}

/// Removes everything downloaded (the status file stays).
fn clear(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if e.file_name() != STATUS {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The verified update waiting in `dir`, if any; stale, partial or broken downloads are removed.
pub(crate) fn staged_in(dir: &Path, key: &str, current: &Version) -> Option<Staged> {
    let mut best: Option<Staged> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(".part") {
            let _ = std::fs::remove_file(e.path());
            continue;
        }
        let Some(tag) = name.strip_prefix("glint-").and_then(|n| n.strip_suffix("-windows-x64.exe")) else { continue };
        let sig_path = dir.join(format!("{name}.minisig"));
        let read = || -> Result<(), String> {
            let failed = |e: std::io::Error| e.to_string();
            let exe = std::fs::read(e.path()).map_err(failed)?;
            let sig = std::fs::read_to_string(&sig_path).map_err(failed)?;
            let sums = std::fs::read_to_string(dir.join(SUMS)).map_err(failed)?;
            verify::verify(&exe, &sums, &sig, key, tag, current)
        };
        match (read(), Version::parse(tag)) {
            (Ok(()), Some(version)) if best.as_ref().is_none_or(|b| version > b.version) => {
                best = Some(Staged { version, tag: tag.into(), exe: e.path() });
            }
            (Ok(()), _) => {}
            (Err(why), _) => {
                log(&format!("discarding the downloaded {name}: {why}"));
                let _ = std::fs::remove_file(e.path());
                let _ = std::fs::remove_file(&sig_path);
            }
        }
    }
    best
}

/// The update downloaded for this build, verified again.
pub(crate) fn staged() -> Option<Staged> {
    staged_in(&dir(), PUBLIC_KEY, &current())
}

/// Start-up housekeeping: old exes left by an update, downloads for this version or older,
/// and a status left by an earlier run that no longer holds. `updated` (this start follows a
/// swap) waits for the previous process to let go of its exe.
pub(crate) fn tidy(updated: bool) {
    if let Ok(exe) = std::env::current_exe() {
        apply::remove_asides(&exe, updated);
    }
    if updated {
        clear(&dir());
    }
    let current = current();
    let newer = |v: &str| Version::parse(v).is_some_and(|v| v > current);
    let status = Status::load();
    let stale = match &status.state {
        State::Checking | State::Downloading { .. } => true,
        State::Available { version } => !newer(version),
        State::Ready { version } => !newer(version) || !dir().join(asset_name(&format!("v{version}"))).exists(),
        State::Unknown | State::UpToDate | State::Failed { .. } => false,
    };
    if stale {
        Status { state: State::Unknown, ..status }.save();
    }
}

/// Verifies the staged update once more, swaps it in for the running exe and starts it with
/// `args`. On success this process should exit at once.
pub(crate) fn install(update: &Staged, args: &[String]) -> Result<(), String> {
    let fresh = staged()
        .filter(|s| s.version == update.version)
        .ok_or("the downloaded update is gone or damaged; it downloads again at the next check")?;
    let exe = std::env::current_exe().map_err(|e| format!("can't find glint.exe: {e}"))?;
    apply::swap(&exe, &fresh.exe, args)?;
    log(&format!("installed {} over {}, restarting", fresh.tag, exe.display()));
    Ok(())
}
