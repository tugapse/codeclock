//! The background loop: sample the desktop, apply the counting rules, refresh the tray.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ksni::blocking::TrayMethods;

use crate::config::Config;
use crate::platform::{self, session::Session, FocusSource, WindowInfo};
use crate::project::Projects;
use crate::report;
use crate::store::Store;
use crate::tracker::{Observation, Tracker, Work};
use crate::tray::{CodeclockTray, TrayState};
use crate::sync;
use crate::web::{self, Live, SharedLive};

/// Everything needed to turn the desktop's state into an observation.
pub struct Sampler {
    pub cfg: Config,
    pub projects: Projects,
    pub focus: Option<Box<dyn FocusSource>>,
    pub session: Option<Session>,
    last_scan: Instant,
}

/// What one sample saw, kept for `codeclock status`.
#[derive(Debug, Default)]
pub struct Sample {
    pub window: Option<WindowInfo>,
    pub editor: Option<String>,
    pub project: Option<String>,
    pub observation: Observation,
}

impl Sampler {
    pub fn new(cfg: Config) -> Self {
        let projects = Projects::scan(&cfg.roots());
        let session = match Session::connect() {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!("no D-Bus session information: {e:#}");
                None
            }
        };
        Sampler { projects, focus: platform::detect_focus(), session, cfg, last_scan: Instant::now() }
    }

    pub fn sample(&mut self) -> Sample {
        // New project folders appear while running.
        if self.last_scan.elapsed() > Duration::from_secs(60) {
            self.projects = Projects::scan(&self.cfg.roots());
            self.last_scan = Instant::now();
        }
        if self.focus.is_none() {
            self.focus = platform::detect_focus();
        }

        let window = match self.focus.as_mut().map(|f| f.focused()) {
            Some(Ok(w)) => w,
            Some(Err(e)) => {
                // The X server went away (logout, XWayland restart): reconnect next time.
                log::warn!("focus source failed: {e:#}");
                self.focus = None;
                None
            }
            None => None,
        };

        let mut s = Sample { window: window.clone(), ..Default::default() };
        if let Some(w) = &window
            && let Some(editor) = self.cfg.editor_for(&w.class, &w.process_name()) {
                s.editor = Some(editor.name.clone());
                s.project = self.projects.detect(&w.title, w.cwd().as_deref());
            }
        let (idle_secs, locked) = match self.session.as_mut() {
            Some(sess) => (sess.idle_secs(), sess.locked()),
            None => (None, false),
        };
        s.observation = Observation {
            work: match (&s.editor, &s.project) {
                (Some(e), Some(p)) => Some(Work { project: p.clone(), editor: e.clone() }),
                _ => None,
            },
            idle_secs,
            locked,
        };
        s
    }
}

pub fn run(cfg: Config) -> Result<()> {
    let mut store = Store::open(&Store::default_path())?;
    let machine = cfg.machine();
    store.set_machine(&machine);
    let sync_interval = Duration::from_secs(cfg.sync.interval_secs.max(60));
    let mut tracker = Tracker::new(cfg.idle_timeout_secs, cfg.sample_secs);
    let interval = Duration::from_secs(cfg.sample_secs.max(1));
    let dashboard_port = cfg.dashboard_port;
    let roots = cfg.roots();
    let mut sampler = Sampler::new(cfg);

    // Folders that are clones of a repository with another name: their old blocks move to it.
    for (folder, name) in sampler.projects.renamed() {
        match store.rename_project(&folder, &name, &machine) {
            Ok(0) => {}
            Ok(n) => log::info!("moved {n} blocks from folder name {folder} to repository name {name}"),
            Err(e) => log::warn!("could not rename {folder} to {name}: {e:#}"),
        }
    }

    // Stop cleanly on SIGTERM (logout, `systemctl stop`) so the last changes get synced.
    let stop = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(sig, stop.clone())?;
    }

    let worker = if sync::is_enabled(&sync::repo_dir()) {
        log::info!("syncing as {machine} through {}", sync::repo_dir().display());
        Some(sync::Worker::start(Store::default_path(), sync::repo_dir(), machine.clone(), sync_interval)?)
    } else {
        None
    };

    // The dashboard reads and edits the same database; it must not touch the open block.
    let live = SharedLive::default();
    let dashboard = match dashboard_port {
        0 => None,
        port => match web::spawn(port, Store::default_path(), roots, live.clone(), worker.clone()) {
            Ok(()) => Some(web::url(port)),
            Err(e) => {
                log::warn!("dashboard not started: {e:#}");
                None
            }
        },
    };

    log::info!(
        "codeclock running: {} projects, focus source {}, database {}",
        sampler.projects.len(),
        sampler.focus.as_ref().map(|f| f.name()).unwrap_or("none"),
        Store::default_path().display()
    );
    if sampler.focus.is_none() {
        log::warn!("no focus source for this desktop: nothing will be counted until one is available");
    }

    // The tray host may appear after us (login, unlocking the screen): keep trying.
    let mut tray = None;
    let mut last_tray_try: Option<Instant> = None;
    let mut warned_tray = false;

    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let sample = sampler.sample();
        if let Err(e) = tracker.tick(report::now(), &sample.observation, &mut store) {
            log::error!("could not record time: {e:#}");
        }
        if let Ok(mut l) = live.lock() {
            *l = Live { project: tracker.current_project().map(str::to_string), block: tracker.current_block() };
        }

        if tray.is_none() && last_tray_try.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
            last_tray_try = Some(Instant::now());
            match (CodeclockTray { state: TrayState::default(), dashboard: dashboard.clone() }).spawn() {
                Ok(h) => {
                    log::info!("tray icon registered");
                    tray = Some(h);
                }
                Err(e) if !warned_tray => {
                    log::warn!("no system tray yet, counting without icon and retrying: {e}");
                    warned_tray = true;
                }
                Err(_) => {}
            }
        }

        if let Some(handle) = &tray {
            let current = tracker.current_project().map(str::to_string);
            let today = store.totals(report::start_of_today(), report::now()).unwrap_or_default();
            let current_total = current
                .as_deref()
                .and_then(|p| store.project_total(p).ok())
                .unwrap_or(0);
            handle.update(|t| t.state = TrayState { current, today, current_total });
        }

        // Sleep in short steps so a stop request is noticed quickly.
        while started.elapsed() < interval && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    log::info!("stopping");
    if let Some(w) = worker {
        match w.sync_now(Duration::from_secs(20)) {
            Ok(o) => log::info!("final sync: sent {}, received {}", o.sent, o.received),
            Err(e) => log::warn!("final sync: {e}"),
        }
    }
    Ok(())
}
