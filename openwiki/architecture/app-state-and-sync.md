---
type: architecture
title: App state and sync
description: How the backend-neutral `App` struct holds chats, history rings, previews and pending work, and how `sync.rs` applies live events and worker responses to it identically for the TUI and the GUI.
tags: [state, sync, app, history, backoff, live-events]
verified:
  - by: openwiki/0.7.2
    at: 2026-10-27T16:14:44.132Z
sources:
  - id: openwiki-source-9fec9d1af408501c9c455161
    resource: repo://src/app.rs
  - id: openwiki-source-52b2e9fac92c4d8e41db7b04
    resource: repo://src/backend/tg/worker.rs
  - id: openwiki-source-f985e52353b94464277d2b2d
    resource: repo://src/gui/main.rs
  - id: openwiki-source-b55a21a31ede1b56cd31a6a6
    resource: repo://src/main.rs
  - id: openwiki-source-cc76cc07825d7eb399a2b402
    resource: repo://src/model.rs
  - id: openwiki-source-5b1fad98f7b3082039d72750
    resource: repo://src/sync.rs
generated: { by: "claude-code", at: "2026-10-27T16:14:44.132Z" }
---

# App state and sync

`src/app.rs` owns the in-memory state of one messaging account (`App`), and
`src/sync.rs` is the only place where backend output mutates that state. Both
frontends — the ratatui TUI (`src/main.rs`) and the iced GUI
(`src/gui/main.rs`) — call the same two entry points, so behaviour such as
unread badges, de-duplication and retry backoff is shared rather than
re-implemented per UI.

```
backend worker ──Response──▶ sync::handle_response ──▶ App (mutations)
backend live   ──Response::Live──▶ sync::handle_live ─┘
App helpers (request_older / request_quoted / flush_translations /
             refresh_preview_queue / request_avatars) ──Request──▶ backend
```

The state never imports backend types: it only uses the neutral types of
`src/model.rs` and the `Request`/`Response` enums of `src/send.rs` (see
[Request/Response protocol](request-response-protocol.md)).

## The `App` struct

`App` is a `#[derive(Default)]` plain struct with public fields. The main
groups are:

| Group | Fields (selection) | Notes |
| --- | --- | --- |
| Chat list | `contacts: Vec<DialogInfo>`, `contact_cursor`, `open` | `open` is the `ChatId` of the open chat |
| Histories | `histories: HashMap<ChatId, VecDeque<HistoryRow>>` | one ring per loaded chat |
| Compose / edit | `compose`, `editing`, `replying`, `del_arm`, `del_id`, `pending_photo`, `pending_file`, `uploading` | TUI keys and GUI actions both drive these |
| Stickers | `sets`, `set_docs`, `open_set`, `docs_failed` | `sets: None` means "still loading / failed, retry" |
| Previews | `previews`, `pending`, `failed` | media key → converted PNG path, in-flight set, backoff map |
| Paging | `older_busy`, `older_done`, `want_older`, `quoted`, `quoted_busy`, `quoted_failed` | backward scroll and quoted-reply lookups |
| Chat details | `peer_info`, `shared`, `shared_asked`, `avatars`, `avatar_asked`, `typing` | |
| Settings-backed | `media_prefs`, `translate`, `translations`, `tr_queue`, `starred`, `profile`, `sessions` | |

The GUI keeps one `App` per environment (Telegram, WhatsApp); see
[Desktop GUI](../frontends/gui.md).

## Memory limits

RAM is a design goal of the project, so history is bounded:

- `MAX_HISTORY = 60` — rows requested when a chat is first opened
  (`Request::Hist` uses it in the Telegram dispatcher).
- `OLDER_PAGE = 60` — page size of backward scrolling.
- `RING_CAP = 500` — hard cap of rows kept per chat. `push_each` drops from the
  front when the ring grows past it, `prepend_rows` refuses rows beyond the
  remaining room, and `request_older` marks the chat as done when the cap is
  reached.
- `SHARED_PAGE = 60` — items per tab in a chat's shared media.

`push_each` de-duplicates by message id because the same message can arrive
twice: once as the send confirmation (`Response::Sent`) and once as a live
update.

