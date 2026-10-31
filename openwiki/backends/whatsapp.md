---
type: backend
title: WhatsApp backend (whatsapp-rust)
description: The optional, unofficial WhatsApp backend built on whatsapp-rust 0.7 — QR or pair-code linking, the whatsapp.db session, the local JSON/JSONL message archive with hot-chat cache and id mapping, event handling, and the request dispatcher.
tags: [whatsapp, whatsapp-rust, backend, pairing, local-archive, multi-device]
verified:
  - by: openwiki/0.7.2
    at: 2026-10-27T16:14:44.132Z
sources:
  - id: openwiki-source-36f054f1f38f2197a6af5bca
    resource: repo://build.rs
  - id: openwiki-source-651d1fb6c9e49916a916ab51
    resource: repo://Cargo.toml
  - id: openwiki-source-23775c3de52f3ab95a13cb8b
    resource: repo://README.md
  - id: openwiki-source-5282e29a55d4c3315601bfa7
    resource: repo://src/backend/wa/live.rs
  - id: openwiki-source-f8b6ed185c6e26f9b0242389
    resource: repo://src/backend/wa/mod.rs
  - id: openwiki-source-bc27099a99ce839206d235cb
    resource: repo://src/backend/wa/store.rs
  - id: openwiki-source-0a069b3906143584680c8e59
    resource: repo://src/backend/wa/worker.rs
generated: { by: "claude-code", at: "2026-10-27T16:14:44.132Z" }
---

# WhatsApp backend (whatsapp-rust)

Compiled only with `--features whatsapp`. Rondine links to an account as a
WhatsApp "linked device" using the multi-device protocol of WhatsApp Web via
the pure-Rust `whatsapp-rust` crate. It is **not an official client**: the
README warns that Meta may suspend the account and recommends a secondary
number. Like Telegram, it exposes only the neutral types of
[the protocol](../architecture/request-response-protocol.md).

| File | Responsibility |
| --- | --- |
| `wa/mod.rs` | `Wa` handle (client + store), protobuf → `StoredMsg`/`HistoryRow` conversion, media download/upload, pairing types, `start`/`start_with`, `has_session` |
| `wa/store.rs` | local archive of chats and messages |
| `wa/live.rs` | `Shared` event handler: stores events and emits `Live` / `Dialogs` |
| `wa/worker.rs` | `run_worker` dispatcher and the `Wa` methods behind each request |

## Linking and session

`start_with(req_rx, res_tx, pair, phone)`:

1. creates `<data>/rondine/whatsapp/` (0700) and opens the `Store` there;
2. opens the whatsapp-rust SQLite storage at `whatsapp/whatsapp.db`;
3. builds a `Bot` with `on_qr_code` and `on_event` callbacks; if a phone number
   is supplied, configures pair-code linking (8-character code entered on the
   phone) instead of QR;
4. spawns the bot and keeps its handle alive for the app's lifetime;
5. waits for the first `Event::Connected` (signalled through a one-shot "ready"
   channel), or returns the pairing/connection error;
6. starts `worker::run_worker` and returns the stored dialogs and the push name.

Pairing progress goes to a `PairSink` (`Arc<dyn Fn(Pairing)>`) with variants
`Qr`, `Code`, `Paired`, `Status`. The TUI uses `terminal_sink`, which renders
the QR with Unicode half blocks (`qrcode` `Dense1x2`); `start` reads the phone
for code pairing from `RONDINE_WA_PHONE`. The GUI maps the same events into its
own `Pairing` enum and draws the QR in the window (see
[Desktop GUI](../frontends/gui.md)).

`has_session()` is simply "`whatsapp/whatsapp.db` exists"; the GUI uses it to
decide whether to connect at startup or show the linking page.

### One SQLite in the binary

grammers (via libsql) already links a static SQLite. The WhatsApp storage
crate is pulled in with default features off so it does not bring a second
copy (two static copies would not link). On Windows the `libsqlite3-sys`
bindings still ask the linker for `sqlite3.lib`, so `build.rs` compiles an
empty C file into a stub `sqlite3.lib` when the target is Windows and the
`whatsapp` feature is on. A unit test
(`lo_storage_sqlite_si_apre_accanto_a_libsql`) checks that the storage opens,
migrates and reopens on that shared SQLite. See
[Build, features and platforms](../operations/build-and-platforms.md).

## Local archive (`store.rs`)

WhatsApp servers keep no chat history: messages arrive once (history sync at
pairing, then new messages), so Rondine stores everything locally:

```
<data>/rondine/whatsapp/
 ├─ whatsapp.db          whatsapp-rust session (keys, device)
 ├─ chats.json           Meta: chats[], push names, lid→phone aliases, address-book names, state_synced
 └─ msgs/<chat_id>.jsonl one StoredMsg per line, per chat
```

