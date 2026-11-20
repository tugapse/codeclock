//! codeclock: how long you actually work on each project, counted from your editor.

mod config;
mod daemon;
mod legacy;
mod platform;
mod project;
mod report;
mod store;
mod sync;
mod tracker;
mod tray;
mod web;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use config::Config;
use report::fmt_dur;
use store::Store;

#[derive(Parser)]
#[command(name = "codeclock", version, about = "Time spent working on each project, counted from your editor")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum SyncAction {
    /// Clone a private git repository and start syncing through it
    Init {
        /// e.g. git@github.com:you/codeclock-data.git
        repository: String,
        /// This machine already synced before (reinstalled): keep using its name
        #[arg(long)]
        reuse: bool,
    },
    /// Show this machine's name, the repository and the last sync
    Status,
}

#[derive(Subcommand)]
enum Command {
    /// Run in the background with the tray icon (the default)
    Run,
    /// Time per project today (or this week, or all time)
    Report {
        #[arg(long, conflicts_with = "all")]
        week: bool,
        #[arg(long)]
        all: bool,
        /// Do not sync with the other machines first
        #[arg(long)]
        offline: bool,
    },
    /// Sync with your other machines now, or set syncing up
    Sync {
        #[command(subcommand)]
        action: Option<SyncAction>,
    },
    /// Show what codeclock sees right now: focused window, editor, project, idle, lock
    Status {
        /// Check one X11 window by id (e.g. 0x1a00003) instead of the focused one
        #[arg(long)]
        window: Option<String>,
    },
    /// Write the default config file if there is none, and print its path
    InitConfig,
    /// Install and start the systemd user service
    InstallService,
    /// Open the history dashboard in the browser
    Open,
    /// Run only the dashboard, without counting (the service already serves it)
    Serve {
        /// Port to listen on (default: dashboard_port from the config)
        #[arg(long)]
        port: Option<u16>,
    },
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    legacy::migrate();
    match cli.command.unwrap_or(Command::Run) {
        Command::Run => daemon::run(Config::load()?),
        Command::Report { week, all, offline } => report_cmd(week, all, offline),
        Command::Sync { action } => sync_cmd(action),
        Command::Status { window: None } => status_cmd(),
        Command::Status { window: Some(id) } => window_cmd(&id),
        Command::InitConfig => {
            println!("{}", Config::write_default_if_missing()?.display());
            Ok(())
        }
        Command::InstallService => install_service(),
        Command::Open => {
            let port = Config::load()?.dashboard_port;
            anyhow::ensure!(port != 0, "the dashboard is turned off (dashboard_port = 0)");
            let url = web::url(port);
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
                anyhow::bail!("nothing is listening on {url}: is the codeclock service running?");
            }
            tray::open_url(&url);
            println!("{url}");
            Ok(())
        }
        Command::Serve { port } => {
            let cfg = Config::load()?;
            let port = port.unwrap_or(cfg.dashboard_port);
            anyhow::ensure!(port != 0, "pick a port with --port");
            let dir = sync::repo_dir();
            let worker = if sync::is_enabled(&dir) {
                let interval = std::time::Duration::from_secs(cfg.sync.interval_secs.max(60));
                Some(sync::Worker::start(Store::default_path(), dir, cfg.machine(), interval)?)
            } else {
                None
            };
            web::serve(port, Store::default_path(), cfg.roots(), worker)
        }
    }
}

fn report_cmd(week: bool, all: bool, offline: bool) -> Result<()> {
    let store = Store::open(&Store::default_path())?;
    let dir = sync::repo_dir();
    if !offline && sync::is_enabled(&dir) {
        match sync::run(&store, &dir, &Config::load()?.machine(), true) {
            Ok(o) if o.online => {}
            Ok(o) => eprintln!("Could not reach the other machines, showing local data: {}", o.error.unwrap_or_default()),
            Err(e) => eprintln!("Sync failed, showing local data: {e:#}"),
        }
    }
    let (from, label) = if all {
        (0, "all time")
    } else if week {
        (report::start_of_week(), "this week")
    } else {
        (report::start_of_today(), "today")
    };
    let rows = store.totals(from, report::now())?;
    if rows.is_empty() {
        println!("No time recorded ({label}).");
        return Ok(());
    }
    let width = rows.iter().map(|(p, _)| p.len()).max().unwrap_or(7).max(7);
    println!("{:width$}  {label}", "project");
    for (p, s) in &rows {
        println!("{p:width$}  {}", fmt_dur(*s));
    }
    let total: i64 = rows.iter().map(|(_, s)| s).sum();
    println!("{:width$}  {}", "total", fmt_dur(total));
    Ok(())
}

