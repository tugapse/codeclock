//! Which project a focused editor window belongs to.
//!
//! Projects are the direct subfolders of the configured roots (`~/Code/shop`, `~/Code/web-app`…).
//! A project is named after its git repository (the `origin` remote), so a clone in a folder with
//! another name still counts as the same project on every machine; folders without a remote keep
//! their folder name.
//!
//! A window is matched by its title first (VS Code: "main.py — shop — Visual Studio Code", where
//! "shop" is the folder), then by the working directory of its process.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct Projects {
    roots: Vec<PathBuf>,
    /// Folder name -> project name.
    folders: BTreeMap<String, String>,
}

impl Projects {
    /// Scans the roots for project folders (hidden folders are skipped).
    pub fn scan(roots: &[PathBuf]) -> Self {
        let mut folders = BTreeMap::new();
        for root in roots {
            let Ok(entries) = std::fs::read_dir(root) else { continue };
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let folder = entry.file_name().to_string_lossy().to_string();
                    if !folder.starts_with('.') {
                        let name = repo_name(&entry.path()).unwrap_or_else(|| folder.clone());
                        folders.insert(folder, name);
                    }
                }
            }
        }
        Projects { roots: roots.to_vec(), folders }
    }

    #[cfg(test)]
    pub fn from_names(names: &[&str]) -> Self {
        Projects {
            roots: vec![PathBuf::from("/home/u/Code")],
            folders: names.iter().map(|n| (n.to_string(), n.to_string())).collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.folders.len()
    }

    /// Project names, sorted, without repeats (two clones of one repository are one project).
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.folders.values().cloned().collect();
        v.sort();
        v.dedup();
        v
    }

    /// Folders whose project name differs from the folder name: (folder, project).
    pub fn renamed(&self) -> Vec<(String, String)> {
        self.folders.iter().filter(|(f, n)| f != n).map(|(f, n)| (f.clone(), n.clone())).collect()
    }

    /// The project named in a window title: a title part (split on " - ", " — ", " – ")
    /// equal to a project folder. VS Code puts the workspace folder there.
    pub fn in_title(&self, title: &str) -> Option<String> {
        let title = strip_markers(title);
        let mut parts: Vec<&str> = vec![title.as_str()];
        for sep in [" — ", " – ", " - "] {
            parts = parts.iter().flat_map(|p| p.split(sep)).collect();
        }
        // The last matching part wins: the workspace comes after the file name.
        parts
            .iter()
            .rev()
            .map(|p| p.trim())
            .find_map(|p| self.folders.get(p).cloned())
    }

    /// The project a path lives in: its first folder below a root.
    pub fn of_path(&self, path: &Path) -> Option<String> {
        for root in &self.roots {
            if let Ok(rest) = path.strip_prefix(root) {
                let first = rest.components().next()?;
                let folder = first.as_os_str().to_string_lossy().to_string();
                if let Some(name) = self.folders.get(&folder) {
                    return Some(name.clone());
                }
            }
        }
        None
    }

    /// Title first, then the process's working directory.
    pub fn detect(&self, title: &str, cwd: Option<&Path>) -> Option<String> {
        self.in_title(title).or_else(|| cwd.and_then(|c| self.of_path(c)))
    }
}

/// The repository name of a git checkout, from the `origin` remote (else the first remote).
/// Reads the config file directly; also follows `.git` files (worktrees, submodules).
pub fn repo_name(dir: &Path) -> Option<String> {
    let dot_git = dir.join(".git");
    let config = if dot_git.is_dir() {
        dot_git.join("config")
    } else {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let gitdir = dir.join(text.strip_prefix("gitdir:")?.trim());
        // A worktree's git dir points at the main repository through `commondir`.
        match std::fs::read_to_string(gitdir.join("commondir")) {
            Ok(common) => gitdir.join(common.trim()).join("config"),
            Err(_) => gitdir.join("config"),
        }
    };
    let text = std::fs::read_to_string(config).ok()?;
    let urls = remote_urls(&text);
    let url = urls.iter().find(|(r, _)| r == "origin").or(urls.first())?;
    name_from_url(&url.1)
}

/// (remote, url) pairs from a git config file, in order.
fn remote_urls(config: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut remote: Option<String> = None;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            remote = line
                .strip_prefix("[remote")
                .and_then(|r| r.trim().strip_suffix(']'))
                .map(|r| r.trim().trim_matches('"').to_string());
        } else if let (Some(r), Some((k, v))) = (&remote, line.split_once('='))
            && k.trim() == "url"
        {
            out.push((r.clone(), v.trim().to_string()));
        }
    }
    out
}

