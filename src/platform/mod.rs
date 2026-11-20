//! Signals from the desktop: the focused window, idle time and screen lock.
//!
//! Each desktop exposes the focused window differently, so focus sources are small modules
//! chosen at startup: KWin (KDE, Wayland or X11) and X11 (which also covers XWayland apps on
//! other Wayland desktops). Sway, Hyprland and GNOME-on-Wayland modules come later.

pub mod kwin;
pub mod session;
pub mod wayland_idle;
pub mod x11;

use std::path::PathBuf;

use anyhow::Result;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WindowInfo {
    /// X11 `WM_CLASS` class (or app id on Wayland sources).
    pub class: String,
    pub title: String,
    pub pid: Option<u32>,
}

impl WindowInfo {
    /// The process name (`/proc/<pid>/comm`), empty when unknown.
    pub fn process_name(&self) -> String {
        self.pid
            .and_then(|p| std::fs::read_to_string(format!("/proc/{p}/comm")).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// The working directory of the window's process.
    pub fn cwd(&self) -> Option<PathBuf> {
        self.pid.and_then(|p| std::fs::read_link(format!("/proc/{p}/cwd")).ok())
    }
}

pub trait FocusSource {
    fn name(&self) -> &'static str;
    /// The focused window, or `None` when no window (or a window this source cannot see) has focus.
    fn focused(&mut self) -> Result<Option<WindowInfo>>;
}

/// The first focus source that works on this desktop: KWin when it is running (KDE, on
/// Wayland or X11), else X11 (which also sees XWayland apps on other Wayland desktops).
pub fn detect_focus() -> Option<Box<dyn FocusSource>> {
    match kwin::KwinFocus::connect() {
        Ok(f) => return Some(Box::new(f)),
        Err(e) => log::debug!("no KWin focus source: {e:#}"),
    }
    match x11::X11Focus::connect() {
        Ok(f) => Some(Box::new(f)),
        Err(e) => {
            log::warn!("no X11 focus source: {e:#}");
            None
        }
    }
}
