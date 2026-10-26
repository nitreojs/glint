//! glint's side of the shared update flow ([`glint_shell::update::Host`]).

use glint_core::config::UpdateMode;
use glint_core::update::GLINT;
use glint_core::upload::proxy::{self, Setting};
use glint_shell::update::{Build, Click, Host, Updates};
use windows::Win32::Foundation::HWND;

use super::{App, ClickTarget, State};
use crate::upload;

/// This build of glint, as the updater knows it.
pub(crate) const BUILD: Build = Build { product: GLINT, version: env!("CARGO_PKG_VERSION") };

impl Host for App {
    fn updates(&mut self) -> &mut Updates {
        &mut self.update
    }

    fn window(&self) -> HWND {
        self.hwnd
    }

    fn update_settings(&self) -> (UpdateMode, bool) {
        (self.cfg.update_mode, self.cfg.update_beta)
    }

    fn update_proxy(&self) -> Result<Setting, String> {
        proxy::parse(&self.cfg.upload_proxy)
    }

    /// Nothing is being picked, edited, recorded or uploaded.
    fn idle(&self) -> bool {
        matches!(self.state, State::Idle) && !upload::active() && !self.exiting
    }

    fn notify_update(&mut self, title: &str, text: &str, error: bool, click: Click) {
        let click = match click {
            Click::Install => ClickTarget::Update,
            Click::Log => ClickTarget::Log,
        };
        self.notify(title, text, error, click);
    }

    fn mark_update_ready(&mut self, ready: bool) {
        self.tray.set_update_ready(ready);
    }

    fn exit_for_update(&mut self) {
        self.exiting = true;
        self.close_window();
    }
}