/// `git@github.com:me/shop.git`, `https://github.com/me/shop`, `/srv/git/shop.git/` -> `shop`.
fn name_from_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    let name = url.rsplit(['/', ':', '\\']).next()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Drops VS Code's decorations that are not part of the folder name:
/// the dirty marker "● " and remote tags such as "[SSH: devbox]".
fn strip_markers(title: &str) -> String {
    let mut t = title.trim_start_matches('●').trim().to_string();
    // Look for the "]" only after the "[", so a stray "]" earlier in the title does not stop us.
    while let Some(start) = t.find('[') {
        match t[start..].find(']') {
            Some(len) => t.replace_range(start..=start + len, ""),
            None => break,
        }
    }
    t.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Projects {
        Projects::from_names(&["shop", "web-app", "vscode-ext", "codeclock"])
    }

    #[test]
    fn vs_code_title_gives_the_workspace() {
        assert_eq!(p().in_title("main.py — shop — Visual Studio Code").as_deref(), Some("shop"));
        assert_eq!(
            p().in_title("● app.ts - web-app - Visual Studio Code").as_deref(),
            Some("web-app")
        );
    }

    #[test]
    fn file_named_like_a_project_does_not_win_over_the_workspace() {
        assert_eq!(p().in_title("shop — codeclock — Visual Studio Code").as_deref(), Some("codeclock"));
    }

    #[test]
    fn remote_tag_and_unknown_folder() {
        assert_eq!(
            p().in_title("x.rs — codeclock [SSH: devbox] — Visual Studio Code").as_deref(),
            Some("codeclock")
        );
        assert_eq!(p().in_title("Welcome — Visual Studio Code"), None);
        assert_eq!(p().in_title("notes — other — Visual Studio Code"), None);
    }

    #[test]
    fn stray_brackets_do_not_stop_tag_removal() {
        assert_eq!(strip_markers("odd] name [SSH: x]"), "odd] name");
        assert_eq!(strip_markers("● a [one] b [two]"), "a  b");
        assert_eq!(strip_markers("open [bracket"), "open [bracket");
        assert_eq!(
            p().in_title("v2] notes.md — codeclock [WSL: Ubuntu] — Visual Studio Code").as_deref(),
            Some("codeclock")
        );
    }

    #[test]
    fn path_inside_a_project() {
        let proj = p();
        assert_eq!(proj.of_path(Path::new("/home/u/Code/shop/src/x.py")).as_deref(), Some("shop"));
        assert_eq!(proj.of_path(Path::new("/home/u/Code")), None);
        assert_eq!(proj.of_path(Path::new("/tmp/shop")), None);
        assert_eq!(
            proj.detect("Terminal", Some(Path::new("/home/u/Code/vscode-ext"))).as_deref(),
            Some("vscode-ext")
        );
    }

    #[test]
    fn repository_names_from_urls() {
        for url in [
            "git@github.com:you/shop.git",
            "https://github.com/you/shop",
            "https://github.com/you/shop.git/",
            "ssh://git@host:22/srv/shop.git",
            "/srv/git/shop.git",
            "C:\\repos\\shop",
        ] {
            assert_eq!(name_from_url(url).as_deref(), Some("shop"), "{url}");
        }
        assert_eq!(name_from_url(""), None);
    }

    #[test]
    fn origin_wins_over_other_remotes() {
        let cfg = "[core]\n\tbare = false\n[remote \"fork\"]\n\turl = git@github.com:me/fork.git\n\
                   [remote \"origin\"]\n\turl = https://github.com/you/shop.git\n\tfetch = +refs/heads/*\n\
                   [branch \"main\"]\n\tremote = origin\n";
        let urls = remote_urls(cfg);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls.iter().find(|(r, _)| r == "origin").unwrap().1, "https://github.com/you/shop.git");
    }

    #[test]
    fn clones_are_named_after_their_repository() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git_cfg = |folder: &str, url: &str| {
            std::fs::create_dir_all(root.join(folder).join(".git")).unwrap();
            std::fs::write(root.join(folder).join(".git/config"), format!("[remote \"origin\"]\n\turl = {url}\n")).unwrap();
        };
        git_cfg("shop", "git@github.com:you/shop.git");
        git_cfg("shop-test", "https://github.com/you/shop");
        std::fs::create_dir(root.join("scratch")).unwrap(); // no git: folder name
        // A worktree: `.git` is a file pointing into the main repository.
        std::fs::create_dir_all(root.join("shop/.git/worktrees/wt")).unwrap();
        std::fs::write(root.join("shop/.git/worktrees/wt/commondir"), "../..\n").unwrap();
        std::fs::create_dir(root.join("shop-wt")).unwrap();
        std::fs::write(root.join("shop-wt/.git"), format!("gitdir: {}\n", root.join("shop/.git/worktrees/wt").display())).unwrap();

        let p = Projects::scan(&[root.to_path_buf()]);
        assert_eq!(p.names(), vec!["scratch".to_string(), "shop".to_string()]);
        assert_eq!(p.in_title("x.rs — shop-test — Visual Studio Code").as_deref(), Some("shop"));
        assert_eq!(p.of_path(&root.join("shop-wt/src")).as_deref(), Some("shop"));
        assert_eq!(p.renamed(), vec![("shop-test".to_string(), "shop".to_string()), ("shop-wt".to_string(), "shop".to_string())]);
    }

    #[test]
    fn scan_skips_files_and_hidden_folders() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("alpha")).unwrap();
        std::fs::create_dir(dir.path().join(".cache")).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        let proj = Projects::scan(&[dir.path().to_path_buf()]);
        assert_eq!(proj.len(), 1);
        assert_eq!(proj.in_title("x — alpha — Visual Studio Code").as_deref(), Some("alpha"));
    }
}
