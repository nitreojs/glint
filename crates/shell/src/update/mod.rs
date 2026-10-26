//! Updates from GitHub releases, for each of our apps. A check reads the release list, picks the
//! app's newest signed release above this build, downloads its exe into `%APPDATA%\<app>\update`
//! and keeps it only if it matches SHA256SUMS and carries a minisign signature from the release
//! key made for exactly that release. [`Build::install`] swaps it in; [`Host`] is the tray app's
//! side: when to check, what to say, when to restart.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use glint_core::log::log;
use glint_core::paths::app_dir_for;
use glint_core::update::Product;
use glint_core::update::release::{self, Asset, Candidate, SUMS};
use glint_core::update::status::{STATUS, State, Status};
use glint_core::update::version::Version;
use glint_core::upload::http::{Request, Response};
use glint_core::upload::proxy::Setting;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::net::http;
use crate::time::date_time;

mod agent;
mod apply;
#[cfg(test)]
mod tests;
mod verify;

pub use agent::{CHECK_TIMER_ID, Click, FIRST_CHECK_MS, Host, IDLE_TIMER_ID, Updates};

/// The public half of the release signing key (minisign, ed25519, key id 7EC36F307DC07BB7); the
/// release workflows of every app sign with it.
pub const PUBLIC_KEY: &str = "RWS3e8B9MG/DfriGVzlxJqGcJ1GUR6vEqX8+9nW2keufnCNTmzznWq0I";
const RELEASES_API: &str = "https://api.github.com/repos/nitreojs/glint/releases?per_page=100";
/// Release list pages read per check; the apps' releases share the list.
const MAX_PAGES: usize = 5;
/// Passed to the new exe after a swap, with the version it replaced.
pub const UPDATED_ARG: &str = "--updated";
/// A finished check: a `Box<Checked>` in LPARAM.
pub const WM_UPDATE_DONE: u32 = WM_APP + 60;
/// The settings window's "check now", to the tray app; WPARAM holds [`CHECK_DOWNLOAD`] /
/// [`CHECK_BETA`] bits.
pub const WM_UPDATE_CHECK: u32 = WM_APP + 24;
/// The settings window asks for the update now (downloaded first if it's only known).
pub const WM_UPDATE_INSTALL: u32 = WM_APP + 25;
/// WPARAM bits of [`WM_UPDATE_CHECK`].
pub const CHECK_DOWNLOAD: usize = 1;
pub const CHECK_BETA: usize = 2;
const MAX_EXE: u64 = 256 * 1024 * 1024;
/// How often a download's progress is written for the settings window, at most.
const PROGRESS_EVERY: Duration = Duration::from_millis(150);

/// The running build of one of our apps: what it updates, and from which version.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Build {
    pub product: Product,
    /// The app's Cargo version, `env!("CARGO_PKG_VERSION")`.
    pub version: &'static str,
}

/// Where releases come from and who must have signed them.
pub struct Source {
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
pub struct Staged {
    pub version: Version,
    pub tag: String,
    pub exe: PathBuf,
}

pub struct Job {
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

pub enum Found {
    UpToDate,
    Available(Candidate),
    Ready(Staged),
}

pub struct Checked {
    pub manual: bool,
    pub install: bool,
    pub result: Result<Found, String>,
}

impl Build {
    /// The version this build was made from.
    pub fn current(&self) -> Version {
        Version::parse(self.version).expect("Cargo.toml has a semver version")
    }

    /// `%APPDATA%\<app>\update`.
    pub fn dir(&self) -> PathBuf {
        app_dir_for(self.product.name).join("update")
    }

    /// The tray app's update status, for the settings window.
    pub fn status_path(&self) -> PathBuf {
        self.dir().join(STATUS)
    }

    pub fn status(&self) -> Status {
        Status::load_from(&self.status_path())
    }

    fn save_status(&self, status: &Status) {
        if let Err(e) = status.save_to(&self.status_path()) {
            log(&e);
        }
    }

    /// Records `state`, stamping the time when it ends a check.
    pub fn set_status(&self, state: State) {
        let mut status = self.status();
        if !state.busy() {
            let (date, time) = date_time();
            status.checked = Some(format!("{date} {}", time.get(..5).unwrap_or(&time).replace('-', ":")));
        }
        status.state = state;
        self.save_status(&status);
    }

