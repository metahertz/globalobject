# Using GlobalObject

GlobalObject backs up your folders, encrypted on your machine, onto other pool members' machines. In return you
share some of your disk with them. You need Linux or macOS. Windows isn't supported yet.

## Joining

Someone running a pool sends you a join link like `https://cp.example.org/join/1a2b…`. Each link works once. Open
it, or run:

```sh
curl -fsSL https://cp.example.org/install.sh | sh -s -- --join https://cp.example.org/join/1a2b…
```

This installs `globalobject` and `globalobject-node` to `~/.local/bin`. Add `--system` to install to
`/usr/local/bin` instead. Then `globalobject setup` asks:

1. **Your recovery phrase.** Setup makes 12 words, then asks you to type three of them back. **Write the words
   down and keep them somewhere safe and offline.** They are the only way to restore your backups on another
   machine. Nobody can recover them for you, including the operator.
2. **How much disk to share.** The default is a quarter of your free space. With the usual 3 copies, you get
   about a quarter of what you share as backup space.
3. **Which folders to back up.** The default is Documents, Pictures and Desktop, where they exist.
4. **How often.** Every 24 hours by default.

Setup then installs a background service. It is a systemd user service on Linux and a LaunchAgent on macOS, and it
runs as you, not as root. It does three things:

- serves your shared disk to the pool
- runs backups on schedule
- serves a dashboard at `http://127.0.0.1:7410`

On Linux, setup also turns on lingering (`loginctl enable-linger`), so the service keeps running after you log
out. If it can't, it tells you the command to run.

## Day to day

| Command | What it does |
|---|---|
| `globalobject status` | A health check: account, service, last backup, space used. Start here if something seems wrong. |
| `globalobject dashboard` | Opens the dashboard in your browser, already signed in |
| `globalobject backup` | Back up now, instead of waiting for the schedule |
| `globalobject snapshots` | List snapshots |
| `globalobject restore <snapshot> <folder>` | Restore a snapshot into a folder |
| `globalobject paths add ~/Music` | Back up another folder. `paths remove` stops backing one up. |
| `globalobject slice 1TB` | Share more or less disk |
| `globalobject invite` | Make a join link for someone else, if your pool allows it |
| `globalobject service logs -f` | Follow the service's logs |
| `globalobject service restart` | Restart the background service |

The dashboard shows the same health information. It also lists snapshots with a Restore button, lets you change
settings, and shows recent activity.

**Restoring:** pick a new or empty folder. The dashboard refuses a folder that already has files in it, so nothing
gets overwritten. Files come back under their original full path inside that folder.

**Settings** live in `config.toml`, in `~/.config/globalobject/` on Linux or
`~/Library/Application Support/org.globalobject.globalobject/` on macOS. You can edit the file by hand, and the
service picks up changes within a minute.

**Shrinking your share** happens gradually. Other members' pieces move off your disk first, then the space is freed.

## Backup space

Your backup space grows as your shared disk is actually used. It isn't granted just for offering the disk.

If you use more than your share earns, for example after shrinking your slice or being offline a lot, you have 14
days to fix it. After that your account becomes read-only: you can still restore, but you can't back up. After a
further 30 days your data is deleted. `globalobject status` and the dashboard say plainly when this is happening.

## A new or reinstalled machine

```sh
curl -fsSL https://cp.example.org/install.sh | sh -s -- --restore --server https://cp.example.org
```

Type your recovery phrase when asked. Setup finds your account from the phrase alone, and your snapshots are there
to restore. The new machine shares its own disk; the old machine's share drains away once it stops checking in.

## What's kept on your machine

- **`credentials`** holds your recovery phrase, readable only by you. Scheduled backups need it, in the same way
  restic needs a password file. **Anyone who can read this file can read your backups.**
- **`config.toml`** holds your settings.
- **The data directory** holds the state and your shared slice: `~/.local/share/globalobject/` on Linux, under
  Application Support on macOS. The slice holds other members' encrypted pieces, which you can't read.

## Leaving

1. Run `globalobject service uninstall`.
2. Delete the config and data directories.

Your share stops counting once your node is offline past the grace period. Your backups are then deleted along the
schedule in "Backup space" above.
