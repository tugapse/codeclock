//! Focused window through X11 (`_NET_ACTIVE_WINDOW`).
//!
//! Works on any X11 desktop, and on Wayland desktops for apps running under XWayland: the
//! property points at the focused X11 window, and is 0 while a native Wayland window has focus.
//! VS Code runs under XWayland unless started with `--ozone-platform=wayland`.

use anyhow::{Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt, Window};
use x11rb::rust_connection::RustConnection;

use super::{FocusSource, WindowInfo};

pub struct X11Focus {
    conn: RustConnection,
    root: Window,
    net_active_window: Atom,
    net_wm_name: Atom,
    net_wm_pid: Atom,
    utf8_string: Atom,
}

impl X11Focus {
    pub fn connect() -> Result<Self> {
        let (conn, screen) = x11rb::connect(None).context("connecting to the X server")?;
        let root = conn.setup().roots[screen].root;
        let atom = |name: &str| -> Result<Atom> {
            Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
        };
        Ok(X11Focus {
            net_active_window: atom("_NET_ACTIVE_WINDOW")?,
            net_wm_name: atom("_NET_WM_NAME")?,
            net_wm_pid: atom("_NET_WM_PID")?,
            utf8_string: atom("UTF8_STRING")?,
            root,
            conn,
        })
    }

    fn property(&self, window: Window, prop: Atom, kind: Atom) -> Result<Vec<u8>> {
        let reply = self
            .conn
            .get_property(false, window, prop, kind, 0, 4096)?
            .reply()?;
        Ok(reply.value)
    }

    fn active_window(&self) -> Result<Option<Window>> {
        let reply = self
            .conn
            .get_property(false, self.root, self.net_active_window, AtomEnum::WINDOW, 0, 1)?
            .reply()?;
        Ok(reply.value32().and_then(|mut v| v.next()).filter(|w| *w != 0))
    }

    pub fn info(&self, window: Window) -> Result<WindowInfo> {
        // WM_CLASS = "instance\0class\0"; the class is the second string.
        let class_raw = self.property(window, AtomEnum::WM_CLASS.into(), AtomEnum::STRING.into())?;
        let mut parts = class_raw.split(|b| *b == 0).filter(|s| !s.is_empty());
        let instance = parts.next().map(|s| String::from_utf8_lossy(s).to_string());
        let class = parts.next().map(|s| String::from_utf8_lossy(s).to_string()).or(instance);

        let mut title = self.property(window, self.net_wm_name, self.utf8_string)?;
        if title.is_empty() {
            title = self.property(window, AtomEnum::WM_NAME.into(), AtomEnum::STRING.into())?;
        }

        let pid = self
            .conn
            .get_property(false, window, self.net_wm_pid, AtomEnum::CARDINAL, 0, 1)?
            .reply()?
            .value32()
            .and_then(|mut v| v.next());

        Ok(WindowInfo {
            class: class.unwrap_or_default(),
            title: String::from_utf8_lossy(&title).to_string(),
            pid,
        })
    }
}

impl FocusSource for X11Focus {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn focused(&mut self) -> Result<Option<WindowInfo>> {
        match self.active_window()? {
            None => Ok(None),
            // The window may be gone between the two requests: treat that as no focus.
            Some(w) => Ok(self.info(w).ok()),
        }
    }
}