fn sync_cmd(action: Option<SyncAction>) -> Result<()> {
    let cfg = Config::load()?;
    let machine = cfg.machine();
    let dir = sync::repo_dir();
    let store = Store::open(&Store::default_path())?;
    match action {
        Some(SyncAction::Init { repository, reuse }) => {
            sync::init(&repository, &dir, &machine, reuse)?;
            println!("Cloned {repository} into {}", dir.display());
            let o = sync::run(&store, &dir, &machine, true)?;
            println!("This machine is {machine}: sent {} changes, received {}.", o.sent, o.received);
            if let Some(e) = o.error {
                println!("Not sent yet: {e}");
            }
            println!("Restart the service to sync in the background: systemctl --user restart codeclock");
        }
        Some(SyncAction::Status) => {
            println!("machine:     {machine}");
            println!("repository:  {}", if sync::is_enabled(&dir) { dir.display().to_string() } else { "not set up".into() });
            let when = |k: &str| {
                store
                    .meta(k)
                    .ok()
                    .flatten()
                    .and_then(|v| v.parse::<i64>().ok())
                    .and_then(|t| chrono::TimeZone::timestamp_opt(&chrono::Local, t, 0).single())
                    .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "never".into())
            };
            println!("last sync:   {}", when("last_sync"));
            println!("last online: {}", when("last_online_sync"));
            if let Some(e) = store.meta("last_sync_error")?.filter(|e| !e.is_empty()) {
                println!("last error:  {e}");
            }
            println!("waiting:     {} changes", store.unsynced()?.len());
        }
        None => {
            let o = sync::run(&store, &dir, &machine, true)?;
            println!("Sent {} changes, received {}.", o.sent, o.received);
            if let Some(e) = o.error {
                println!("Offline, will retry later: {e}");
            }
        }
    }
    Ok(())
}

fn status_cmd() -> Result<()> {
    let cfg = Config::load()?;
    println!("config:       {}", Config::path().display());
    let projects = project::Projects::scan(&cfg.roots());
    println!("projects:     {} in {:?}", projects.names().len(), cfg.project_roots);
    for (folder, name) in projects.renamed() {
        println!("              folder {folder} counts as {name} (its git repository)");
    }
    let mut sampler = daemon::Sampler::new(cfg);
    let s = sampler.sample();
    println!("focus:        {}", sampler.focus.as_ref().map(|f| f.name()).unwrap_or("none"));
    match &s.window {
        Some(w) => println!("window:       {} | {:?} | pid {:?}", w.class, w.title, w.pid),
        None => println!("window:       (no focused window this source can see)"),
    }
    println!("editor:       {}", s.editor.as_deref().unwrap_or("-"));
    println!("project:      {}", s.project.as_deref().unwrap_or("-"));
    println!(
        "idle:         {} (source {})",
        s.observation.idle_secs.map(|i| format!("{i} s")).unwrap_or_else(|| "unknown".into()),
        sampler.session.as_ref().map(|x| x.idle_method()).unwrap_or("none")
    );
    println!("locked:       {}", if s.observation.locked { "yes" } else { "no" });
    println!("counting:     {}", if s.observation.work.is_some() && !s.observation.locked { "yes" } else { "no" });
    Ok(())
}

/// Runs editor and project detection on one X11 window, focused or not.
fn window_cmd(id: &str) -> Result<()> {
    let cfg = Config::load()?;
    let projects = project::Projects::scan(&cfg.roots());
    let raw = id.trim_start_matches("0x");
    let window = u32::from_str_radix(raw, 16).with_context(|| format!("bad window id {id}"))?;
    let w = platform::x11::X11Focus::connect()?.info(window)?;
    println!("window:   {} | {:?} | pid {:?} ({})", w.class, w.title, w.pid, w.process_name());
    let editor = cfg.editor_for(&w.class, &w.process_name());
    println!("editor:   {}", editor.map(|e| e.name.as_str()).unwrap_or("-"));
    let project = editor.and_then(|_| projects.detect(&w.title, w.cwd().as_deref()));
    println!("project:  {}", project.as_deref().unwrap_or("-"));
    Ok(())
}

fn install_service() -> Result<()> {
    let exe = std::env::current_exe().context("finding the codeclock executable")?;
    let dir: PathBuf = dirs::config_dir().context("no config dir")?.join("systemd/user");
    std::fs::create_dir_all(&dir)?;
    let unit = dir.join("codeclock.service");
    std::fs::write(
        &unit,
        format!(
            "[Unit]\n\
             Description=codeclock: time per project, counted from the editor\n\
             PartOf=graphical-session.target\n\
             After=graphical-session.target\n\n\
             [Service]\n\
             ExecStart={} run\n\
             Restart=on-failure\n\
             RestartSec=10\n\n\
             [Install]\n\
             WantedBy=graphical-session.target\n",
            exe.display()
        ),
    )?;
    for args in [vec!["daemon-reload"], vec!["enable", "--now", "codeclock.service"]] {
        let ok = std::process::Command::new("systemctl").arg("--user").args(&args).status()?.success();
        anyhow::ensure!(ok, "systemctl --user {} failed", args.join(" "));
    }
    println!("Service installed and started: {}", unit.display());
    Ok(())
}
