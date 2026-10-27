---
type: concept
title: On-disk storage and paths
description: Where Rondine keeps credentials, sessions, caches, settings and saved files on Linux, macOS and Windows; the RTete-to-Rondine directory migration; private file permissions; and safe naming of downloads.
tags: [paths, storage, xdg, permissions, migration, config, security]
verified:
  - by: openwiki/0.7.2
    at: 2026-10-27T16:14:44.132Z
sources:
  - id: openwiki-source-f985e52353b94464277d2b2d
    resource: repo://src/gui/main.rs
  - id: openwiki-source-73d3b88eef9cdf3ebbf1dd27
    resource: repo://src/gui/settings.rs
  - id: openwiki-source-b55a21a31ede1b56cd31a6a6
    resource: repo://src/main.rs
  - id: openwiki-source-514af1f9d4353b80cbbf634c
    resource: repo://src/paths.rs
  - id: openwiki-source-38251fdb0c9989d14c9dda87
    resource: repo://src/starred.rs
  - id: openwiki-source-88bf525ee1897df0d3f5c2db
    resource: repo://src/translate.rs
  - id: openwiki-source-89402e3f86a3a076c01e7039
    resource: repo://src/worker.rs
generated: { by: "claude-code", at: "2026-10-27T16:14:44.132Z" }
---

# On-disk storage and paths

`src/paths.rs` is the single source of directory locations, shared by every
backend and frontend. Rondine keeps no message history of its own for
Telegram (the server has it); what it stores is credentials, sessions, caches,
small settings files and — for WhatsApp only — a local message archive.

## Directory roots

| Root | Resolution order | Typical Linux value |
| --- | --- | --- |
| config base | `$XDG_CONFIG_HOME` (only if absolute) → `%APPDATA%` (Windows only) → `$HOME/.config` → `.` | `~/.config` |
| data base | `$XDG_DATA_HOME` (only if absolute) → `%LOCALAPPDATA%` (Windows only) → `$HOME/.local/share` → `.` | `~/.local/share` |
| home | `$HOME`, else `%USERPROFILE%` | |
| downloads | `$XDG_DOWNLOAD_DIR` (quotes stripped, `$HOME` expanded, must be absolute) → `<home>/Downloads` | `~/Downloads` |

`config_dir()` and `data_dir()` append the app directory `rondine`.

## What lives where

```
<config>/rondine/
 ├─ credentials        Telegram API ID + hash                 (tg/mod.rs, 0600)
 ├─ translate.conf     translation provider, keys, auto chats (translate.rs, 0600)
 └─ gui.conf           GUI settings, "key = value" lines      (gui/settings.rs)

<data>/rondine/
 ├─ rondine.db (+ -wal/-shm/-journal)  Telegram sqlite session (0600)
 ├─ starred.json        locally starred messages (with text copy)
 ├─ assets/             media cache: <key>.<ext>, <key>.png
 │   └─ avatars/<photo_id>.jpg
 ├─ tmp/                clipboard photos, playback temp files (play-<gen>-<name>)
 └─ whatsapp/           [feature whatsapp]
     ├─ whatsapp.db
     ├─ chats.json
     └─ msgs/<chat_id>.jsonl

<downloads>/            saved documents (Ctrl-O / GUI "save")
```

Owners: [Telegram backend](../backends/telegram.md) (credentials, session),
[WhatsApp backend](../backends/whatsapp.md) (whatsapp/),
[Media cache](media-cache-and-conversion.md) (assets/),
[Message translation](../features/translation.md) (translate.conf),
[Desktop GUI](../frontends/gui.md) (gui.conf),
[Downloads, playback, clipboard and links](../features/media-actions-and-clipboard.md)
(tmp/, starred.json, downloads).

## Private permissions

Files that contain secrets are protected on Unix:

- `ensure_private_dir(path)` creates the directory and **always** chmods it to
  0700, also repairing directories that older installs created with the
  default umask (755).
- `private_file()` returns `OpenOptions` with mode 0600 on Unix, so files are
  born private instead of being written with the umask and chmod-ed later.
  On Windows it falls back to the permissions inherited from the user profile.

Used by `credentials`, `translate.conf`, the Telegram session (pre-created
0600 and repaired on open) and WhatsApp message files. Sqlite side files rely
on the 0700 directory. `starred.json` is written with a plain `fs::write`
inside the 0700 data directory, and `gui.conf` with plain `fs::write` in the
config directory (it holds no secrets).

## RTete → Rondine migration

The app used to be called RTete and stored data under `rtete/`.
`paths::migrate_old_dirs()` runs first in both binaries:

1. for both the config and data base, rename `rtete/` to `rondine/` if the old
   one exists and the new one does not;
2. in the data directory, rename `rtete.db` (and `-wal`, `-shm`, `-journal`) to
   `rondine.db…` if the new names are absent.

So an existing login keeps working. Environment variables follow the same rule:
`rondine::env_var("X")` reads `RONDINE_X`, falling back to `RTETE_X`.

## Saving files safely

- `sanitize_file_name` strips control characters, replaces `/`, `\` and `..`
  with `_`, trims trailing dots and whitespace, and returns `None` for names
  that become empty or `.`.
- `unique_save_path` never overwrites: it tries `name`, then `name (1).ext` …
  `name (999).ext`, then falls back to a millisecond timestamp suffix.
- `worker::save_job` downloads to a hidden `.rondine-<ts>.part` in the
  downloads directory and renames to the unique name only after a complete
  download; the `.part` is always removed afterwards.

## Tests

`src/paths.rs` tests `sanitize_file_name` (path separators, `..`, empty/dot
names) and `unique_save_path` collision handling.
