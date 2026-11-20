# Desktops and systems

codeclock needs three things from the desktop: which window has focus, how long since you last used the keyboard or mouse, and whether the screen is locked. Each desktop offers them differently, so support depends on where you run it.

## At a glance

| Desktop | Focused window | Idle time | Locked screen | Tray icon |
| --- | --- | --- | --- | --- |
| GNOME on X11 | Yes (X11) | Yes (Mutter) | Yes | With an AppIndicator extension |
| GNOME on Wayland | Only XWayland apps (X11) | Yes (Mutter) | Yes | With an AppIndicator extension |
| KDE Plasma 5.27 and 6 on Wayland | Yes (KWin script) | Yes (Wayland `ext-idle-notify`) | Yes | Yes |
| KDE Plasma on X11 | Yes (KWin script) | Often not (see below) | Yes | Yes |
| Xfce, Cinnamon, MATE and other X11 desktops | Yes (X11) | Usually (`org.freedesktop.ScreenSaver`) | Depends on the locker | Usually |
| i3 and other bare X11 window managers | Yes (X11), if the WM sets `_NET_ACTIVE_WINDOW` | Often not | Often not | Only with a tray bar |
| Sway, Hyprland and other wlroots compositors | Not yet | Yes (Wayland `ext-idle-notify`) | Depends on the locker | With a tray bar (waybar) |
| Windows, macOS | Not yet | Not yet | Not yet | Not yet |

`codeclock status` shows which focus and idle source it picked on your machine.

## GNOME

**Wayland:** GNOME does not let apps see the focused window, so codeclock reads it through X11, which only covers apps running under XWayland. VS Code runs under XWayland unless it is started in native Wayland mode. If `codeclock status` shows no window while VS Code has focus, start VS Code in X11 mode: add `--ozone-platform=x11` to `~/.config/code-flags.conf` (Arch and others) or to the launcher. The snap package on Ubuntu already does this.

**Tray icon:** GNOME has no tray of its own. Ubuntu ships the AppIndicator extension; on other distributions install "AppIndicator and KStatusNotifierItem Support". GNOME cannot show text next to the icon, so the times appear in its tooltip and menu.

## KDE Plasma

When KWin is running, codeclock asks it to load a small script that reports every change of the focused window, or of its title, over D-Bus. This works for native Wayland and XWayland windows alike, so VS Code needs no special flags.

- Each running codeclock loads its own copy of the script (named `codeclock-<pid>`) and removes it when it exits. You may see it in System Settings, Window Management, KWin Scripts while codeclock runs.
- Only calls that come from KWin itself are accepted.
- If KWin restarts, codeclock loads the script again within a minute.

**Idle time:** Plasma refuses the usual `org.freedesktop.ScreenSaver` idle query and does not keep logind's idle hint, so on Wayland codeclock asks KWin directly through the `ext-idle-notify` protocol (Plasma 5.27 and later). It only listens to the keyboard and mouse, so a playing video does not hide that you stepped away. On Plasma under X11 there is no such protocol: there `codeclock status` may show `idle: 0 s (source logind)` even after a long pause, and long pauses keep counting until the screen locks.

## Other X11 desktops and window managers

The focused window comes from `_NET_ACTIVE_WINDOW`, which every EWMH window manager sets (Xfce, Cinnamon, MATE, Openbox, i3 and most others).

**Idle time is the weak spot.** codeclock tries Mutter, then `org.freedesktop.ScreenSaver`, then the Wayland compositor, then logind's idle hint. Bare window managers often provide none of them. When no idle source works, codeclock cannot tell that you walked away, so a long pause in front of the editor keeps counting. `codeclock status` shows `idle: unknown` in that case, or `idle: 0 s (source logind)` that never grows when the desktop does not maintain logind's hint. Running a session that provides `org.freedesktop.ScreenSaver` (for example xfce4-screensaver or xscreensaver with its D-Bus bridge) fixes it.

## Sway, Hyprland and other wlroots compositors

Not supported yet: these compositors do not expose the focused window to X11 for native Wayland apps, and they need their own focus module (planned). Apps under XWayland may be seen, but this is untested. Idle time already works through `ext-idle-notify`.

## Locked screen

codeclock reads logind's `LockedHint`. GNOME and KDE set it. Some standalone lockers (i3lock, swaylock, slock) do not; there a locked screen is not detected directly, but the idle limit still stops counting after 5 minutes without input.

## Suspend

Works everywhere: a gap between two samples longer than 4 sample periods (at least 30 seconds) is treated as a suspend and never counted.

## Windows and macOS

Not supported yet. The sync format already works across systems, so a future Windows build can join the same repository; only the focus and idle modules are missing.

## Release binaries

The Linux binary on the releases page is built on Ubuntu 24.04 and needs glibc 2.39 or newer (Ubuntu 24.04, Fedora 40, current Arch). On older systems, build from source with `cargo build --release`.
