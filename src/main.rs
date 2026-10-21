#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod compact_menu;
#[cfg(windows)]
mod container_frame;
#[cfg(windows)]
mod hotkey;
#[cfg(windows)]
mod list_scrollbar;
#[cfg(windows)]
mod localization;
#[cfg(windows)]
mod picker_overlay;
#[cfg(windows)]
mod presentation;
#[cfg(windows)]
mod settings;
#[cfg(windows)]
mod settings_panel;
#[cfg(windows)]
mod taskbar;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod ui_theme;
#[cfg(windows)]
mod window_container;
#[cfg(windows)]
mod window_control;
#[cfg(windows)]
mod window_events;
#[cfg(windows)]
mod window_search;
#[cfg(windows)]
mod window_topmost;

#[cfg(windows)]
fn main() {
    if let Err(message) = app::run() {
        app::show_startup_error(&message);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Trans 只支持 Windows 10 / 11");
}
