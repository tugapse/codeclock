# Syncing between machines

Each machine counts its own time and syncs through a private git repository, so every machine shows the same totals per project, wherever the work happened.

## Setting it up

```sh
gh repo create codeclock-data --private          # once, from any machine
codeclock sync init https://github.com/you/codeclock-data.git
systemctl --user restart codeclock
```

Run `codeclock sync init` on every machine. git uses the credentials the machine already has: `gh auth login` for HTTPS addresses, or an SSH key for `git@github.com:` addresses.

## When it syncs

- When the service starts, every 15 minutes (`[sync] interval_secs`) and when it stops (logout, shutdown).
- When you open the dashboard, or click "Synced …" in its header.
- Before `codeclock report` prints (skip it with `--offline`).
- Any time with `codeclock sync`.

Without network, everything keeps working locally; the changes wait in a local commit and go out with the next sync.

## How it works

- Each machine writes only to its own folder, `machines/<machine>/<YYYY-MM>.jsonl`, one JSON line per change (a block added, edited or deleted). Because no machine ever writes another machine's files, git never has conflicts.
- Every machine reads every folder and applies the changes to its own database. When two machines change the same block, the most recent change wins.
- Deleting a block in the dashboard leaves a mark, so the deletion reaches the other machines and an older copy cannot bring the block back.
- The SQLite database never leaves the machine. The local clone lives in `~/.local/share/codeclock/sync`.

## Machine names

Each machine needs its own name, which is also its folder in the repository. The default is `<hostname>-<os>`, for example `laptop-linux`. Set another one in the config:

```toml
[sync]
machine = "laptop"
```

`codeclock sync init` refuses a name another installation already uses, because two installations writing the same folder would conflict. If it is the same machine reinstalled, run it with `--reuse` to carry on with its old name.

## Caveats

- **Clocks:** "the most recent change wins" compares the machines' clocks. Keep them synced (NTP, which every desktop distribution enables by default).
- **Privacy:** the repository holds project names, editor names and times. Keep it private.
- **Size:** about 1 MB per machine per year of normal use.

## Checking

`codeclock sync status` shows the machine name, the last sync, the last sync that reached the remote, any error, and how many changes are waiting.
