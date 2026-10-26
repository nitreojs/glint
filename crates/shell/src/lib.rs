//! What a Windows tray app of ours is made of, shared by glint and afterglow: the hidden main
//! window and its message loop, one instance at a time, the notification-area icon and its
//! menu, global hotkeys, launch at sign-in, the log file, monitors and local time, the clipboard,
//! dragging files out, and what glint and afterglow say to each other.

pub mod afterglow;
pub mod autostart;
pub mod clipboard;
pub mod drag;
pub mod explorer;
pub mod file_menu;
pub mod gfx;
pub mod hotkey;
pub mod icon;
pub mod identity;
pub mod instance;
pub mod log;
pub mod menu;
pub mod monitors;
pub mod paste;
pub mod start_menu;
pub mod time;
pub mod tray;
pub mod window;
