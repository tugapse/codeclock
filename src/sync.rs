//! Syncing blocks between machines through a private git repository.
//!
//! Each machine appends the changes it makes to `machines/<machine>/<YYYY-MM>.jsonl` and never
//! writes anywhere else, so pulls and pushes never conflict. Every machine reads every file and
//! applies the changes to its own database; the newest change to a block wins (`Store::apply`).
//! The SQLite file itself never goes into git.
//!
//! A sync: write local changes and commit, pull (rebase), read the other machines' files, push.
//! Without network the commit stays local and goes out with the next sync.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::store::{Event, Store};

/// `<hostname>-<os>`, e.g. `laptop-linux`. The OS is part of it because a dual-boot machine
/// usually keeps its hostname, and each system has its own database.
pub fn default_machine() -> String {
    let host = std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .or_else(|| fs::read_to_string("/etc/hostname").ok())
        .unwrap_or_else(|| "machine".into());
    format!("{}-{}", clean_name(&host), std::env::consts::OS)
}

/// Lowercase letters, digits and dashes only: the name is a folder in the repository.
pub fn clean_name(name: &str) -> String {
    let s: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() { "machine".into() } else { s }
}

/// The local clone: `$CODECLOCK_SYNC_DIR`, else next to the database.
pub fn repo_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("CODECLOCK_SYNC_DIR") {
        return PathBuf::from(p);
    }
    Store::default_path().with_file_name("sync")
}

pub fn is_enabled(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// What one sync did.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Outcome {
    pub at: i64,
    /// Changes written to this machine's files.
    pub sent: usize,
    /// Changes from the sync files that changed the database.
    pub received: usize,
    /// Whether pull and push both reached the remote.
    pub online: bool,
    /// The first problem, if any; local work still happens without network.
    pub error: Option<String>,
}

fn git(dir: &Path, args: &[&str]) -> Result<Output> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        // Never wait for a password prompt; fail fast without network.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes -o ConnectTimeout=10")
        .output()
        .context("running git (is it installed?)")?;
    Ok(out)
}

