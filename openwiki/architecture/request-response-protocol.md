---
type: architecture
title: Request/Response protocol and neutral model
description: The contract between Rondine's UI/state and its messaging backends — the Request, Response and SendJob enums in send.rs, the backend-neutral types in model.rs (ChatId, ChatRef, MediaRef, HistoryRow, Live), id-space rules, and how to extend the protocol or add a backend.
tags: [protocol, model, backend-contract, channels, extension]
verified:
  - by: openwiki/0.7.2
    at: 2026-10-27T16:14:44.132Z
sources:
  - id: openwiki-source-1317db6898e1cf0daa186e3d
    resource: repo://src/backend/tg/mod.rs
  - id: openwiki-source-52b2e9fac92c4d8e41db7b04
    resource: repo://src/backend/tg/worker.rs
  - id: openwiki-source-bc27099a99ce839206d235cb
    resource: repo://src/backend/wa/store.rs
  - id: openwiki-source-0a069b3906143584680c8e59
    resource: repo://src/backend/wa/worker.rs
  - id: openwiki-source-cc76cc07825d7eb399a2b402
    resource: repo://src/model.rs
  - id: openwiki-source-8b9177c16218108d4b93861c
    resource: repo://src/send.rs
generated: { by: "claude-code", at: "2026-10-27T16:14:44.132Z" }
---

# Request/Response protocol and neutral model

Two files define everything a backend must speak:

- `src/send.rs` — the `Request` enum (UI → backend), the `Response` enum
  (backend → UI, including live events) and the `SendJob`/`SendKind`/`SendDone`
  types for outgoing messages.
- `src/model.rs` — the neutral data types. The file's doc comment states the
  rule: no grammers (or WhatsApp) type appears here; each backend converts its
  objects into these and receives them back inside `Request`s.

[App state and sync](app-state-and-sync.md) consumes these types; the
[Telegram](../backends/telegram.md) and [WhatsApp](../backends/whatsapp.md)
backends produce them.

## Channels

There is exactly one request channel and one response channel per backend
connection, both `tokio::sync::mpsc::unbounded_channel`. Comments in
`send.rs` and `main.rs` record the history: the app previously had ten channel
pairs and a hand-rolled `waiting_pick` with `.expect("servizio chiuso")` on
every receive.

Every `Response` that answers a `Request` carries a `Result<_, String>`:
errors travel as values on the channel so the UI can show them (for example a
failed history load is not confused with an empty chat). Live server events use
the same channel as `Response::Live(Live)`, and stream errors as
`Response::LiveErr(String)`.

## Identity types

| Type | Meaning |
| --- | --- |
| `ChatId(i64)` | Stable chat identity. For Telegram it is the "Bot API" dialog id (peer type packed into the number). Other backends must choose numbers outside Telegram's ranges. It is also persisted (auto-translate chat list, starred messages), so it must not change between runs. |
| `ChatRef { id, auth }` | What a backend needs to address a chat: identity plus server permission (`access_hash` on Telegram, `0` where unused). |
| `MsgId = i32` | Message id within a chat. WhatsApp maps its textual ids to per-chat sequential numbers. |

Id-space rules:

- `ChatId::own_msg_ids()` is true for Telegram channels/supergroups, whose
  message ids are per-channel; a deletion event without a chat id must not touch
  them.
- WhatsApp ids are computed by `store::chat_id_of` as
  `2^41 + (FNV-1a(jid) mod 2^50)`, i.e. above Telegram user ids (up to 2^40) and
  positive, so they never collide with Telegram ids. `model::WA_STATUS` is the
  precomputed id of `status@broadcast`.
- The Telegram adapter maps grammers' self-user peer (no numeric id) to the
  sentinel `1 << 40`.

## `MediaRef`: type-erased backend media

`MediaRef` is an `Arc` around a cache key, the original file extension
(`jpg`, `png`, `webp`, `gif` or `bin`), a sticker emoji and a
`Box<dyn Any + Send + Sync>` holding the backend's own media object. Only the
backend that created it can download or re-send it: Telegram stores a grammers
`Media` and recovers it with `m.get::<Media>()`; WhatsApp stores a `WaMedia`
wrapping the message protobuf. The key is what the
[media cache](../concepts/media-cache-and-conversion.md) uses for file names.

## Content types

