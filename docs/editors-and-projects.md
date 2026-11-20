# Editors and projects

## Which windows count

A focused window counts when it belongs to an editor from your list, matched by its window class or by its process name (case-insensitive):

```toml
[[editors]]
name = "VS Code"
window_classes = ["code", "Code", "code_code", "code-url-handler"]
process_names = ["code", "code-oss", "codium"]
```

To add an editor, find its window class with `codeclock status` while it has focus (or `xprop WM_CLASS` on X11) and add an `[[editors]]` block. Writing your own `[[editors]]` list replaces the default one, so keep the VS Code block if you still use it.

### Caveats per editor

| Editor | Notes |
| --- | --- |
| VS Code (deb, rpm, AUR, tarball) | Class `code` on X11 and on Wayland (KDE). Works out of the box. |
| VS Code snap (Ubuntu) | Class `code_code`. Included in the defaults. |
| VS Code flatpak | The class matches, but the process id is inside the sandbox, so the process name and working directory cannot be read. Projects are found from the window title only. |
| VSCodium, Code - OSS | Process names `codium` and `code-oss` are in the defaults; add their window classes if needed. |
| JetBrains IDEs, Zed, Sublime Text | Work once added to `[[editors]]`, as long as the window title contains the project folder name (they do by default). |
| Vim, Neovim, Helix, Emacs in a terminal | Not detected: the focused window is the terminal, not the editor. Adding the terminal to `[[editors]]` would count all terminal time. |

## How a project is found

Projects are the direct subfolders of `project_roots` (default `~/Code`). Hidden folders are skipped, and new folders are picked up within a minute.

1. **Title first.** The window title is split on ` — `, ` – ` and ` - `, and the last part that is a project folder wins. VS Code shows `file — folder — Visual Studio Code`, so the folder is found even when a file has the same name as another project. The unsaved marker `●` and tags such as `[SSH: host]` or `[WSL: Ubuntu]` are ignored.
2. **Then the working directory** of the editor's process, if it is inside a project folder.
3. **No match, no count.** A window that matches no project (the welcome page, a file outside your roots) does not count.

Only the first level counts: a VS Code window opened on `~/Code/webshop/server` belongs to `webshop` if its title says `webshop`; if it says `server`, it matches only when `server` is itself a project folder.

## Project names come from git

Each project is named after its git repository, read from the `origin` remote in `.git/config` (or the first remote when there is no `origin`). So:

- A clone in a folder with another name (`~/Code/webshop-test`) counts as the same project (`webshop`), on every machine.
- Worktrees and submodules, where `.git` is a file, are followed to their repository.
- A folder without git, or without a remote, keeps its folder name.

`codeclock status` lists the folders whose project name differs from the folder name. When the service starts, blocks this machine recorded under such a folder name move to the repository name, and the change syncs.

**Renamed repositories:** if you rename a repository on GitHub, existing clones keep the old remote URL, and their time counts under the old name until you update the remote (`git remote set-url origin <new url>`).

## Project roots per machine

`project_roots` is per machine, so each machine can keep its code wherever it likes:

```toml
project_roots = ["~/Code"]                    # Linux
project_roots = ["C:/Users/you/source/repos"] # Windows, when supported
```

Because names come from git, the same repository counts as one project everywhere.