    /// The update downloaded for this build, verified again.
    pub fn staged(&self) -> Option<Staged> {
        staged_in(&self.product, &self.dir(), PUBLIC_KEY, &self.current())
    }

    /// Start-up housekeeping: old exes left by an update, downloads for this version or older,
    /// and a status left by an earlier run that no longer holds. `updated` (this start follows a
    /// swap) waits for the previous process to let go of its exe.
    pub fn tidy(&self, updated: bool) {
        if let Ok(exe) = std::env::current_exe() {
            apply::remove_asides(&exe, updated);
        }
        let dir = self.dir();
        if updated {
            clear(&dir);
        }
        let status = self.status();
        let ready = |v: &str| dir.join(self.product.asset_name(&format!("{}{v}", self.product.tag_prefix))).exists();
        if status.stale(&self.current(), ready) {
            self.save_status(&Status { state: State::Unknown, ..status });
        }
    }

    /// Verifies the staged update once more, swaps it in for the running exe and starts it with
    /// `args`. On success this process should exit at once.
    pub fn install(&self, update: &Staged, args: &[String]) -> Result<(), String> {
        let fresh = self
            .staged()
            .filter(|s| s.version == update.version)
            .ok_or("the downloaded update is gone or damaged; it downloads again at the next check")?;
        let name = self.product.name;
        let exe = std::env::current_exe().map_err(|e| format!("can't find {name}.exe: {e}"))?;
        apply::swap(name, &exe, &fresh.exe, args)?;
        log(&format!("installed {} over {}, restarting", fresh.tag, exe.display()));
        Ok(())
    }

    /// At start, before anything else: a downloaded update is swapped in and started with the
    /// arguments that tell it so (and keep `extra`). None when there's nothing to install;
    /// `Some(Ok)` means this process should exit at once.
    pub fn install_at_start(&self, extra: &[&str]) -> Option<Result<(), String>> {
        let staged = self.staged()?;
        let mut args = vec![UPDATED_ARG.to_string(), self.current().to_string()];
        args.extend(extra.iter().map(|a| a.to_string()));
        log(&format!("installing the downloaded {} at start", staged.tag));
        let result = self.install(&staged, &args);
        if let Err(e) = &result {
            log(&format!("update to {} failed: {e}", staged.tag));
        }
        Some(result)
    }

