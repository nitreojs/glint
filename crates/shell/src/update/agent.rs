//! The tray app's side of updates: checks on a timer (soon after start, then hourly) as the
//! settings' update mode says, "check now" and "install" from the settings window, the tray
//! icon's mark and the notifications, and restarting into the update once the app is idle.

use glint_core::config::UpdateMode;
use glint_core::log::log;
use glint_core::update::release::Candidate;
use glint_core::update::status::State;
use glint_core::update::version::Version;
use glint_core::upload::proxy::Setting;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::reopen::{self, OwnWindow};
use super::{Build, CHECK_BETA, CHECK_DOWNLOAD, Checked, Found, Job, Staged, UPDATED_ARG};

/// Timer ids on the tray app's window; the apps keep theirs below these.
pub const CHECK_TIMER_ID: usize = 9;
/// Retries what waits for the app to be idle (restarting, the "ready" notification).
pub const IDLE_TIMER_ID: usize = 10;
/// The first check waits a little so start-up stays fast.
pub const FIRST_CHECK_MS: u32 = 30_000;
const CHECK_EVERY_MS: u32 = 60 * 60 * 1000;
const IDLE_RETRY_MS: u32 = 2_000;

/// What clicking an update notification does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    /// Restart into the update, or download and install it.
    Install,
    /// Opens the log, which says what went wrong.
    Log,
}

/// What the tray app knows about updates.
pub struct Updates {
    build: Build,
    checking: bool,
    ready: Option<Staged>,
    available: Option<Candidate>,
    /// Restart into the update as soon as the app is idle.
    install: bool,
    /// Show the "ready" / "available" notification once the app is idle.
    announce: bool,
    announced: Option<Version>,
}

impl Updates {
    pub fn new(build: Build) -> Updates {
        Updates { build, checking: false, ready: None, available: None, install: false, announce: false, announced: None }
    }

    /// The downloaded update's version, for the tray menu's "restart to update".
    pub fn ready_version(&self) -> Option<String> {
        self.ready.as_ref().map(|s| s.version.to_string())
    }
}

/// A tray app that updates itself; the provided methods are the whole update flow.
pub trait Host {
    fn updates(&mut self) -> &mut Updates;
    /// The tray app's window: timers and finished checks go to it.
    fn window(&self) -> HWND;
    /// The update mode and the beta channel, as the settings have them.
    fn update_settings(&self) -> (UpdateMode, bool);
    /// The proxy update checks go through.
    fn update_proxy(&self) -> Result<Setting, String>;
    /// Nothing is under way that a restart would cut short.
    fn idle(&self) -> bool;
    fn notify_update(&mut self, title: &str, text: &str, error: bool, click: Click);
    /// Marks the tray icon (and its menu) while a downloaded update waits for a restart.
    fn mark_update_ready(&mut self, ready: bool);
    /// The update was swapped in and started: quit now.
    fn exit_for_update(&mut self);
    /// The app's windows that run as processes of their own (settings, the gallery): a restart
    /// into an update closes them, and the new version opens them again.
    fn own_windows(&self) -> Vec<OwnWindow> {
        Vec::new()
    }

    fn schedule_update_check(&self, ms: u32) {
        unsafe {
            SetTimer(Some(self.window()), CHECK_TIMER_ID, ms, None);
        }
    }

    /// Handles the update timers; false for any other timer.
    fn on_update_timer(&mut self, id: usize) -> bool {
        match id {
            CHECK_TIMER_ID => self.on_check_timer(),
            IDLE_TIMER_ID => self.when_idle(),
            _ => return false,
        }
        true
    }

    fn on_check_timer(&mut self) {
        self.schedule_update_check(CHECK_EVERY_MS);
        let (mode, beta) = self.update_settings();
        if mode == UpdateMode::Off {
            return;
        }
        self.start_update_job(mode == UpdateMode::Auto, beta, false, false);
    }

    /// "check now" in settings; `bits` are [`CHECK_DOWNLOAD`] and [`CHECK_BETA`].
    fn check_now(&mut self, bits: usize) {
        self.updates().available = None;
        self.start_update_job(bits & CHECK_DOWNLOAD != 0, bits & CHECK_BETA != 0, true, false);
    }

