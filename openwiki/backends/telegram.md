---
type: backend
title: Telegram backend (grammers)
description: How Rondine connects to Telegram with grammers 0.11 — API credentials, terminal and in-window sign-in, the private sqlite session, connection retries, the per-request dispatcher, live update translation, peer-id mapping and media download.
tags: [telegram, grammers, mtproto, backend, login, session, live-updates]
verified:
  - by: openwiki/0.7.2
    at: 2026-10-27T16:14:44.132Z
sources:
  - id: openwiki-source-911ac49ed677c25780a954d6
    resource: repo://src/backend/tg/live.rs
  - id: openwiki-source-1317db6898e1cf0daa186e3d
    resource: repo://src/backend/tg/mod.rs
  - id: openwiki-source-52b2e9fac92c4d8e41db7b04
    resource: repo://src/backend/tg/worker.rs
  - id: openwiki-source-f57a68e955fb9bbaf4aac2fd
    resource: repo://src/gui/net.rs
  - id: openwiki-source-514af1f9d4353b80cbbf634c
    resource: repo://src/paths.rs
generated: { by: "claude-code", at: "2026-10-27T16:14:44.132Z" }
---

# Telegram backend (grammers)

Everything that touches grammers lives in `src/backend/tg/` and nothing else
in the crate imports grammers types (the module comment of `tg/mod.rs` states
this). The library re-exports the module as `rondine::tg`.

| File | Responsibility |
| --- | --- |
| `tg/mod.rs` | credentials, sign-in, connection, type conversion (grammers ↔ `model`), and one async function per server operation |
| `tg/worker.rs` | the request dispatcher (`run_worker`) and `send_job`; `start` wires dispatcher + live stream |
| `tg/live.rs` | translation of grammers `Update`s into neutral `Live` events and the update stream task |

It speaks the protocol described in
[Request/Response protocol](../architecture/request-response-protocol.md).

## Credentials and sign-in

Telegram requires an application API ID and API hash (from my.telegram.org).
They are stored in `<config>/rondine/credentials` as a single line
`"<api_id> <api_hash>"`. `save_credentials` creates the config directory with
`ensure_private_dir` (0700) and opens the file with `paths::private_file()`
so it is born 0600 on Unix — there is no window in which the hash is readable
by others. See [On-disk storage and paths](../concepts/storage-and-paths.md).

Two sign-in flows exist:

