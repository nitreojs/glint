//! glint's notification-area icon: idle (in the accent, with a dot while an update waits) or
//! recording, and balloon notifications.

use glint_core::icon::{RECORDING_RED, add_badge, icon_pixels};
use glint_shell::tray::{TrayIcon, destroy, icon_size, make_icon};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::HICON;

pub(crate) const IDLE_TIP: &str = "glint\nclick: screenshot · double-click: settings";
const READY_MARK: &str = " (update ready)";

pub(crate) struct Tray {
    icon: TrayIcon,
    size: i32,
    idle: HICON,
    rec: HICON,
    recording: bool,
    accent: [u8; 3],
    /// A downloaded update waits: the idle icon has a dot and the tooltip says so.
    update_ready: bool,
    /// The idle tooltip, without the update mark.
    idle_tip: String,
}

impl Tray {
    pub(crate) fn new(hwnd: HWND, callback: u32, accent: [u8; 3]) -> Tray {
        let size = icon_size();
        let idle = glint_icon(size, true, accent, false);
        Tray {
            icon: TrayIcon::new(hwnd, callback, idle, IDLE_TIP),
            size,
            idle,
            rec: glint_icon(size, false, RECORDING_RED, false),
            recording: false,
            accent,
            update_ready: false,
            idle_tip: IDLE_TIP.to_string(),
        }
    }

    /// Redraws the idle icon in a new accent colour.
    pub(crate) fn set_accent(&mut self, accent: [u8; 3]) {
        self.accent = accent;
        self.redraw_idle();
    }

    /// Marks the icon and tooltip while a downloaded update waits for a restart.
    pub(crate) fn set_update_ready(&mut self, ready: bool) {
        if self.update_ready == ready {
            return;
        }
        self.update_ready = ready;
        self.redraw_idle();
    }

    fn redraw_idle(&mut self) {
        let old = std::mem::replace(&mut self.idle, glint_icon(self.size, true, self.accent, self.update_ready));
        self.refresh();
        destroy(old);
    }

    /// Shows the icon and tooltip for the current state.
    fn refresh(&mut self) {
        if !self.recording {
            let tip = tip_with_mark(&self.idle_tip, self.update_ready);
            self.icon.show(self.idle, &tip);
        }
    }

    /// Adds the icon again after Explorer restarts.
    pub(crate) fn add(&self) {
        self.icon.add();
    }

    pub(crate) fn set_recording(&mut self, on: bool, tip: &str) {
        self.recording = on;
        if on {
            self.icon.show(self.rec, tip);
        } else {
            self.idle_tip = tip.to_string();
            self.refresh();
        }
    }

    pub(crate) fn set_idle_tip(&mut self, tip: &str) {
        self.idle_tip = tip.to_string();
        self.refresh();
    }

    pub(crate) fn notify(&self, title: &str, text: &str, error: bool) {
        self.icon.notify(title, text, error);
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.icon.remove();
        destroy(self.idle);
        destroy(self.rec);
    }
}

fn tip_with_mark(tip: &str, ready: bool) -> String {
    if !ready {
        return tip.to_string();
    }
    match tip.split_once('\n') {
        Some((first, rest)) => format!("{first}{READY_MARK}\n{rest}"),
        None => format!("{tip}{READY_MARK}"),
    }
}

fn glint_icon(size: i32, highlight: bool, color: [u8; 3], badge: bool) -> HICON {
    let mut px = icon_pixels(size, highlight, color);
    if badge {
        add_badge(&mut px, size, color);
    }
    make_icon(size, &px)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waiting_update_shows_in_the_tooltip() {
        assert_eq!(tip_with_mark(IDLE_TIP, false), IDLE_TIP);
        assert_eq!(tip_with_mark(IDLE_TIP, true), "glint (update ready)\nclick: screenshot · double-click: settings");
        assert_eq!(tip_with_mark("glint: uploading 40 %", true), "glint: uploading 40 % (update ready)");
    }
}
