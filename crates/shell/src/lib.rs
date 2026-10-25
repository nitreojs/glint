//! What a Windows tray app of ours is made of, shared by glint and afterglow: the hidden main
//! window and its message loop, one instance at a time, the notification-area icon, global
//! hotkeys, launch at sign-in, the log file, monitors and local time, and what glint and
//! afterglow say to each other.

pub mod afterglow;
pub mod autostart;
pub mod explorer;
pub mod gfx;
pub mod hotkey;
pub mod identity;
pub mod instance;
pub mod log;
pub mod monitors;
pub mod start_menu;
pub mod time;
pub mod tray;
pub mod window;
