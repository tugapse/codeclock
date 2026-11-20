//! Idle time from the Wayland compositor (`ext-idle-notify-v1`), for desktops that do not offer
//! it over D-Bus: KDE Plasma refuses `org.freedesktop.ScreenSaver.GetSessionIdleTime` and does
//! not keep logind's idle hint, but KWin (like Sway and Hyprland) speaks this protocol.
//!
//! The protocol only says "idle" after a timeout and "resumed" on input, so codeclock asks for a
//! one-second timeout: when "idle" arrives, the last input was about a second earlier.
//! Version 2's input-only notification is preferred, so a playing video (an idle inhibitor)
//! does not hide that you stepped away.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notification_v1::{self, ExtIdleNotificationV1};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::ExtIdleNotifierV1;

const TIMEOUT_MS: u32 = 1000;

#[derive(Default)]
struct Shared {
    /// When the last input happened, while idle; `None` while active.
    idle_since: Option<Instant>,
}

struct App {
    shared: Arc<Mutex<Shared>>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for App {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ExtIdleNotifierV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: <ExtIdleNotifierV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Ok(mut s) = app.shared.lock() {
            match event {
                ext_idle_notification_v1::Event::Idled => {
                    s.idle_since = Some(Instant::now() - Duration::from_millis(TIMEOUT_MS as u64));
                }
                ext_idle_notification_v1::Event::Resumed => s.idle_since = None,
                _ => {}
            }
        }
    }
}

pub struct WaylandIdle {
    shared: Arc<Mutex<Shared>>,
    alive: Arc<AtomicBool>,
}

impl WaylandIdle {
    /// Works on Wayland compositors that offer `ext_idle_notifier_v1`.
    pub fn connect() -> Result<Self> {
        let conn = Connection::connect_to_env().context("no Wayland display")?;
        let (globals, mut queue) = registry_queue_init::<App>(&conn)?;
        let qh = queue.handle();
        let notifier: ExtIdleNotifierV1 =
            globals.bind(&qh, 1..=2, ()).context("the compositor has no ext-idle-notify")?;
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ()).context("no Wayland seat")?;
        let notification = if notifier.version() >= 2 {
            notifier.get_input_idle_notification(TIMEOUT_MS, &seat, &qh, ())
        } else {
            notifier.get_idle_notification(TIMEOUT_MS, &seat, &qh, ())
        };

        let shared = Arc::new(Mutex::new(Shared::default()));
        let alive = Arc::new(AtomicBool::new(true));
        let mut app = App { shared: shared.clone() };
        queue.roundtrip(&mut app)?;

        let still = alive.clone();
        std::thread::Builder::new().name("wayland-idle".into()).spawn(move || {
            // Kept here so the compositor keeps sending events for as long as the thread runs.
            let _keep = (conn, notifier, seat, notification);
            while queue.blocking_dispatch(&mut app).is_ok() {}
            still.store(false, Ordering::Relaxed);
            log::warn!("Wayland idle source disconnected");
        })?;
        // "Idle" can only arrive one timeout after the notification exists; wait for it once, so
        // the first reading (and `codeclock status`) already knows whether you are away.
        std::thread::sleep(Duration::from_millis(TIMEOUT_MS as u64 + 200));
        Ok(WaylandIdle { shared, alive })
    }

    /// Seconds since the last input, or `None` once the compositor connection is gone.
    pub fn idle_secs(&self) -> Option<u64> {
        if !self.alive.load(Ordering::Relaxed) {
            return None;
        }
        let since = self.shared.lock().ok()?.idle_since;
        Some(since.map(|t| t.elapsed().as_secs()).unwrap_or(0))
    }
}
