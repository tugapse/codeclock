# Troubleshooting

Start with `codeclock status`, run from another window while the editor has focus (`sleep 3; codeclock status` gives you time to switch back). It shows what codeclock sees: focus source, window, editor, project, idle time and lock.

## Nothing is counted

| `codeclock status` shows | Cause | Fix |
| --- | --- | --- |
| `focus: none` | No focus module for this desktop | See [Desktops and systems](desktops.md) |
| `window: (no focused window this source can see)` while the editor has focus | The editor is a native Wayland window on a desktop where codeclock only sees X11 | On GNOME, start the editor in X11 mode; see [Desktops and systems](desktops.md) |
| `editor: -` | The window class and process name are not in `[[editors]]` | Add them; see [Editors and projects](editors-and-projects.md) |
| `project: -` | The open folder is not directly under a project root | Open the project folder itself, or add its parent to `project_roots` |
| `locked: yes` while unlocked | The locker leaves logind's hint set | Report it with your desktop and locker |

## Pauses keep counting

`idle: unknown` means no idle source works on this desktop, so codeclock cannot tell that you stepped away. `idle: 0 s (source logind)` that stays at 0 after a long pause means the same: the desktop does not keep logind's idle hint. See the idle notes in [Desktops and systems](desktops.md).

## No tray icon

codeclock keeps counting without the icon and retries every 30 seconds, so the icon appears once a tray is available (for example after unlocking the screen). On GNOME you need an AppIndicator extension. Check the log for the reason:

```sh
journalctl --user -u codeclock | grep -i tray
```

## The dashboard does not open

`codeclock open` says whether anything listens on the port. If another program uses 8417, set another `dashboard_port` and restart the service.

## Sync says offline

Run `codeclock sync` to see git's message. Usually the machine has no credentials for the repository: run `gh auth login`, or add an SSH key and use the `git@github.com:` address. `codeclock sync status` shows the last error.

## Logs

```sh
journalctl --user -u codeclock -f
```

For more detail, run `systemctl --user edit codeclock`, add `Environment=RUST_LOG=debug` under `[Service]`, and restart the service.

## Trying things on a copy

Set `CODECLOCK_DB=/path/to/copy.db` (and `CODECLOCK_SYNC_DIR` for a separate sync clone) for any command, for example:

```sh
CODECLOCK_DB=copy.db codeclock serve --port 8418
```
