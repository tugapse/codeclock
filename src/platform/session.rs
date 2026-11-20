//! Idle time and screen lock over D-Bus, without depending on one desktop.
//!
//! Idle time, first source that answers wins:
//! 1. GNOME/Mutter `org.gnome.Mutter.IdleMonitor.GetIdletime` (milliseconds, precise)
//! 2. `org.freedesktop.ScreenSaver.GetSessionIdleTime` (Xfce, Cinnamon and others; seconds)
//! 3. the Wayland compositor's `ext-idle-notify` (KDE Plasma, Sway, Hyprland), see `wayland_idle`
//! 4. logind `IdleHint` / `IdleSinceHint` (coarse, and many desktops never set it)
//!
//! Screen lock: logind `LockedHint` of the user's graphical session.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedObjectPath;

use super::wayland_idle::WaylandIdle;

#[derive(Debug, Clone, Copy, PartialEq)]
enum IdleMethod {
    Mutter,
    ScreenSaver,
    Wayland,
    Logind,
}

pub struct Session {
    bus: Option<Connection>,
    system: Connection,
    wayland: Option<WaylandIdle>,
    method: Option<IdleMethod>,
}

impl Session {
    pub fn connect() -> Result<Self> {
        Ok(Session {
            bus: Connection::session().ok(),
            system: Connection::system().context("connecting to the system bus")?,
            wayland: match WaylandIdle::connect() {
                Ok(w) => Some(w),
                Err(e) => {
                    log::debug!("no Wayland idle source: {e:#}");
                    None
                }
            },
            method: None,
        })
    }

    /// Name of the idle source in use, once one answered.
    pub fn idle_method(&self) -> &'static str {
        match self.method {
            Some(IdleMethod::Mutter) => "mutter",
            Some(IdleMethod::ScreenSaver) => "screensaver",
            Some(IdleMethod::Wayland) => "wayland",
            Some(IdleMethod::Logind) => "logind",
            None => "none",
        }
    }

    /// Seconds since the last keyboard or mouse input, or `None` when no source answers.
    pub fn idle_secs(&mut self) -> Option<u64> {
        let order = match self.method {
            Some(m) => vec![m],
            None => vec![IdleMethod::Mutter, IdleMethod::ScreenSaver, IdleMethod::Wayland, IdleMethod::Logind],
        };
        for m in order {
            let value = match m {
                IdleMethod::Mutter => self.mutter_idle(),
                IdleMethod::ScreenSaver => self.screensaver_idle(),
                IdleMethod::Wayland => self.wayland.as_ref().and_then(|w| w.idle_secs()).context("no Wayland idle source"),
                IdleMethod::Logind => self.logind_idle(),
            };
            if let Ok(v) = value {
                if self.method.is_none() {
                    log::info!("idle source: {:?}", m);
                }
                self.method = Some(m);
                return Some(v);
            }
        }
        // The chosen source stopped answering (desktop restarted): try all again next time.
        self.method = None;
        None
    }

    fn mutter_idle(&self) -> Result<u64> {
        let bus = self.bus.as_ref().context("no session bus")?;
        let p = Proxy::new(
            bus,
            "org.gnome.Mutter.IdleMonitor",
            "/org/gnome/Mutter/IdleMonitor/Core",
            "org.gnome.Mutter.IdleMonitor",
        )?;
        let ms: u64 = p.call("GetIdletime", &())?;
        Ok(ms / 1000)
    }

    fn screensaver_idle(&self) -> Result<u64> {
        let bus = self.bus.as_ref().context("no session bus")?;
        let p = Proxy::new(
            bus,
            "org.freedesktop.ScreenSaver",
            "/org/freedesktop/ScreenSaver",
            "org.freedesktop.ScreenSaver",
        )?;
        let secs: u32 = p.call("GetSessionIdleTime", &())?;
        Ok(secs as u64)
    }

    fn logind_idle(&self) -> Result<u64> {
        let s = self.session_proxy()?;
        if !s.get_property::<bool>("IdleHint")? {
            return Ok(0);
        }
        let since_us: u64 = s.get_property("IdleSinceHint")?;
        let now_us = SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros() as u64;
        Ok(now_us.saturating_sub(since_us) / 1_000_000)
    }

    /// Whether the user's graphical session is locked (false when unknown).
    pub fn locked(&self) -> bool {
        self.session_proxy()
            .and_then(|s| Ok(s.get_property::<bool>("LockedHint")?))
            .unwrap_or(false)
    }

    /// The user's graphical session, through `user/self` so it also works from a systemd
    /// user service, which runs outside the session.
    fn session_proxy(&self) -> Result<Proxy<'_>> {
        let user = Proxy::new(
            &self.system,
            "org.freedesktop.login1",
            "/org/freedesktop/login1/user/self",
            "org.freedesktop.login1.User",
        )?;
        let (_id, path): (String, OwnedObjectPath) = user.get_property("Display")?;
        Ok(Proxy::new(
            &self.system,
            "org.freedesktop.login1",
            path,
            "org.freedesktop.login1.Session",
        )?)
    }
}