**Terminal (TUI)** — `prompt_credentials` asks for API ID/hash on stdin the
first time; `connect_and_sign` then calls `sign_in`, which asks for the phone
number, the login code and, if required, the 2FA password read without echo via
`rpassword`. Invalid code or password abort with a message ("restart and try
again"). The phone number is logged only masked (first 4 chars) to stderr.

**Window (GUI)** — `src/gui/net.rs::run` asks the GUI for credentials through
`NetEvent::Login`, and if the session is not authorised calls
`sign_in_steps(client, api_hash, ask, answers)`. That function loops on each
step, re-asking with an error message on `PHONE_NUMBER_INVALID`, invalid code
or invalid password; answers arrive from `net::answer` through an mpsc channel.
`SignUpRequired` is a hard error (the account must be created with an official
app first).

`normalize_phone` accepts spaces, dashes, parentheses and a `00` prefix; a bare
10-digit number starting with `3` is read as an Italian mobile (`+39…`), any
other bare number just gets `+` prepended, so foreign numbers need their
international prefix.

## Session

`connect` opens the grammers `SqliteSession` at `<data>/rondine/rondine.db`.
Because the database contains authorization keys:

- the data directory is created/repaired to 0700;
- if the db file does not exist it is pre-created 0600 before sqlite opens it;
- on Unix an existing file is chmod-ed to 0600 (repairing old installs);
  `-wal`/`-journal` files rely on the 0700 directory.

A `SenderPool` is built from the session and API ID; its runner is spawned as a
tokio task and its update receiver is returned for the live stream.

`connect_and_sign` (TUI) retries up to 5 attempts with waits of 2, 4, 8, 16 s,
but only for errors classified as network wobble by `net_wobble`
(`NetworkUnreachable`, `TimedOut`, `ConnectionRefused`, `UnexpectedEof`, …);
wrong numbers or codes are not retried. The GUI's `run` has its own loop that
doubles the wait from 2 s while it is ≤ 16 s and reports progress as
`NetEvent::Status`.

After connecting, both frontends call `load_dialogs` (iterating all dialogs and
filling unread count, pinned, archived = folder 1, muted, manual unread mark,
`read_outbox_max_id`, photo id and last seen) and `own_name`, then
`tg::worker::start`.

## Id and type mapping

- `to_chat_id` uses grammers' Bot-API dialog id; the self user (no numeric id)
  maps to the sentinel `1 << 40`. `to_chat_ref`/`to_peer_ref` carry the
  `access_hash` in `ChatRef.auth`.
- `to_media_ref` wraps a grammers `Media` into a `MediaRef` whose key is the
  hex MD5 of the server media id (`media_key`), with the original's extension
  and the sticker emoji.
- `row_from_message` / `row_kind` build `HistoryRow`s; only standard emoji
  reactions are kept (`reactions_from`).

## Dispatcher (`tg/worker.rs`)

`start` spawns `run_worker` and then `live::run`. `run_worker` loops over the
request channel and spawns one task per request so network calls overlap:

- `Conv` and `Avatar` share a `Semaphore(2)` — conversions keep RAM flat and
  the burst of avatar downloads at startup does not saturate the network.
- `Translate` uses a `Semaphore(3)` via the shared `worker::translate_job`.
- `Play` goes through a single `Player` owned by the dispatcher loop (a new
  playback stops the previous one); `Save` uses `worker::save_job`.
- `Hist` loads `MAX_HISTORY` (60) messages, `HistOlder` loads `OLDER_PAGE` (60)
  before an id, `Quoted` loads by id; errors are returned, not swallowed.
- `MarkRead`, `Presence` (`account.updateStatus`) and `Typing`
  (`messages.setTyping`) are fire-and-forget.
- `Star` is ignored: Telegram has no starred messages, stars stay local.

`send_job` handles each `SendKind`: text, sticker, clipboard photo (temp file
deleted after a successful upload), file (photo if PNG/JPEG, otherwise
document) and `EditText`, which returns a synthetic local row with `date = 0`
that `App::replace_row` merges with the original.

Chat-level operations map to raw TL calls in `tg/mod.rs`:
`ChatAction::Archive` → `folders.editPeerFolders` (folder 1),
`Pin` → `messages.toggleDialogPin`, `Mute` → `account.updateNotifySettings`
with `mute_until = i32::MAX` for "forever", `MarkUnread` →
`messages.markDialogUnread`, `Ttl` → `messages.setHistoryTTL`.
`delete_chat` leaves channels/supergroups, leaves small groups and clears their
history, and clears private chats (optionally `revoke` for both sides) by
looping `messages.deleteHistory` until `offset <= 0`.

## Media download

`fetch_raw` downloads the original into the asset cache once
(`loader::cache_raw(key, ext)`); for videos it downloads only the largest
thumbnail as JPEG. The [media cache](../concepts/media-cache-and-conversion.md)
then converts it to PNG. `download_media_to` writes to a caller-supplied path
(used by save and playback).

## Live updates (`tg/live.rs`)

`run` calls `client.stream_updates` with `catch_up: false` and spawns a task
that translates each update with `translate` and sends every resulting `Live`
as `Response::Live`; stream errors become `Response::LiveErr`. The task ends
when the response channel is closed.

| grammers update | `Live` event |
| --- | --- |
| `NewMessage` | `NewMessage` (with a `DialogInfo` only if the peer is already in the update packet; otherwise the app reloads the dialog list) |
| `MessageEdited` | `Edited` |
| `MessageDeleted` | `Deleted` with the channel id if present, else `None` |
| raw `MessageReactions` | `Reactions` |
| raw `ReadHistoryInbox` (no topic) / `ReadChannelInbox` | `ReadInbox` |
| raw `ReadHistoryOutbox` / `ReadChannelOutbox` | `ReadOutbox` |
| raw `PeerBlocked` | `Blocked` |
| raw `DialogUnreadMark`, `DialogPinned`, `FolderPeers`, `NotifySettings`, `PeerHistoryTtl` | `ChatChanged` with the matching `ChatAction` |
| raw `UserStatus` | `Seen` |
| raw `UserTyping`, `ChatUserTyping`, `ChannelUserTyping` | `Typing` |

## Related

- [WhatsApp backend](whatsapp.md) — the second implementation of the same
  contract.
- [Desktop GUI](../frontends/gui.md) — the in-window login phases.
- [Terminal UI](../frontends/tui.md) — terminal startup.