    fn start_update_job(&mut self, download: bool, beta: bool, manual: bool, install: bool) {
        if self.updates().checking {
            self.updates().install |= install;
            return;
        }
        let proxy = match self.update_proxy() {
            Ok(p) => p,
            Err(e) => return self.on_checked(Checked { manual, install, result: Err(format!("upload proxy: {e}")) }),
        };
        let candidate = self.updates().available.clone().filter(|_| download);
        let channel = if beta { "beta" } else { "stable" };
        match &candidate {
            Some(c) => log(&format!("downloading the update to {}", c.tag)),
            None => log(&format!("checking for updates ({channel}{})", if manual { ", asked" } else { "" })),
        }
        let build = self.updates().build;
        build.set_status(match &candidate {
            Some(c) => State::Downloading { version: c.version.to_string(), percent: 0 },
            None => State::Checking,
        });
        self.updates().checking = true;
        let job = Job { beta, download, manual, install, candidate, proxy };
        if let Err(e) = build.start(job, self.window()) {
            self.on_checked(Checked { manual, install, result: Err(e) });
        }
    }

    fn on_checked(&mut self, done: Checked) {
        let u = self.updates();
        let build = u.build;
        let name = build.product.name;
        u.checking = false;
        let install = done.install || std::mem::take(&mut u.install);
        let state = match done.result {
            Ok(Found::UpToDate) => {
                log(&format!("update check: {} is the newest", build.current()));
                u.available = None;
                State::UpToDate
            }
            Ok(Found::Available(c)) => {
                log(&format!("update check: {} is available", c.tag));
                let version = c.version.clone();
                u.available = Some(c);
                if install {
                    return self.install_update();
                }
                if done.manual || u.announced.as_ref() != Some(&version) {
                    u.announce = true;
                }
                State::Available { version: version.to_string() }
            }
            Ok(Found::Ready(staged)) => {
                log(&format!("update {} downloaded and verified: {}", staged.tag, staged.exe.display()));
                let version = staged.version.clone();
                if done.manual || install || u.announced.as_ref() != Some(&version) {
                    u.announce = true;
                }
                u.install = install;
                u.ready = Some(staged);
                u.available = None;
                self.mark_update_ready(true);
                State::Ready { version: version.to_string() }
            }
            Err(e) => {
                log(&format!("update check failed: {e}"));
                if done.manual || install {
                    self.notify_update(&format!("{name}: update check failed"), &e, true, Click::Log);
                }
                State::Failed { error: e }
            }
        };
        build.set_status(state);
        self.when_idle();
    }

    /// The notification, the tray menu or settings asked for the update now.
    fn install_update(&mut self) {
        if self.updates().ready.is_some() {
            self.updates().install = true;
            return self.when_idle();
        }
        let (_, beta) = self.update_settings();
        self.start_update_job(true, beta, true, true);
    }

    /// Restarts into the update or shows its notification, now or (polling) once the app is idle.
    fn when_idle(&mut self) {
        let waiting = self.updates().install || self.updates().announce;
        if waiting && !self.idle() {
            unsafe {
                SetTimer(Some(self.window()), IDLE_TIMER_ID, IDLE_RETRY_MS, None);
            }
            return;
        }
        unsafe {
            let _ = KillTimer(Some(self.window()), IDLE_TIMER_ID);
        }
        if self.updates().install {
            self.updates().announce = false;
            return self.restart_into_update();
        }
        let u = self.updates();
        if !std::mem::take(&mut u.announce) {
            return;
        }
        let name = u.build.product.name;
        let ready = u.ready.as_ref().map(|s| (s.version.clone(), true));
        let Some((version, ready)) = ready.or_else(|| u.available.as_ref().map(|c| (c.version.clone(), false))) else {
            return;
        };
        u.announced = Some(version.clone());
        if ready {
            let text = format!("click to restart now; otherwise it's installed the next time {name} starts.");
            self.notify_update(&format!("{name} {version} is ready, restart to update"), &text, false, Click::Install);
        } else {
            self.notify_update(&format!("{name} {version} is available"), "click to download and install it.", false, Click::Install);
        }
    }

    fn restart_into_update(&mut self) {
        let u = self.updates();
        u.install = false;
        let build = u.build;
        let Some(staged) = u.ready.take() else { return };
        self.mark_update_ready(false);
        let open = reopen::open_ones(&self.own_windows());
        let mut args = vec![UPDATED_ARG.to_string(), build.current().to_string()];
        args.extend(reopen::args(&open));
        match build.install(&staged, &args) {
            Ok(()) => {
                reopen::close(&open);
                self.exit_for_update()
            }
            Err(e) => {
                log(&format!("update to {} failed: {e}", staged.tag));
                build.set_status(State::Failed { error: e.clone() });
                self.notify_update(&format!("{}: couldn't update", build.product.name), &e, true, Click::Log);
            }
        }
    }

    /// Settings changed: a newly enabled channel or mode gets its check soon.
    fn updates_reconfigured(&mut self) {
        self.updates().available = None;
        if self.update_settings().0 != UpdateMode::Off {
            self.schedule_update_check(FIRST_CHECK_MS);
        }
    }
}
