---
type: quickstart
title: Quickstart
description: Entry point to the Rondine wiki — what the project is, how to build and run the TUI and GUI with Telegram and WhatsApp, first-run login, and a task-routing map to every other page.
tags: [quickstart, overview, build, run, navigation]
verified:
  - by: openwiki/0.7.2
    at: 2026-10-27T16:14:44.132Z
sources:
  - id: openwiki-source-651d1fb6c9e49916a916ab51
    resource: repo://Cargo.toml
  - id: openwiki-source-23775c3de52f3ab95a13cb8b
    resource: repo://README.md
  - id: openwiki-source-f8b6ed185c6e26f9b0242389
    resource: repo://src/backend/wa/mod.rs
  - id: openwiki-source-f985e52353b94464277d2b2d
    resource: repo://src/gui/main.rs
  - id: openwiki-source-ed8bf05e307c6278442542c2
    resource: repo://src/lib.rs
generated: { by: "claude-code", at: "2026-10-27T16:14:44.132Z" }
---

# Quickstart

**Rondine** (formerly *RTete*; the repository folder keeps the old name) is a
low-RAM messaging client in Rust for Linux, with best-effort support for
macOS and Windows:

- **Telegram** through pure-Rust MTProto (`grammers`), no TDLib;
- **WhatsApp** (optional, unofficial) through `whatsapp-rust`'s multi-device
  protocol;
- two frontends over one shared core: a **ratatui TUI** (`rondine`) with real
  images via the kitty graphics protocol or sixel, and an **iced GUI**
  (`rondine-gui`).

The UI is in Italian and English; source code comments and the README are in
Italian.

## Build and run

```sh
cargo build --release                                   # TUI, Telegram
target/release/rondine

cargo build --release --features whatsapp               # TUI + WhatsApp
target/release/rondine --whatsapp

cargo build --release --features gui,whatsapp --bin rondine-gui
target/release/rondine-gui                # both services in one window
target/release/rondine-gui --telegram     # or -t: Telegram-only instance
target/release/rondine-gui --whatsapp     # or -w: WhatsApp-only instance
```

Only a stable Rust toolchain is required. Optional runtime tools: `mpv` or
`ffplay` (playback), `wl-clipboard` (Linux clipboard), `notify-send` (GUI
notifications), a kitty-protocol or sixel terminal for TUI images. Details:
[Build, features and platforms](operations/build-and-platforms.md).

## First run

- **Telegram**: API ID and API hash from my.telegram.org (saved once in
  `~/.config/rondine/credentials`), then phone number, login code and optional
  2FA password — in the terminal for the TUI, inside the window for the GUI.
  The session is kept in `~/.local/share/rondine/rondine.db`.
- **WhatsApp**: scan the QR from *WhatsApp → Linked devices* on the phone, or
  pair with an 8-character code (`RONDINE_WA_PHONE=…` in the TUI, phone field in
  the GUI). Use a secondary number: it is not an official client.

## Where to go

| I want to… | Read |
| --- | --- |
| understand the overall structure and crates | [Architecture overview](architecture/overview.md) |
| add or change a backend operation | [Request/Response protocol and neutral model](architecture/request-response-protocol.md) |
| understand chat state, unread counts, paging, retries | [App state and sync](architecture/app-state-and-sync.md) |
| work on Telegram login, session or API calls | [Telegram backend](backends/telegram.md) |
| work on WhatsApp pairing, local archive or events | [WhatsApp backend](backends/whatsapp.md) |
| change TUI keys, layout or the event loop | [Terminal UI](frontends/tui.md) |
| fix image rendering in the terminal | [Terminal graphics (kitty and sixel)](frontends/terminal-graphics.md) |
| change the desktop app, environments, settings, themes, notifications | [Desktop GUI](frontends/gui.md) |
| change how media is downloaded, converted or cached | [Media cache and conversion](concepts/media-cache-and-conversion.md) |
| find a file on disk or change permissions/migration | [On-disk storage and paths](concepts/storage-and-paths.md) |
| work on message translation (DeepL / OpenAI-compatible) | [Message translation](features/translation.md) |
| add or fix UI strings or a new UI language | [Interface languages (i18n)](features/i18n.md) |
| work on saving, playback, clipboard, Telegram links, stars | [Downloads, playback, clipboard and links](features/media-actions-and-clipboard.md) |
| run or add tests | [Testing](testing/unit-tests.md) |

## Key invariants to keep in mind

- UI and state never import backend types: everything crosses one `Request`
  channel and one `Response` channel using `src/model.rs` types.
- RAM stays flat: at most 500 history rows per chat, media converted to disk,
  two conversions at a time.
- Every new `tr("…")` string needs an entry in `locales/en.txt`, or
  `cargo test` fails.
- Secrets (credentials, sessions, translation keys) live in 0700 directories
  and 0600 files.