fn git_ok(dir: &Path, args: &[&str]) -> Result<String> {
    let out = git(dir, args)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("git {}: {}", args.join(" "), err.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Clones the repository and prepares it for this machine. Refuses a machine name another
/// installation already writes to, unless `reuse` (the same machine, reinstalled).
pub fn init(url: &str, dir: &Path, machine: &str, reuse: bool) -> Result<()> {
    if is_enabled(dir) {
        bail!("{} is already a sync repository", dir.display());
    }
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    let out = Command::new("git")
        .args(["clone", "--quiet", url])
        .arg(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("running git (is it installed?)")?;
    if !out.status.success() {
        bail!("git clone {url}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let own = dir.join("machines").join(machine);
    let taken = fs::read_dir(&own).map(|mut d| d.next().is_some()).unwrap_or(false);
    if taken && !reuse {
        fs::remove_dir_all(dir)?;
        bail!(
            "the repository already has data for a machine called {machine}.\n\
             If this is that machine reinstalled, run again with --reuse.\n\
             Otherwise give this machine its own name in the config ([sync] machine = \"...\") and run again."
        );
    }
    // Commits made by codeclock, not by you; nothing personal goes into them.
    git_ok(dir, &["config", "user.name", "codeclock"])?;
    git_ok(dir, &["config", "user.email", &format!("codeclock@{machine}")])?;
    fs::create_dir_all(dir.join("machines").join(machine))?;
    Ok(())
}

/// Runs one sync. `network: false` only writes and reads local files.
pub fn run(store: &Store, dir: &Path, machine: &str, network: bool) -> Result<Outcome> {
    if !is_enabled(dir) {
        bail!("sync is not set up: run `codeclock sync init <repository>`");
    }
    // Several processes may sync at once (service, `report`, dashboard): one at a time.
    let lock = File::create(dir.join(".git").join("codeclock.lock"))?;
    lock.lock()?;

    let mut out = Outcome { at: chrono::Local::now().timestamp(), ..Default::default() };
    let note = |e: anyhow::Error, out: &mut Outcome| {
        log::warn!("sync: {e:#}");
        out.error.get_or_insert_with(|| format!("{e:#}"));
    };

    out.sent = export(store, dir, machine)?;

    let has_upstream = git(dir, &["rev-parse", "--verify", "--quiet", "@{u}"])?.status.success();
    let mut online = network;
    if network && has_upstream
        && let Err(e) = git_ok(dir, &["pull", "--rebase", "--quiet"])
    {
        // Never leave the clone half-way through a rebase.
        let _ = git(dir, &["rebase", "--abort"]);
        note(e, &mut out);
        online = false;
    }

    out.received = import(store, dir)?;

    if online {
        let push = if has_upstream {
            git_ok(dir, &["push", "--quiet"])
        } else {
            git_ok(dir, &["push", "--quiet", "--set-upstream", "origin", "HEAD"])
        };
        if let Err(e) = push {
            // Someone pushed in between: take their commits and try once more.
            let retry = git_ok(dir, &["pull", "--rebase", "--quiet"]).and_then(|_| git_ok(dir, &["push", "--quiet"]));
            if let Err(e2) = retry {
                let _ = git(dir, &["rebase", "--abort"]);
                note(e.context(e2.to_string()), &mut out);
                online = false;
            } else {
                out.received += import(store, dir)?;
            }
        }
    }
    out.online = online;

    store.set_meta("last_sync", &out.at.to_string())?;
    store.set_meta("last_sync_error", out.error.as_deref().unwrap_or(""))?;
    if online {
        store.set_meta("last_online_sync", &out.at.to_string())?;
    }
    Ok(out)
}

/// Writes local changes to this machine's file for the current month and commits them.
fn export(store: &Store, dir: &Path, machine: &str) -> Result<usize> {
    let events = store.unsynced()?;
    if events.is_empty() {
        return Ok(0);
    }
    let folder = dir.join("machines").join(machine);
    fs::create_dir_all(&folder)?;
    let file = folder.join(format!("{}.jsonl", chrono::Local::now().format("%Y-%m")));
    let mut text = String::new();
    for e in &events {
        text.push_str(&serde_json::to_string(e)?);
        text.push('\n');
    }
    OpenOptions::new().create(true).append(true).open(&file)?.write_all(text.as_bytes())?;

    let rel = format!("machines/{machine}");
    git_ok(dir, &["add", "--", &rel])?;
    git_ok(dir, &["commit", "--quiet", "-m", &format!("{machine}: {} changes", events.len()), "--", &rel])?;
    for e in &events {
        store.mark_synced(e.id(), e.at())?;
    }
    Ok(events.len())
}

/// Applies every new line of every machine's files. Files only grow, so each one is read
/// from where the last read stopped; applying a line twice changes nothing.
fn import(store: &Store, dir: &Path) -> Result<usize> {
    let mut files = Vec::new();
    let machines = dir.join("machines");
    if let Ok(entries) = fs::read_dir(&machines) {
        for m in entries.flatten().filter(|e| e.path().is_dir()) {
            for f in fs::read_dir(m.path())?.flatten() {
                if f.path().extension().is_some_and(|x| x == "jsonl") {
                    files.push(f.path());
                }
            }
        }
    }
    files.sort();

    let mut changed = 0;
    for path in files {
        let key = path.strip_prefix(dir).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        let mut f = File::open(&path)?;
        let len = f.metadata()?.len();
        let mut offset = store.file_offset(&key)?;
        if offset > len {
            offset = 0; // the file was replaced: read it again
        }
        if offset == len {
            continue;
        }
        f.seek(SeekFrom::Start(offset))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        // Only complete lines; a line still being written is read next time.
        let complete = buf.iter().rposition(|b| *b == b'\n').map(|i| i + 1).unwrap_or(0);
        for line in buf[..complete].split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            match serde_json::from_slice::<Event>(line) {
                Ok(e) => changed += store.apply(&e)? as usize,
                Err(e) => log::warn!("sync: skipping a bad line in {key}: {e}"),
            }
        }
        store.set_file_offset(&key, offset + complete as u64)?;
    }
    Ok(changed)
}

/// What the dashboard shows about syncing.
pub fn status(store: &Store, dir: &Path) -> serde_json::Value {
    let num = |k: &str| store.meta(k).ok().flatten().and_then(|v| v.parse::<i64>().ok());
    serde_json::json!({
        "enabled": is_enabled(dir),
        "last": num("last_sync"),
        "last_online": num("last_online_sync"),
        "error": store.meta("last_sync_error").ok().flatten().filter(|e| !e.is_empty()),
    })
}

/// A background thread that syncs at start, every `interval`, and on request.
#[derive(Clone)]
pub struct Worker {
    tx: Sender<Sender<Result<Outcome, String>>>,
}

impl Worker {
    pub fn start(db: PathBuf, dir: PathBuf, machine: String, interval: Duration) -> Result<Self> {
        let (tx, rx): (_, Receiver<Sender<Result<Outcome, String>>>) = mpsc::channel();
        let store = Store::open(&db)?;
        std::thread::Builder::new().name("sync".into()).spawn(move || {
            let sync = || run(&store, &dir, &machine, true).map_err(|e| format!("{e:#}"));
            let log_result = |r: &Result<Outcome, String>| match r {
                Ok(o) if o.sent + o.received > 0 => {
                    log::info!("sync: sent {}, received {}{}", o.sent, o.received, if o.online { "" } else { " (offline)" })
                }
                Ok(_) => {}
                Err(e) => log::warn!("sync failed: {e}"),
            };
            log_result(&sync());
            loop {
                match rx.recv_timeout(interval) {
                    Ok(reply) => {
                        let r = sync();
                        log_result(&r);
                        let _ = reply.send(r);
                    }
                    Err(RecvTimeoutError::Timeout) => log_result(&sync()),
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        })?;
        Ok(Worker { tx })
    }

    /// Syncs now and waits for the result, up to `timeout`.
    pub fn sync_now(&self, timeout: Duration) -> Result<Outcome, String> {
        let (reply, rx) = mpsc::channel();
        self.tx.send(reply).map_err(|_| "the sync thread stopped".to_string())?;
        rx.recv_timeout(timeout).map_err(|_| "sync is taking long; it goes on in the background".to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Sink;

    fn sh(dir: &Path, args: &[&str]) {
        let ok = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn names_are_folder_safe() {
        assert_eq!(clean_name(" Laptop\n"), "laptop");
        assert_eq!(clean_name("DESKTOP_AB 12"), "desktop-ab-12");
        assert_eq!(clean_name("..."), "machine");
        assert!(default_machine().ends_with(std::env::consts::OS));
    }

    /// Two machines and a bare "remote": blocks, edits and deletions reach both sides.
    #[test]
    fn two_machines_meet_through_the_remote() {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        fs::create_dir(&remote).unwrap();
        sh(&remote, &["init", "--quiet", "--bare"]);
        let url = remote.to_string_lossy().to_string();

        let (dir_a, dir_b) = (tmp.path().join("a"), tmp.path().join("b"));
        let mut a = Store::open(&tmp.path().join("a.db")).unwrap();
        let b = Store::open(&tmp.path().join("b.db")).unwrap();
        a.set_machine("laptop-linux");

        init(&url, &dir_a, "laptop-linux", false).unwrap();
        let id = a.open_block("shop", "VS Code", 1000).unwrap();
        a.set_end(id, 4600).unwrap();
        let o = run(&a, &dir_a, "laptop-linux", true).unwrap();
        assert_eq!((o.sent, o.online), (1, true), "{:?}", o.error);

        init(&url, &dir_b, "arch-linux", false).unwrap();
        let err = init(&url, &tmp.path().join("c"), "laptop-linux", false).unwrap_err();
        assert!(err.to_string().contains("--reuse"), "{err}");
        assert!(!tmp.path().join("c").exists());
        let o = run(&b, &dir_b, "arch-linux", true).unwrap();
        assert_eq!(o.received, 1, "{:?}", o.error);
        assert_eq!(b.projects().unwrap(), vec![("shop".into(), 3600)]);
        assert_eq!(b.blocks(0, 10_000).unwrap()[0].machine, "laptop-linux");

        // B fixes the project; A sees the fix.
        std::thread::sleep(Duration::from_millis(1100)); // a newer second for "newest wins"
        let bid = b.blocks(0, 10_000).unwrap()[0].id;
        b.update_block(bid, "codeclock", 1000, 4600).unwrap();
        assert_eq!(run(&b, &dir_b, "arch-linux", true).unwrap().sent, 1);
        run(&a, &dir_a, "laptop-linux", true).unwrap();
        assert_eq!(a.projects().unwrap(), vec![("codeclock".into(), 3600)]);

        // A deletes it; B loses it too, and nothing is sent back.
        std::thread::sleep(Duration::from_millis(1100));
        a.delete_block(id).unwrap();
        run(&a, &dir_a, "laptop-linux", true).unwrap();
        let o = run(&b, &dir_b, "arch-linux", true).unwrap();
        assert_eq!((o.received, o.sent), (1, 0));
        assert!(b.projects().unwrap().is_empty());

        // Each machine only ever wrote its own folder.
        let folders: Vec<_> = fs::read_dir(dir_a.join("machines")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(folders.len(), 2);
    }

    #[test]
    fn offline_keeps_the_commit_for_later() {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        fs::create_dir(&remote).unwrap();
        sh(&remote, &["init", "--quiet", "--bare"]);
        let dir = tmp.path().join("a");
        init(&remote.to_string_lossy(), &dir, "laptop-linux", false).unwrap();
        let mut s = Store::open(&tmp.path().join("a.db")).unwrap();
        let id = s.open_block("shop", "VS Code", 0).unwrap();
        s.set_end(id, 60).unwrap();

        let o = run(&s, &dir, "laptop-linux", false).unwrap();
        assert_eq!((o.sent, o.online), (1, false));
        assert!(s.unsynced().unwrap().is_empty(), "written locally, not sent again");
        let o = run(&s, &dir, "laptop-linux", true).unwrap();
        assert!(o.online, "{:?}", o.error);
        let log = git_ok(&remote, &["log", "--oneline", "--all"]).unwrap();
        assert!(log.contains("laptop-linux: 1 changes"));
    }
}