    /// Runs `job` on its own thread and posts [`WM_UPDATE_DONE`] to `notify`. A download's
    /// progress goes to the status file as it arrives.
    pub fn start(self, job: Job, notify: HWND) -> Result<(), String> {
        let hwnd = notify.0 as isize;
        std::thread::Builder::new()
            .name("update".into())
            .spawn(move || {
                let mut last: Option<(u8, Instant)> = None;
                let mut progress = |version: &Version, percent: u8| {
                    let due = last.is_none_or(|(p, at)| p != percent && (percent == 100 || at.elapsed() >= PROGRESS_EVERY));
                    if due {
                        last = Some((percent, Instant::now()));
                        self.set_status(State::Downloading { version: version.to_string(), percent });
                    }
                };
                let result = run(&self.product, &job, &Source::github(), &self.current(), &self.dir(), &mut progress);
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
}

/// The version a run started after an update replaced (`--updated <version>`).
pub fn updated_from(args: &[String]) -> Option<String> {
    args.iter().position(|a| a == UPDATED_ARG).and_then(|i| args.get(i + 1)).cloned()
}

fn get(url: &str, accept: &str) -> Request {
    Request { method: "GET", url: url.into(), headers: vec![("Accept".into(), accept.into())], body: Vec::new() }
}

fn refused(app: &Product, what: &str, r: &Response) -> String {
    match r.status {
        403 | 429 if r.header("x-ratelimit-remaining") == Some("0") => {
            format!("github's limit on update checks from this network was reached; {} tries again later", app.name)
        }
        404 => format!("{what} wasn't found on github (404)"),
        s => format!("github answered {s} for {what}"),
    }
}

/// `app`'s newest release above `current` (pre-releases too with `beta`), if there is one.
pub fn latest(app: &Product, src: &Source, current: &Version, beta: bool, proxy: &Setting) -> Result<Option<Candidate>, String> {
    let mut releases = Vec::new();
    let mut url = Some(src.api.clone());
    for _ in 0..MAX_PAGES {
        let Some(page) = url.take() else { break };
        let mut req = get(&page, "application/vnd.github+json");
        req.headers.push(("X-GitHub-Api-Version".into(), "2022-11-28".into()));
        let r = http::send(&req, proxy, &mut |_, _| {})?;
        if !r.ok() {
            return Err(refused(app, "the release list", &r));
        }
        releases.extend(release::parse(&r.text)?);
        url = release::next_page(r.header("link"));
    }
    Ok(release::pick(app, &releases, current, beta))
}

fn fetch_text(app: &Product, asset: &Asset, proxy: &Setting) -> Result<String, String> {
    let r = http::send(&get(&asset.browser_download_url, "application/octet-stream"), proxy, &mut |_, _| {})?;
    if r.ok() { Ok(r.text) } else { Err(refused(app, &asset.name, &r)) }
}

/// Downloads and verifies `c` into `dir`, telling `progress` the percentage that has arrived;
/// an already verified copy is reused.
pub fn download(
    app: &Product,
    src: &Source,
    c: &Candidate,
    current: &Version,
    proxy: &Setting,
    dir: &Path,
    progress: &mut dyn FnMut(&Version, u8),
) -> Result<Staged, String> {
    if let Some(s) = staged_in(app, dir, &src.key, current)
        && s.version == c.version
    {
        return Ok(s);
    }
    clear(dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    progress(&c.version, 0);
    let sig = fetch_text(app, &c.sig, proxy)?;
    let sums = fetch_text(app, &c.sums, proxy)?;
    let name = app.asset_name(&c.tag);
    let part = dir.join(format!("{name}.part"));
    let max = if c.exe.size > 0 { c.exe.size.min(MAX_EXE) } else { MAX_EXE };
    let size = c.exe.size;
    let mut arrived = |bytes: u64| {
        if let Some(percent) = (bytes * 100).checked_div(size) {
            progress(&c.version, percent.min(100) as u8);
        }
    };
    let r = http::download(&get(&c.exe.browser_download_url, "application/octet-stream"), proxy, &part, max, &mut arrived)?;
    if !r.ok() {
        return Err(refused(app, &name, &r));
    }
    let stored = |e: std::io::Error| format!("{}: {e}", dir.display());
    let exe = std::fs::read(&part).map_err(stored)?;
    if let Err(e) = verify::verify(app, &exe, &sums, &sig, &src.key, &c.tag, current) {
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
pub fn run(
    app: &Product,
    job: &Job,
    src: &Source,
    current: &Version,
    dir: &Path,
    progress: &mut dyn FnMut(&Version, u8),
) -> Result<Found, String> {
    let candidate = match &job.candidate {
        Some(c) => Some(c.clone()),
        None => latest(app, src, current, job.beta, &job.proxy)?,
    };
    match candidate {
        None => Ok(Found::UpToDate),
        Some(c) if job.download => download(app, src, &c, current, &job.proxy, dir, progress).map(Found::Ready),
        Some(c) => Ok(Found::Available(c)),
    }
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

/// `app`'s verified update waiting in `dir`, if any; stale, partial or broken downloads are
/// removed.
pub fn staged_in(app: &Product, dir: &Path, key: &str, current: &Version) -> Option<Staged> {
    let mut best: Option<Staged> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(".part") {
            let _ = std::fs::remove_file(e.path());
            continue;
        }
        let Some(tag) = app.asset_tag(&name) else { continue };
        let sig_path = dir.join(format!("{name}.minisig"));
        let read = || -> Result<(), String> {
            let failed = |e: std::io::Error| e.to_string();
            let exe = std::fs::read(e.path()).map_err(failed)?;
            let sig = std::fs::read_to_string(&sig_path).map_err(failed)?;
            let sums = std::fs::read_to_string(dir.join(SUMS)).map_err(failed)?;
            verify::verify(app, &exe, &sums, &sig, key, &tag, current)
        };
        match (read(), app.version(&tag)) {
            (Ok(()), Some(version)) if best.as_ref().is_none_or(|b| version > b.version) => {
                best = Some(Staged { version, tag, exe: e.path() });
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
