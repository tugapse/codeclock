//! Carries an installation over from codeclock's old name, "playtime": stops the old service and
//! moves the config, the database and the sync clone to their new places. Runs once; after that
//! the old folders are gone and this does nothing.

use std::path::{Path, PathBuf};
use std::process::Command;

const OLD: &str = "playtime";
const NEW: &str = "codeclock";

pub fn migrate() {
    // Someone trying things on a copy (tests, screenshots) must not move the real data.
    if std::env::var_os("CODECLOCK_DB").is_some() || std::env::var_os("CODECLOCK_SYNC_DIR").is_some() {
        return;
    }
    let (Some(data), Some(config)) = (dirs::data_local_dir(), dirs::config_dir()) else { return };
    let old_data = data.join(OLD);
    let old_config = config.join(OLD);
    let old_unit = config.join("systemd/user").join(format!("{OLD}.service"));
    if !old_data.exists() && !old_config.exists() && !old_unit.exists() {
        return;
    }

    // The old service writes to the database; stop it before moving anything.
    if old_unit.exists() {
        let _ = Command::new("systemctl").args(["--user", "disable", "--now", &format!("{OLD}.service")]).status();
        match std::fs::remove_file(&old_unit) {
            Ok(()) => eprintln!("Stopped and removed the old {OLD} service"),
            Err(e) => eprintln!("Could not remove {}: {e}", old_unit.display()),
        }
        let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).status();
    }

    if move_dir(&old_data, &data.join(NEW)) {
        let dir = data.join(NEW);
        for suffix in ["", "-wal", "-shm"] {
            let from = dir.join(format!("{OLD}.db{suffix}"));
            if from.exists() {
                let _ = std::fs::rename(&from, dir.join(format!("{NEW}.db{suffix}")));
            }
        }
        let clone = dir.join("sync");
        if clone.join(".git").exists() {
            let _ = std::fs::remove_file(clone.join(".git").join(format!("{OLD}.lock")));
            for (key, value) in [("user.name", NEW.to_string()), ("user.email", git_email(&clone))] {
                let _ = Command::new("git").arg("-C").arg(&clone).args(["config", key, &value]).status();
            }
        }
    }
    move_dir(&old_config, &config.join(NEW));
}

/// Moves `from` to `to` unless `to` already exists; says what it did.
fn move_dir(from: &Path, to: &PathBuf) -> bool {
    if !from.exists() {
        return false;
    }
    if to.exists() {
        eprintln!("Both {} and {} exist; using the new one and leaving the old one alone", from.display(), to.display());
        return false;
    }
    match std::fs::rename(from, to) {
        Ok(()) => {
            eprintln!("Moved {} to {}", from.display(), to.display());
            true
        }
        Err(e) => {
            eprintln!("Could not move {} to {}: {e}", from.display(), to.display());
            false
        }
    }
}

/// The sync commits' e-mail with the new name, keeping the machine part (`playtime@<machine>`).
fn git_email(clone: &Path) -> String {
    let old = Command::new("git")
        .arg("-C")
        .arg(clone)
        .args(["config", "user.email"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    match old.split_once('@') {
        Some((_, machine)) if !machine.is_empty() => format!("{NEW}@{machine}"),
        _ => format!("{NEW}@localhost"),
    }
}