Key rules:

- **Chat ids**: `chat_id_of(jid)` = `2^41 + FNV-1a(jid) mod 2^50`, stable and
  outside Telegram's ranges. Message files are named by this numeric id.
- **Message ids**: WhatsApp's textual ids are stored in `StoredMsg.wa`; each
  chat assigns sequential `i32` ids from `ChatMeta.next_id`. `insert` skips
  messages whose WhatsApp id is already stored.
- **Writes**: `chats.json` is written atomically (temp file + rename,
  `write_atomic`). New messages newer than the last stored one are appended to
  the JSONL file (opened with `private_file`, 0600); out-of-order inserts and
  updates (edit, revoke, reaction) rewrite the whole file atomically.
- **RAM bound**: only the messages of the last `HOT_CHATS = 8` chats touched are
  kept in memory (LRU in `order`); others are re-read from disk on demand.
- **Identity merging**: the same contact can appear as a phone JID
  (`…@s.whatsapp.net`) or an anonymous `…@lid`. `link(lid, pn)` records the
  alias and `merge` folds a duplicate chat into the phone-number chat
  (messages, unread counts, pinned/muted/unread flags). `canon(jid)` always
  returns the phone JID when known.
- **Media**: no whatsapp-rust types in the store; the media-bearing protobuf is
  kept base64-encoded in `StoredMsg.media` and decoded back into a `WaMedia`
  inside a `MediaRef`.
- **Status updates**: `status@broadcast` (`STATUS_JID`) is stored like a chat;
  `prune_status` drops statuses older than 24 h on each connection. Its
  `ChatId` is exported as `model::WA_STATUS` so the GUI can show statuses
  separately from the chat list.
- **Stickers**: there are no browsable sticker sets; `recent_stickers` scans
  stored messages for stickers and serves them as the "recent" set.

## Events (`live.rs`)

`Shared::handle` receives every whatsapp-rust `Event`:

| Event | Handling |
| --- | --- |
| `Connected` | prune old statuses, set presence "available" (needed to receive online/typing), resolve `@lid` chats to phone numbers, request a one-time full app-state sync of contacts and archive/pin/mute state, signal readiness to `start` |
| `PairSuccess` / `PairError` / `LoggedOut` / `TemporaryBan` | pairing sink or a failure (before readiness: returned by `start`; after: `Response::LiveErr`) |
| `Messages` | store each message; reactions and protocol messages (revoke, edit) update the stored message instead of creating a row; emit `Live` events |
| `Receipt` | read receipts → read state |
| `ArchiveUpdate`, `PinUpdate`, `MuteUpdate`, `MarkChatAsReadUpdate` | update `ChatMeta` and emit `Live::ChatChanged` |
| `ChatPresence` | `Live::Typing` ("is typing" / "is recording a voice message"), re-emitted every 5 s for up to 30 s until paused or superseded, because WhatsApp sends it only once |
| `Presence` | `Live::Seen` (online is reported as valid for one hour) |
| `HistorySync`, `ContactUpdate`, `PushNameUpdate`, `PictureUpdate` | fill the archive, names and avatars |

Bursts of list-affecting events (contacts and photos arrive by the hundreds)
are coalesced by `refresh_soon`: one save and one `Response::Dialogs` 1.5 s
after the first event.

## Dispatcher (`worker.rs`)

`run_worker` mirrors the Telegram dispatcher (see
[Telegram backend](telegram.md#dispatcher-tgworkerrs)) with these differences:

- `Hist`, `HistOlder`, `Quoted`, `Search`, `Shared`, `Info` and `Dialogs` are
  answered synchronously from the local store. Opening a private chat also
  subscribes to the contact's presence.
- Conversions use `Semaphore(2)`, translations `Semaphore(3)`, avatar lookups
  a separate `Semaphore(4)`.
- `Del` revokes the message and then emits `Live::Edited` so the row stays as
  "message deleted".
- `Sets` returns an empty list; `SetDocs` returns recent stickers only for
  `RECENT_SET`.
- `Link`/`Join` return "non disponibile su WhatsApp".
- `Sessions` returns a single synthetic entry for this PC (other linked devices
  are visible only on the phone); `EndSession` unlinks this device.
- `Star` is synced to the phone; Telegram keeps stars only locally.
- Media download decrypts from the CDN; previews of videos use the JPEG
  thumbnail embedded in the message. Uploads send PNG/JPEG as images and other
  files as documents.

## README drift

The README's WhatsApp section lists forwarding, blocking, chat deletion and the
GUI as not yet available, but `worker.rs` implements `Forward`, `Block` and
`DeleteChat`, and the GUI has a full WhatsApp environment. Treat the source as
authoritative.
