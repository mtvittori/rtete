---
type: testing
title: Testing
description: Where Rondine's tests live, what each inline test module covers, how to run them for each feature combination, and which areas have no automated tests.
tags: [testing, cargo-test, unit-tests, features, coverage]
verified:
  - by: openwiki/0.7.2
    at: 2026-10-27T16:14:44.132Z
sources:
  - id: openwiki-source-1317db6898e1cf0daa186e3d
    resource: repo://src/backend/tg/mod.rs
  - id: openwiki-source-f8b6ed185c6e26f9b0242389
    resource: repo://src/backend/wa/mod.rs
  - id: openwiki-source-bc27099a99ce839206d235cb
    resource: repo://src/backend/wa/store.rs
  - id: openwiki-source-73d3b88eef9cdf3ebbf1dd27
    resource: repo://src/gui/settings.rs
  - id: openwiki-source-63bd149059b1e9f63769905a
    resource: repo://src/i18n.rs
  - id: openwiki-source-571998b1877b0f8daf274c97
    resource: repo://src/view.rs
generated: { by: "claude-code", at: "2026-10-27T16:14:44.132Z" }
---

# Testing

All tests are inline `#[cfg(test)] mod tests` blocks next to the code they
test; there is no `tests/`, `benches/` or `examples/` directory and no CI job
that runs them (the only workflow regenerates this wiki, see
[Build, features and platforms](../operations/build-and-platforms.md)).
There are about 50 tests across 13 files. Test names and messages are mostly in
Italian, matching the code.

## Running

```
cargo test                               # library + TUI modules
cargo test --features whatsapp           # + backend::wa (mod, store)
cargo test --features gui --bin rondine-gui   # + gui::settings
cargo test --features gui,whatsapp       # everything
```

Feature-gated modules are compiled — and therefore tested — only with their
feature. The GUI tests belong to the `rondine-gui` binary target.

## What is covered

| File | Tests cover |
| --- | --- |
| `src/i18n.rs` | every `tr(`/`trf!(` literal in `src/` has an English translation with the same number of `{}`, and `locales/en.txt` has no unused entries; `fill` ordering — see [Interface languages](../features/i18n.md) |
| `src/paths.rs` | `sanitize_file_name` (separators, `..`, empty/dot names); `unique_save_path` collisions |
| `src/model.rs` | `fmt_size`, `fmt_secs`, `caption` truncation, `ChatId::own_msg_ids` ranges, `MediaRef` downcasting |
| `src/app.rs` | `pinned_first`; `bump_chat` keeps pinned chats in place |
| `src/links.rs` | `parse_link` formats (t.me, tg://, invites, private posts, service paths); `channel_chat` |
| `src/clipboard.rs` | `uri_list_paths` and percent-decoding |
| `src/translate.rs` | LLM JSON parsing (`parse_llm`), DeepL target codes |
| `src/backend/tg/mod.rs` | `ChatId` ↔ `PeerId` round trip (incl. self user and channels matching `links::channel_chat`), `ChatRef` carries the access hash, `normalize_phone` cases |
| `src/backend/wa/mod.rs` | protobuf → `StoredMsg` for text, replies and photos (media survives round trip); reactions/protocol messages are not rows; whatsapp-rust SQLite storage opens and reopens next to libsql (`#[tokio::test]`) |
| `src/backend/wa/store.rs` | stable chat ids above Telegram ranges, `WA_STATUS` matches `chat_id_of(STATUS_JID)`, 24 h status pruning, `@lid` + phone JID merge, insert id assignment/dedup/reload from disk, update of message and preview, dialog ordering and names |
| `src/sixel.rs` | PNG → sixel encoding, missing file, rewritten-cell detection, wide characters, last-row rule, LRU eviction — see [Terminal graphics](../frontends/terminal-graphics.md) |
| `src/view.rs` | compose growth, labels, URL detection and wrapping, long-word wrapping, timezone formatting, message meta and reply lines |
| `src/gui/settings.rs` | each style keeps its own theme; save/load round trip and legacy `tema = rtete` value |

## Test environment notes

- Filesystem tests write under `std::env::temp_dir()` with the process id in
  the directory name and clean up afterwards (paths, WhatsApp store and
  storage, settings).
- `salva_e_rilegge_stile_temi_e_preferite` sets `XDG_CONFIG_HOME` for the whole
  test process to redirect `gui.conf` to a temp directory.
- The i18n test reads source files from `CARGO_MANIFEST_DIR/src` at test time,
  so it must run from a checkout that includes the sources.

## Not covered

No automated tests exercise network behaviour (grammers or whatsapp-rust
against real servers), the dispatcher loops, `sync::handle_response` /
`handle_live`, the TUI event loop and input handling in `events.rs`, kitty
output, iced views, notifications or external tools (mpv/ffplay,
wl-clipboard, xdg-open, hyprctl). Changes there need manual verification by
running the binaries (see [Quickstart](../quickstart.md)).
