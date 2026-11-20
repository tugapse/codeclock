//! Focused window through KWin, the KDE window manager (Plasma 5 and 6, Wayland and X11).
//!
//! On Wayland no app can ask which window has focus, so codeclock loads a small KWin script
//! that calls back over D-Bus whenever the active window, or its title, changes. It sees every
//! window KWin manages, native Wayland and XWayland alike.
//!
//! Each running codeclock loads its own copy of the script, named after its process id, that
//! calls this connection's unique bus name; calls from anyone but KWin are ignored.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use zbus::blocking::{connection, Connection};
use zbus::message::Header;

use super::{FocusSource, WindowInfo};

const KWIN: &str = "org.kde.KWin";
const IFACE: &str = "io.github.tugapse.Codeclock.Focus";
const PATH: &str = "/focus";

#[derive(Default)]
struct State {
    window: Option<WindowInfo>,
    /// KWin's unique bus name: the only caller whose updates are accepted.
    kwin: Option<String>,
    /// Whether the script has called back at least once.
    heard: bool,
}

struct FocusIface {
    state: Arc<Mutex<State>>,
}

#[zbus::interface(name = "io.github.tugapse.Codeclock.Focus")]
impl FocusIface {
    /// Called by the KWin script: the active window's title, class and process id
    /// (all strings; empty when no window has focus).
    fn update(&self, #[zbus(header)] header: Header<'_>, caption: String, class: String, pid: String) {
        let sender = header.sender().map(|s| s.to_string());
        if let Ok(mut st) = self.state.lock() {
            if st.kwin.is_none() || sender != st.kwin {
                log::debug!("ignoring a focus update from {sender:?}");
                return;
            }
            st.window = window_from(caption, class, &pid);
            st.heard = true;
        }
    }
}

fn window_from(caption: String, class: String, pid: &str) -> Option<WindowInfo> {
    // Nothing focused, or KWin's own surfaces (the lock screen has class kwin_wayland and no title).
    if (caption.is_empty() && class.is_empty()) || (caption.is_empty() && class.starts_with("kwin")) {
        return None;
    }
    Some(WindowInfo { class, title: caption, pid: pid.trim().parse::<u32>().ok().filter(|p| *p > 0) })
}

/// The script KWin runs. `callDBus` turns JavaScript numbers into doubles, so the process id
/// travels as a string.
fn script(dest: &str) -> String {
    format!(
        r#"// Loaded by codeclock: tells it which window has focus.
const dest = "{dest}";
function send(w) {{
    if (!w) {{
        callDBus(dest, "{PATH}", "{IFACE}", "Update", "", "", "");
        return;
    }}
    callDBus(dest, "{PATH}", "{IFACE}", "Update",
             String(w.caption || ""), String(w.resourceClass || ""), String(w.pid || ""));
}}
let watched = null;
function captionChanged() {{ send(watched); }}
function activated(w) {{
    if (watched && watched.captionChanged) {{
        try {{ watched.captionChanged.disconnect(captionChanged); }} catch (e) {{}}
    }}
    watched = w;
    if (w && w.captionChanged) {{
        w.captionChanged.connect(captionChanged);
    }}
    send(w);
}}
if (workspace.windowActivated) {{          // Plasma 6
    workspace.windowActivated.connect(activated);
    activated(workspace.activeWindow);
}} else {{                                  // Plasma 5
    workspace.clientActivated.connect(activated);
    activated(workspace.activeClient);
}}
"#
    )
}

pub struct KwinFocus {
    conn: Connection,
    state: Arc<Mutex<State>>,
    plugin: String,
    script_path: PathBuf,
    last_check: Instant,
}