## Chat list ordering

- `pinned_first` sorts pinned dialogs to the top (stable sort on `!pinned`).
- `bump_chat` moves a chat that just received a message to the first position
  *below* the pinned block, never moves a pinned chat, inserts an unknown chat
  only if a ready `DialogInfo` is supplied, and keeps `contact_cursor` on the
  same chat.
- `apply_chat_action` applies archive/pin/mute/mark-unread flags (and the
  ephemeral `Ttl` timer to `peer_info`), re-sorting on pin changes.
- `forget_chat` removes a deleted/left chat and every per-chat cache entry,
  closing it if it was open.

## Retry backoff

A single exponential backoff (`backoff_secs`: 1, 2, 4 … capped at 60 s) guards
work that the render loop would otherwise retry every frame:

- failed preview conversions (`failed`, via `conversion_failed`);
- failed sticker-set downloads (`docs_failed`, checked by `docs_cooling`);
- failed quoted-message lookups (`quoted_failed`).

Without it, `refresh_preview_queue` (called whenever the visible assets change)
would re-download a broken asset forever. Avatars use a simpler rule: a photo id
stays in `avatar_asked` even after failure, so it is never re-requested in a
loop.

## Work generated from state

These helpers are called by the frontends' loops and turn state into
`Request`s:

- `refresh_preview_queue` / `ensure_previews` — queue `Request::Conv` for
  visible stickers/photos/video thumbnails not yet converted, honouring
  `MediaPrefs` (animated stickers are never auto-converted; the sticker grid is
  truncated to 24 items).
- `request_older` — when `want_older` was set by scrolling past the oldest row,
  sends one `HistOlder` per chat at a time.
- `request_quoted` — collects `reply_to` ids missing from the ring, at most 100
  per request (Telegram `GetMessages` limit), one request per chat at a time.
- `flush_translations` — sends queued translations; if no provider is
  configured it clears the queue and sets a status hint. See
  [Message translation](../features/translation.md).
- `request_avatars` — one `Request::Avatar` per missing photo, reusing files
  already in the avatar cache.
- `read_mark_due` — throttles `MarkRead` to one every 2 s during message bursts.

## Applying live events (`handle_live`)

| `Live` variant | Effect |
| --- | --- |
| `NewMessage` | bump chat (or request `Dialogs` if the chat is unknown and no dialog was supplied), clear typing, update preview line, push into the open chat (and mark read, throttled) or increment `unread`; queue auto-translation for auto chats |
| `Edited` | `replace_row`; cancels an in-progress local edit of the same id |
| `Deleted` | with a chat id: remove from that ring only; without: remove from every ring **except** channels/supergroups (`ChatId::own_msg_ids`), whose ids collide with private-chat ids |
| `Reactions` | replace reaction list |
| `ReadInbox` / `ReadOutbox` | set `unread` from the remote count / raise `read_out` (double ticks) |
| `ChatChanged`, `Blocked`, `Seen` | update flags in contacts or `peer_info` |
| `Typing` | store a 6 s expiring label (with the first name of the sender in groups) |

## Applying worker responses (`handle_response`)

Every `Response` variant has one branch. Notable rules:

- A failed `Hist` sets a status message instead of showing an empty chat:
  "network error" and "no messages" are kept distinct.
- When the open chat's history arrives, unread is zeroed, a manual
  "mark as unread" flag is cleared via `ChatAction::MarkUnread(false)`,
  `MarkRead` is sent and previews are queued.
- `HistOlder` marks the chat done when the page is short (start of history) or
  when rows were rejected by `RING_CAP`.
- `Docs` errors do not insert an empty set (which would look "loaded" forever);
  they only bump the backoff.
- A failed automatic translation disables auto-translate for that chat to avoid
  an error storm.
- `ProfileSaved` re-requests `Profile`; `Dialogs` replaces the list but keeps
  the cursor on the same chat.

Most outcomes surface as a one-line `app.status` string, localised through
`tr`/`trf!` (see [Interface languages](../features/i18n.md)).

## Tests

`src/app.rs` has unit tests for `pinned_first` and for `bump_chat` leaving
pinned chats in place.
