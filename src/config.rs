//! User configuration: which editors count, where projects live and the idle limit.
//!
//! Stored as TOML in `~/.config/codeclock/config.toml`. A missing file means the defaults
//! (VS Code, `~/Code`, 5 minutes).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Seconds without keyboard or mouse after which the whole pause stops counting.
    pub idle_timeout_secs: u64,
    /// How often the focused window and idle time are sampled.
    pub sample_secs: u64,
    /// Folders whose direct subfolders are projects (`~` is expanded).
    pub project_roots: Vec<String>,
    /// Port of the history dashboard on 127.0.0.1; 0 turns it off.
    pub dashboard_port: u16,
    /// Editors whose focused, active windows count as work.
    pub editors: Vec<Editor>,
    /// Syncing between machines (`codeclock sync init <repository>` turns it on).
    pub sync: SyncSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SyncSettings {
    /// This machine's folder in the sync repository; empty means `<hostname>-<os>`.
    pub machine: String,
    /// Seconds between syncs while the service runs (it also syncs at start and stop).
    pub interval_secs: u64,
}

impl Default for SyncSettings {
    fn default() -> Self {
        SyncSettings { machine: String::new(), interval_secs: 900 }
    }
}

/// An editor is recognised by its window class (X11 `WM_CLASS`) or by its process name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Editor {
    pub name: String,
    #[serde(default)]
    pub window_classes: Vec<String>,
    #[serde(default)]
    pub process_names: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            idle_timeout_secs: 300,
            sample_secs: 5,
            project_roots: vec!["~/Code".into()],
            dashboard_port: 8417,
            editors: vec![Editor {
                name: "VS Code".into(),
                window_classes: vec!["code".into(), "Code".into(), "code_code".into(), "code-url-handler".into()],
                process_names: vec!["code".into(), "code-oss".into(), "codium".into()],
            }],
            sync: SyncSettings::default(),
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("codeclock")
            .join("config.toml")
    }

    /// Loads the config file, or the defaults when it does not exist.
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Writes the defaults to the config path when no file is there yet.
    pub fn write_default_if_missing() -> Result<PathBuf> {
        let path = Self::path();
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, toml::to_string_pretty(&Config::default())?)?;
        }
        Ok(path)
    }

    /// The name of this machine in the sync repository.
    pub fn machine(&self) -> String {
        match self.sync.machine.trim() {
            "" => crate::sync::default_machine(),
            m => crate::sync::clean_name(m),
        }
    }

    /// Project roots with `~` expanded.
    pub fn roots(&self) -> Vec<PathBuf> {
        self.project_roots.iter().map(|r| expand_home(r)).collect()
    }

    /// The editor a window belongs to, by window class or process name (case-insensitive).
    pub fn editor_for(&self, window_class: &str, process_name: &str) -> Option<&Editor> {
        let class = window_class.to_lowercase();
        let process = process_name.to_lowercase();
        self.editors.iter().find(|e| {
            (!class.is_empty() && e.window_classes.iter().any(|c| c.to_lowercase() == class))
                || (!process.is_empty()
                    && e.process_names.iter().any(|p| p.to_lowercase() == process))
        })
    }
}

pub fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    if path == "~"
        && let Some(home) = dirs::home_dir() {
            return home;
        }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_count_vs_code_with_five_minutes() {
        let c = Config::default();
        assert_eq!(c.idle_timeout_secs, 300);
        assert!(c.editor_for("Code", "").is_some());
        assert!(c.editor_for("", "code").is_some());
        assert!(c.editor_for("firefox", "firefox").is_none());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "idle_timeout_secs = 120\n[[editors]]\nname = \"Neovim\"\nprocess_names = [\"nvim\"]\n",
        )
        .unwrap();
        let c = Config::load_from(&path).unwrap();
        assert_eq!(c.idle_timeout_secs, 120);
        assert_eq!(c.sample_secs, 5);
        assert_eq!(c.editors.len(), 1);
        assert!(c.editor_for("", "nvim").is_some());
        assert!(c.editor_for("Code", "code").is_none());
    }

    #[test]
    fn default_config_round_trips() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, Config::default());
    }
}