impl KwinFocus {
    /// Works when KWin is on the session bus.
    pub fn connect() -> Result<Self> {
        let state = Arc::new(Mutex::new(State::default()));
        let conn = connection::Builder::session()?
            .serve_at(PATH, FocusIface { state: state.clone() })?
            .build()
            .context("connecting to the D-Bus session bus")?;
        let mut me = KwinFocus {
            conn,
            state,
            plugin: format!("codeclock-{}", std::process::id()),
            script_path: std::env::temp_dir().join(format!("codeclock-kwin-{}.js", std::process::id())),
            last_check: Instant::now(),
        };
        me.load()?;
        // The script reports the current window as soon as it starts; wait for it briefly so the
        // first sample (and `codeclock status`) already sees it.
        for _ in 0..20 {
            if me.state.lock().map(|s| s.heard).unwrap_or(true) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !me.state.lock().map(|s| s.heard).unwrap_or(false) {
            log::warn!("the KWin focus script loaded but has not called back yet");
        }
        Ok(me)
    }

    fn kwin_owner(&self) -> Result<String> {
        let dbus = zbus::blocking::fdo::DBusProxy::new(&self.conn)?;
        let owner = dbus
            .get_name_owner(KWIN.try_into()?)
            .with_context(|| format!("{KWIN} is not on the session bus (not a KDE session?)"))?;
        Ok(owner.to_string())
    }

    fn call<R>(&self, path: &str, iface: &str, method: &str, args: &(impl serde::Serialize + zbus::zvariant::DynamicType)) -> Result<R>
    where
        R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
    {
        let reply = self.conn.call_method(Some(KWIN), path, Some(iface), method, args)?;
        Ok(reply.body().deserialize::<R>()?)
    }

    /// Writes the script and has KWin run it.
    fn load(&mut self) -> Result<()> {
        let owner = self.kwin_owner()?;
        let me = self.conn.unique_name().context("no unique bus name")?.to_string();
        std::fs::write(&self.script_path, script(&me))?;
        if let Ok(mut st) = self.state.lock() {
            st.kwin = Some(owner);
        }

        let _ = self.call::<bool>("/Scripting", "org.kde.kwin.Scripting", "unloadScript", &(self.plugin.as_str(),));
        let path = self.script_path.to_string_lossy().to_string();
        let id: i32 = self
            .call("/Scripting", "org.kde.kwin.Scripting", "loadScript", &(path.as_str(), self.plugin.as_str()))
            .context("asking KWin to load the focus script")?;
        if id < 0 {
            bail!("KWin refused the focus script");
        }
        // Plasma 6 and late Plasma 5 use /Scripting/Script<id>; older Plasma 5 used /<id>.
        let run = self
            .call::<()>(&format!("/Scripting/Script{id}"), "org.kde.kwin.Script", "run", &())
            .or_else(|_| self.call::<()>(&format!("/{id}"), "org.kde.kwin.Script", "run", &()));
        run.context("starting the focus script")?;
        log::info!("KWin focus script loaded as {}", self.plugin);
        Ok(())
    }
}

impl FocusSource for KwinFocus {
    fn name(&self) -> &'static str {
        "kwin"
    }

    fn focused(&mut self) -> Result<Option<WindowInfo>> {
        // KWin restarts (or crashes) drop the script: check now and then, and load it again.
        if self.last_check.elapsed() > Duration::from_secs(60) {
            self.last_check = Instant::now();
            let loaded: bool = self
                .call("/Scripting", "org.kde.kwin.Scripting", "isScriptLoaded", &(self.plugin.as_str(),))
                .unwrap_or(false);
            if !loaded {
                log::info!("KWin focus script is gone, loading it again");
                self.load()?;
            }
        }
        Ok(self.state.lock().map(|s| s.window.clone()).unwrap_or_default())
    }
}

impl Drop for KwinFocus {
    fn drop(&mut self) {
        let _ = self.call::<bool>("/Scripting", "org.kde.kwin.Scripting", "unloadScript", &(self.plugin.as_str(),));
        let _ = std::fs::remove_file(&self.script_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_calls_back_this_connection() {
        let s = script(":1.42");
        assert!(s.contains(r#"const dest = ":1.42";"#));
        assert!(s.contains(r#""/focus", "io.github.tugapse.Codeclock.Focus", "Update""#));
        assert!(s.contains("workspace.windowActivated") && s.contains("workspace.clientActivated"));
    }

    #[test]
    fn updates_become_windows() {
        let w = window_from("main.rs — shop — Visual Studio Code".into(), "code".into(), "4242").unwrap();
        assert_eq!((w.class.as_str(), w.pid), ("code", Some(4242)));
        assert_eq!(window_from("".into(), "".into(), ""), None);
        assert_eq!(window_from("".into(), "kwin_wayland".into(), "1554"), None, "the lock screen");
        assert_eq!(window_from("x".into(), "y".into(), "0").unwrap().pid, None);
    }
}
