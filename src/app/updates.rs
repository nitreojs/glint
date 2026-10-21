use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::{App, ClickTarget, State};
use crate::config::UpdateMode;
use crate::platform::log::log;
use crate::update::{self, Candidate, Checked, Found, Job, Staged, Status, UPDATED_ARG, Version};
use crate::upload::{self, proxy};

pub(super) const CHECK_TIMER_ID: usize = 9;
/// Retries what waits for glint to be idle (restarting, the "ready" notification).
pub(super) const IDLE_TIMER_ID: usize = 10;
/// The first check waits a little so start-up stays fast.
pub(super) const FIRST_CHECK_MS: u32 = 30_000;
const CHECK_EVERY_MS: u32 = 6 * 60 * 60 * 1000;
const IDLE_RETRY_MS: u32 = 2_000;

/// WPARAM bits of [`crate::app::ipc::WM_UPDATE_CHECK`].
pub(crate) const CHECK_DOWNLOAD: usize = 1;
pub(crate) const CHECK_BETA: usize = 2;

#[derive(Default)]
pub(super) struct Updates {
    checking: bool,
    ready: Option<Staged>,
    available: Option<Candidate>,
    /// Restart into the update as soon as glint is idle.
    install: bool,
    /// Show the "ready" / "available" notification once glint is idle.
    announce: bool,
    announced: Option<Version>,
}

impl App {
    pub(super) fn schedule_update_check(&self, ms: u32) {
        unsafe {
            SetTimer(Some(self.hwnd), CHECK_TIMER_ID, ms, None);
        }
    }

    pub(super) fn on_check_timer(&mut self) {
        self.schedule_update_check(CHECK_EVERY_MS);
        if self.cfg.update_mode == UpdateMode::Off {
            return;
        }
        let (download, beta) = (self.cfg.update_mode == UpdateMode::Auto, self.cfg.update_beta);
        self.start_update_job(download, beta, false, false);
    }

    /// "check now" in settings; `bits` are [`CHECK_DOWNLOAD`] and [`CHECK_BETA`].
    pub(super) fn check_now(&mut self, bits: usize) {
        self.update.available = None;
        self.start_update_job(bits & CHECK_DOWNLOAD != 0, bits & CHECK_BETA != 0, true, false);
    }

    fn start_update_job(&mut self, download: bool, beta: bool, manual: bool, install: bool) {
        if self.update.checking {
            self.update.install |= install;
            return;
        }
        let proxy = match proxy::parse(&self.cfg.upload_proxy) {
            Ok(p) => p,
            Err(e) => return self.on_checked(Checked { manual, install, result: Err(format!("upload proxy: {e}")) }),
        };
        let candidate = self.update.available.clone().filter(|_| download);
        let channel = if beta { "beta" } else { "stable" };
        match &candidate {
            Some(c) => log(&format!("downloading the update to {}", c.tag)),
            None => log(&format!("checking for updates ({channel}{})", if manual { ", asked" } else { "" })),
        }
        Status::set(match &candidate {
            Some(c) => update::State::Downloading { version: c.version.to_string() },
            None => update::State::Checking,
        });
        self.update.checking = true;
        let job = Job { beta, download, manual, install, candidate, proxy };
        if let Err(e) = update::start(job, self.hwnd) {
            self.on_checked(Checked { manual, install, result: Err(e) });
        }
    }

    pub(super) fn on_checked(&mut self, done: Checked) {
        self.update.checking = false;
        let install = done.install || std::mem::take(&mut self.update.install);
        let state = match done.result {
            Ok(Found::UpToDate) => {
                log(&format!("update check: {} is the newest", Version::current()));
                self.update.available = None;
                update::State::UpToDate
            }
            Ok(Found::Available(c)) => {
                log(&format!("update check: {} is available", c.tag));
                let version = c.version.clone();
                self.update.available = Some(c);
                if install {
                    return self.install_update();
                }
                if done.manual || self.update.announced.as_ref() != Some(&version) {
                    self.update.announce = true;
                }
                update::State::Available { version: version.to_string() }
            }
            Ok(Found::Ready(staged)) => {
                log(&format!("update {} downloaded and verified: {}", staged.tag, staged.exe.display()));
                let version = staged.version.clone();
                if done.manual || install || self.update.announced.as_ref() != Some(&version) {
                    self.update.announce = true;
                }
                self.update.install = install;
                self.update.ready = Some(staged);
                self.update.available = None;
                update::State::Ready { version: version.to_string() }
            }
            Err(e) => {
                log(&format!("update check failed: {e}"));
                if done.manual || install {
                    self.notify("glint: update check failed", &e, true, ClickTarget::Log);
                }
                update::State::Failed { error: e }
            }
        };
        Status::set(state);
        self.when_idle();
    }

    /// The notification, the tray menu or settings asked for the update now.
    pub(super) fn install_update(&mut self) {
        if self.update.ready.is_some() {
            self.update.install = true;
            return self.when_idle();
        }
        let beta = self.cfg.update_beta;
        self.start_update_job(true, beta, true, true);
    }

    pub(super) fn ready_version(&self) -> Option<String> {
        self.update.ready.as_ref().map(|s| s.version.to_string())
    }

    fn idle(&self) -> bool {
        matches!(self.state, State::Idle) && !upload::active() && !self.exiting
    }

    /// Restarts into the update or shows its notification, now or (polling) once nothing is
    /// being picked, edited, recorded or uploaded.
    pub(super) fn when_idle(&mut self) {
        let waiting = self.update.install || self.update.announce;
        if waiting && !self.idle() {
            unsafe {
                SetTimer(Some(self.hwnd), IDLE_TIMER_ID, IDLE_RETRY_MS, None);
            }
            return;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), IDLE_TIMER_ID);
        }
        if self.update.install {
            self.update.announce = false;
            return self.restart_into_update();
        }
        if std::mem::take(&mut self.update.announce) {
            if let Some(s) = &self.update.ready {
                let title = format!("glint {} is ready, restart to update", s.version);
                self.update.announced = Some(s.version.clone());
                let text = "click to restart now; otherwise it's installed the next time glint starts.";
                self.notify(&title, text, false, ClickTarget::Update);
            } else if let Some(c) = &self.update.available {
                let title = format!("glint {} is available", c.version);
                self.update.announced = Some(c.version.clone());
                self.notify(&title, "click to download and install it.", false, ClickTarget::Update);
            }
        }
    }

    fn restart_into_update(&mut self) {
        self.update.install = false;
        let Some(staged) = self.update.ready.take() else { return };
        let args = [UPDATED_ARG.to_string(), Version::current().to_string()];
        match update::install(&staged, &args) {
            Ok(()) => {
                self.exiting = true;
                self.close_window();
            }
            Err(e) => {
                log(&format!("update to {} failed: {e}", staged.tag));
                Status::set(update::State::Failed { error: e.clone() });
                self.notify("glint: couldn't update", &e, true, ClickTarget::Log);
            }
        }
    }

    /// Settings changed: a newly enabled channel or mode gets its check soon.
    pub(super) fn updates_reconfigured(&mut self) {
        self.update.available = None;
        if self.cfg.update_mode != UpdateMode::Off {
            self.schedule_update_check(FIRST_CHECK_MS);
        }
    }
}
