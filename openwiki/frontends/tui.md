---
type: frontend
title: Terminal UI (ratatui)
description: How the rondine TUI binary starts, runs its single select loop over terminal events and backend responses, renders with ratatui plus image placements, handles keyboard, mouse and paste input, and always restores the terminal on exit or panic.
tags: [tui, ratatui, crossterm, input, event-loop, terminal]
verified:
  - by: openwiki/0.7.2
    at: 2026-10-27T16:14:44.132Z
sources:
  - id: openwiki-source-651d1fb6c9e49916a916ab51
    resource: repo://Cargo.toml
  - id: openwiki-source-5cbf00031a9c6425a5db3350
    resource: repo://src/events.rs
  - id: openwiki-source-b55a21a31ede1b56cd31a6a6
    resource: repo://src/main.rs
  - id: openwiki-source-21d982b4ac6f23138bba84b3
    resource: repo://src/styles.rs
  - id: openwiki-source-2ec5229a97d7e4100c05d317
    resource: repo://src/term.rs
  - id: openwiki-source-571998b1877b0f8daf274c97
    resource: repo://src/view.rs
generated: { by: "claude-code", at: "2026-10-27T16:14:44.132Z" }
---

# Terminal UI (ratatui)

The `rondine` binary (`src/main.rs`) is the original frontend: a ratatui +
crossterm client styled after *jellyfin-tui*. Its modules are private to the
binary:

| Module | Role |
| --- | --- |
| `main.rs` | startup, channels, terminal setup, main loop |
| `events.rs` | keyboard, mouse and bracketed-paste handling |
| `view.rs` | layout and drawing; projects image positions into `Cmd::Place` |
| `term.rs` | terminal restore, `TermGuard`, panic hook |
| `styles.rs` | ANSI palette that follows the terminal theme |
| `gfx.rs`, `kitty.rs`, `sixel.rs` | images, see [Terminal graphics](terminal-graphics.md) |

State and backend responses use the shared library
([App state and sync](../architecture/app-state-and-sync.md)).

## Startup

1. `paths::migrate_old_dirs()`; `i18n::set(i18n::detect())`.
2. `gfx::startup_detect()` — graphics protocol and cell size, **before** the
   TUI owns stdin.
3. `loader::prune_cache()` — default 32 MiB / 200 files.
4. One `Request` and one `Response` unbounded channel.
5. Backend, still in normal terminal mode so prompts can use stdin:
   - default Telegram: load or prompt credentials, `connect_and_sign`
     (phone/code/2FA on stdin, retries on network wobble), `load_dialogs`,
     `own_name`, `tg::worker::start` ([Telegram backend](../backends/telegram.md));
   - `--whatsapp`/`-w`: `backend::wa::start` (QR in the terminal or pair code
     via `RONDINE_WA_PHONE`), or an error if built without the feature
     ([WhatsApp backend](../backends/whatsapp.md)).
6. `App { contacts (pinned first), me, translate: Config::load(), .. }`.
7. Terminal setup: panic hook, raw mode, `TermGuard`, alternate screen, hidden
   cursor, `refresh_cells` again, mouse capture, bracketed paste, and the kitty
   keyboard protocol flag `DISAMBIGUATE_ESCAPE_CODES` (so Cmd/Super+letter
   arrives cleanly).

## Main loop

```
while !app.quit {
    render: terminal.draw(view::draw) → Vec<Cmd::Place>
    if placements changed since last frame → app.refresh_preview_queue
    app.request_older / request_quoted / flush_translations
    images: gfx frame_begin → draw_png per Cmd → frame_end → frame_flush
    select! {
        crossterm EventStream  → events::handle_event
        response channel       → sync::handle_response
    }
}
```

There is a single `tokio::select!` over the terminal event stream and the one
response channel; a closed channel or stream ends the loop. Every iteration
redraws, so state changes from either side appear immediately. Avatars are not
requested in the TUI loop (only the GUI calls `request_avatars`).

## Input (`events.rs`)

- **Modifiers**: Super (kitty keyboard protocol) or Alt-only (legacy terminals
  degrade Super to Alt) + `c`/`v` copy/paste; AltGr (Ctrl+Alt) is left for
  typing accented characters.
- **Ctrl combos** never change the compose text: `C` copies the selected
  message or quits when nothing is selected, `V` paste, `E` edit own text
  message, `D` arm deletion (Enter confirms), `O` save document, `P` play
  audio, `Y` copy, `R` reply, `T` quick reactions bar (←/→ or 1–8, Enter
  toggles), `G` translate, `F` server-side search in the chat.
- **Navigation**: ↑/↓ always; `j`/`k` only where nothing is being typed
  (sticker panel or chat list); `h`/`l` and ←/→ in the sticker grid; `q` quits
  only from the chat list with an empty compose. Moving ↑ past the oldest row
  sets `want_older`, which loads 60 more (up to 500 per chat).
- **Esc is layered**: close search → leave set / close sticker panel → close
  reactions → discard pending photo/file → cancel edit → cancel reply (text
  kept) → disarm deletion → clear selection → finally close the chat.
- **Tab** toggles the sticker panel (requests `Sets` the first time).
- **Enter** in the chat list opens a chat: sets `open`, sends `Request::Hist`
  and resets edit/reply/search state; in the compose box it sends text, a
  pending photo/file or confirms an edit/deletion.
- **Typing** is capped at 4096 characters (1024 as a caption).
- **Mouse**: wheel scrolls contacts when over the left column
  (`view::side_w`), otherwise moves the message selection; left click selects a
  message or opens a link (`links::open_external`, also for URLs wrapped over
  several lines). Shift+drag still lets kitty select text.
- **Bracketed paste** (`Event::Paste`): multi-line text stays one message in
  the compose box; in search, newlines become spaces.

Clipboard and link details: [Downloads, playback, clipboard and links](../features/media-actions-and-clipboard.md).

## View (`view.rs`)

Rounded panels with the title embedded in the top border, a count on the right
edge, an amber border on the focused pane, key hints in the bottom border, a
scroll rail with arrows, the compose line `❯ text` in the bottom frame, and
transient notices in the top-right corner (README "Tema"). `view::draw` fills
`Vec<Cmd>`; each `Cmd::Place { rect, path }` is an image to draw after the
ratatui frame. Image band heights scale with `RONDINE_SCALE` (0.5–4).
`history_lines` projects rows into wrapped lines with clickable link spans and
records `mouse_map`/`link_map` for click handling.

`styles.rs` uses only `Color::Reset` and the 16 ANSI colours, so the TUI
follows the terminal theme (e.g. Omarchy's foot/kitty colours); the selection
bar is `REVERSED`.

## Terminal lifecycle (`term.rs`)

The release profile uses `panic = "abort"`, which skips `Drop`. So restore is
done twice-safely:

- `TermGuard`'s `Drop` covers normal returns and `?` errors;
- `panic_hook` covers panics;
- both consume the `TUI_UP` atomic flag, so restore runs exactly once.

`restore_terminal` clears images, shows the cursor, pops the keyboard flags,
disables mouse capture and bracketed paste, leaves raw mode and the alternate
screen — each step independent so one failure does not block the rest.

## Related

- [Desktop GUI](gui.md) — the iced frontend over the same core.