- `DialogInfo` — one chat-list row: name, `peer_ref`, `peer_id`, unread count,
  pinned/archived/muted/broadcast flags, manual unread mark, last-message
  preview (`LastMsg`), `read_out` (double ticks), `photo_id`, `seen`.
- `HistoryRow` — one message: id, outgoing flag, sender name, `RowKind`,
  standard-emoji reactions, Unix date (0 for local rows), `reply_to`,
  `reply_quote`, forward origin, edited flag.
- `RowKind` — `Text`, `Sticker`, `Photo` (also GIF), `Doc`, `Audio`
  (voice or music), `Video` (also round video notes), `Note`, `Service`.
  `summary()` yields the one-line preview used in lists and quotes.
- Others: `ReactionItem`, `StickerSetInfo` (with fake ids `FAVED_SET = -1`,
  `RECENT_SET = -2`), `PeerInfo`, `SharedTab`/`SharedItem`, `MyProfile`,
  `Session`, `SearchHit`, `LinkHit`, `ChatAction`
  (`Archive`/`Pin`/`Mute`/`MarkUnread`/`Ttl`).

## Live events

`model::Live` is what a backend emits for server-side changes:
`NewMessage` (optionally with a ready `DialogInfo` for an unseen chat),
`Edited`, `Deleted` (chat optional), `Reactions`, `ReadInbox`, `ReadOutbox`,
`Blocked`, `ChatChanged`, `Seen`, `Typing`. How each is applied is described in
[App state and sync](app-state-and-sync.md#applying-live-events-handle_live).

## Requests

| Area | Requests |
| --- | --- |
| History | `Hist`, `HistOlder { before }`, `Quoted { ids }`, `MarkRead`, `Search`, `Shared { tab }` |
| Sending | `Send(SendJob)`, `Del`, `React`, `Forward`, `Typing`, `Presence` |
| Stickers / media | `Sets`, `SetDocs`, `Conv(MediaRef)`, `Save`, `Play`, `Avatar` |
| Chats | `Dialogs`, `Info`, `Block`, `DeleteChat { revoke }`, `ChatAction`, `Star`, `Link(TgLink)`, `Join(hash)` |
| Account | `Profile`, `SaveProfile`, `ProfilePhoto`, `Sessions`, `EndSession(Option<i64>)` |
| Translation | `Translate { auto }`, `TranslateTest` |

`SendJob` bundles the target (`peer`, `peer_id`), an optional `reply_to` and a
`SendKind`: `Text`, `Sticker(MediaRef)`, `Photo { path, caption }` (a
clipboard temp file, deleted after upload by the Telegram backend),
`File { path, caption }` (user file; PNG/JPEG go as photos, everything else as
documents, the file is kept) or `EditText { id, text }`. The answer is
`Response::Sent(Result<SendDone, _>)` with `SendDone::New` or `SendDone::Edit`.

## Dispatcher pattern

Both backends implement `run_worker(…, req_rx, out)` the same way: a spawned
loop receives each `Request` and usually spawns one task per job, so network
calls run in parallel. Shared limits:

- preview conversions: `Semaphore::new(2)`;
- translations: `Semaphore::new(3)` (in `worker::translate_job`);
- Telegram avatars reuse the conversion semaphore; WhatsApp avatars have their
  own `Semaphore::new(4)`.

WhatsApp answers history, search, shared and info requests synchronously from
its local archive, and returns `"non disponibile su WhatsApp"` (via
`unsupported()`) for Telegram-only features such as link resolution and joins.
Telegram ignores `Request::Star` because stars are stored locally (see
[Downloads, playback, clipboard and links](../features/media-actions-and-clipboard.md)).

## Extending the protocol

To add an operation:

1. add the `Request` variant (and, if it returns data, a `Response` variant)
   in `src/send.rs`, using only `model` types;
2. handle it in **every** backend's `run_worker` match — the match is
   exhaustive, so a backend compiled without a branch fails to build
   (`tg::worker` always; `wa::worker` with `--features whatsapp`);
3. apply the response in `sync::handle_response`;
4. send it from the TUI (`src/events.rs`) and/or GUI (`src/gui/main.rs`).

To add a backend: create `src/backend/<name>/`, convert its objects to
`model` types, choose a `ChatId` range that cannot collide with existing ones,
implement a `start`-style function returning `(Vec<DialogInfo>, String)` (chats
and own name) after spawning the dispatcher, and wire it into the frontends.
