---
type: architecture
title: Architecture overview
description: Crate layout of Rondine (formerly RTete) — a shared library, a ratatui TUI binary and an optional iced GUI binary — and how frontends, state, workers and the Telegram/WhatsApp backends talk through one request channel and one response channel.
tags: [architecture, crate-layout, features, backends, channels]
verified:
  - by: openwiki/0.7.2
    at: 2026-10-27T16:14:44.132Z
sources:
  - id: openwiki-source-651d1fb6c9e49916a916ab51
    resource: repo://Cargo.toml
  - id: openwiki-source-dd9c1a772eaadcb86ee59311
    resource: repo://src/backend/mod.rs
  - id: openwiki-source-f985e52353b94464277d2b2d
    resource: repo://src/gui/main.rs
  - id: openwiki-source-f57a68e955fb9bbaf4aac2fd
    resource: repo://src/gui/net.rs
  - id: openwiki-source-ed8bf05e307c6278442542c2
    resource: repo://src/lib.rs
  - id: openwiki-source-b55a21a31ede1b56cd31a6a6
    resource: repo://src/main.rs
  - id: openwiki-source-cc76cc07825d7eb399a2b402
    resource: repo://src/model.rs
  - id: openwiki-source-8b9177c16218108d4b93861c
    resource: repo://src/send.rs
generated: { by: "claude-code", at: "2026-10-27T16:14:44.132Z" }
---

# Architecture overview

Rondine is a low-RAM messaging client written in Rust. It started as a
Telegram-only terminal client called *RTete* (the repository directory still
carries that name) and grew a desktop GUI and an unofficial WhatsApp backend.
The guiding constraints, visible throughout the code, are: pure-Rust protocol
stacks (no TDLib, no browser), flat RAM (bounded histories, media converted to
disk immediately) and a UI layer that knows nothing about the messaging
service.

## Crate layout

One Cargo package (`rondine`) produces a library and two binaries:

| Target | Path | Built when | Role |
| --- | --- | --- | --- |
| library `rondine` | `src/lib.rs` | always | state, protocol, workers, media cache, backends |
| binary `rondine` | `src/main.rs` | always | ratatui/crossterm TUI |
| binary `rondine-gui` | `src/gui/main.rs` | `--features gui` | iced desktop GUI |

Cargo features:

- `gui` — enables `iced` and `rfd` (file picker) and the `rondine-gui` binary.
- `whatsapp` — enables `whatsapp-rust`, its SQLite storage, `serde` and
  `qrcode`, and compiles `backend::wa`. Without it the TUI rejects
  `--whatsapp` with an error and the GUI has no WhatsApp environment.

See [Build, features and platforms](../operations/build-and-platforms.md) for
the release profile and the Windows link workaround.

## Module map

```
src/lib.rs ── shared core (pub mods)
 ├─ model.rs      neutral types: ChatId, ChatRef, MediaRef, DialogInfo, HistoryRow, Live …
 ├─ send.rs       Request / Response / SendJob protocol
 ├─ app.rs        App state + update helpers
 ├─ sync.rs       apply Live events and Responses to App
 ├─ worker.rs     backend-neutral jobs: save, player, translate, convert
 ├─ loader.rs     asset cache + PNG/APNG conversion
 ├─ paths.rs      config/data/cache/download directories, migration, permissions
 ├─ translate.rs  DeepL / OpenAI-compatible translation
 ├─ i18n.rs       tr()/trf! with locales/<lang>.txt
 ├─ clipboard.rs, links.rs, starred.rs
 └─ backend/
     ├─ tg/       Telegram via grammers (mod, worker, live)
     └─ wa/       WhatsApp via whatsapp-rust (mod, store, worker, live)  [feature]

src/main.rs ── TUI only: events.rs, view.rs, term.rs, styles.rs, gfx.rs, kitty.rs, sixel.rs
src/gui/   ── GUI only: main.rs, net.rs, ui.rs, settings.rs, style.rs, notify.rs
```

The TUI-specific modules are declared with `mod` inside `src/main.rs`, not in
the library, so the GUI never links them. `lib.rs` re-exports
`backend::tg` as `rondine::tg` and provides `env_var`, which reads
`RONDINE_<KEY>` and falls back to the legacy `RTETE_<KEY>`.

## The backend boundary

The central design rule (stated in `lib.rs`, `backend/mod.rs` and
`model.rs`): the UI and `App` talk to a backend **only** through the neutral
types of `model.rs` and the `Request`/`Response` enums of `send.rs`. Each
backend:

1. translates its native objects (grammers messages, WhatsApp protobufs) into
   `DialogInfo`, `HistoryRow`, `MediaRef`…;
2. runs a dispatcher that receives `Request`s on an unbounded mpsc channel and
   spawns one task per job;
3. sends every result — and every live server event as `Response::Live` — on a
   single unbounded response channel.

```
            ┌──────────── Request (mpsc, unbounded) ─────────────┐
 Frontend ──┤                                                    ▼
 (TUI/GUI)  │                                         backend dispatcher
   ▲        │                                         (tg::worker / wa::worker)
   │        └──── App helpers (previews, older, quoted, translations)
   │                                                        │  │
   └── sync::handle_response ◀── Response (mpsc) ───────────┘  │
                                   ▲                           │
                                   └── Response::Live ◀── live update stream
```

Adding a service means adding a folder under `src/backend/` that executes the
same requests (README "Architettura"). The details of the contract are in
[Request/Response protocol](request-response-protocol.md).

## Startup paths

**TUI** (`src/main.rs`): migrate old RTete directories → set UI language →
detect terminal graphics → prune the asset cache → create the two channels →
start either Telegram (credentials and sign-in prompted on stdin, then
`tg::worker::start`) or, with `--whatsapp`/`-w`, `backend::wa::start` → build
`App` with the loaded dialogs → enter raw mode and run the
render/select loop. Details: [Terminal UI](../frontends/tui.md).

**GUI** (`src/gui/main.rs`): migrate directories → load settings → choose the
iced renderer → parse `--telegram`/`--whatsapp` solo modes → single-instance
hand-off → `net::start` builds a dedicated tokio runtime and starts Telegram;
WhatsApp starts too if a session already exists → iced application. Login and
WhatsApp pairing happen inside the window (the module doc comment in
`src/gui/main.rs` still says login is TUI-only; `src/gui/net.rs` now
implements in-window sign-in). Details: [Desktop GUI](../frontends/gui.md).

## State

`App` (in `app.rs`) holds one account's chats, bounded histories and pending
work; `sync.rs` mutates it from responses. Both frontends share this code. See
[App state and sync](app-state-and-sync.md).

## Backends

- [Telegram backend](../backends/telegram.md): grammers 0.11, sqlite session
  `rondine.db`, API ID/hash from `credentials`.
- [WhatsApp backend](../backends/whatsapp.md): whatsapp-rust 0.7 multi-device,
  `whatsapp/whatsapp.db` plus a local JSON/JSONL message archive because the
  server keeps no history.

## Cross-cutting concerns

- [Media cache and conversion](../concepts/media-cache-and-conversion.md)
- [On-disk storage and paths](../concepts/storage-and-paths.md)
- [Message translation](../features/translation.md)
- [Interface languages](../features/i18n.md)
- [Downloads, playback, clipboard and links](../features/media-actions-and-clipboard.md)
