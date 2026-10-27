---
type: frontend
title: Terminal graphics (kitty and sixel)
description: How the Rondine TUI shows stickers and photos inside the terminal — environment-based protocol detection, cell-size measurement, the kitty transmit-once/place-per-frame model and the sixel damage-tracking renderer with its encoding cache.
tags: [tui, graphics, kitty-graphics-protocol, sixel, foot, ratatui, rendering]
verified:
  - by: openwiki/0.7.2
    at: 2026-10-27T16:14:44.132Z
sources:
  - id: openwiki-source-6aa7b8ef076b4ef4f9683a99
    resource: repo://src/gfx.rs
  - id: openwiki-source-891fdbcad23ece13326c0724
    resource: repo://src/kitty.rs
  - id: openwiki-source-b55a21a31ede1b56cd31a6a6
    resource: repo://src/main.rs
  - id: openwiki-source-5e596d6a16d9e5bc22dba0c3
    resource: repo://src/sixel.rs
generated: { by: "claude-code", at: "2026-10-27T16:14:44.132Z" }
---

# Terminal graphics (kitty and sixel)

The TUI draws images (stickers, photos, GIF/APNG, video thumbnails, avatars)
on top of ratatui's text frame. Three TUI-only modules handle it:

| Module | Role |
| --- | --- |
| `src/gfx.rs` | dispatcher: detection, cell size, `square_fit`, frame hooks — the only module `main.rs` and `term.rs` call |
| `src/kitty.rs` | kitty graphics protocol (Kitty, Ghostty, WezTerm) |
| `src/sixel.rs` | sixel (foot and other sixel terminals), encoded with `icy_sixel` |

All images come from the PNG/APNG files produced by the
[media cache](../concepts/media-cache-and-conversion.md); the renderers never
decode chat media themselves except sixel, which scales the PNG to cell size.

## Detection

`gfx::startup_detect()` runs once in `main` **before** the TUI takes stdin and
never queries the terminal. The modules explain why: a query such as kitty's
`a=q` expects a reply on stdin, and an orphaned blocking read would swallow the
first line the user types (for example the phone number at login).

Order in `gfx::detect`:

1. `RONDINE_FORCE_SIXEL` → sixel; `RONDINE_FORCE_KITTY` → kitty.
2. `TERM=foot*`, or a `foot`/`footclient` ancestor process found by walking
   `/proc/<pid>/stat` (Linux only) → sixel. This handles Omarchy's foot config
   which sets `TERM=xterm-256color`. Foot is checked **before** kitty because
   `KITTY_WINDOW_ID` & co. are inherited by children, and a foot launched from a
   kitty shell would not understand kitty escapes.
3. kitty environment: `TERM=xterm-kitty`, `KITTY_WINDOW_ID`,
   `TERM_PROGRAM=ghostty|WezTerm`, `GHOSTTY_RESOURCES_DIR`, `WEZTERM_PANE` →
   kitty.
4. Otherwise no graphics: only text labels (emoji for stickers) are shown.

## Cell size and sizing

`refresh_cells` reads `crossterm::terminal::window_size()` (ioctl, no stdin)
and divides pixels by columns/rows; it is called at startup, again once the TUI
is up (some terminals report pixels late) and on resize. If the terminal
reports 0 pixels the previous estimate (default 10×20) stays.
`RONDINE_CELL_W`/`RONDINE_CELL_H` override it. A change of cell size
invalidates every sixel image (they were encoded at the old scale).

`square_fit(max_w, max_h)` returns the (cols, rows) that make an image
physically square inside the available cells. `RONDINE_SCALE` (README) scales
sticker/photo boxes in the view.

## Frame lifecycle

`src/main.rs` drives every frame:

```
gfx::sync_begin()                      sixel: DECSET 2026 (synchronized update)
terminal.draw(view::draw → Vec<Cmd::Place{rect,path}>)
gfx::observe_frame(buffer)             sixel: remember which cells ratatui rewrote
gfx::frame_begin()
for each Cmd::Place: gfx::draw_png(path, (x,y), square_fit(w,h))
gfx::frame_end(&mut terminal)          kitty: delete vanished ids / sixel: repair cells
gfx::frame_flush()                     sixel: emit images, DECRST 2026
```

`view.rs` only *projects* positions into `Cmd::Place`; the loop also compares
the list of placements with the previous frame and refreshes the preview queue
only when the visible set changes. See [Terminal UI](tui.md).

## Kitty renderer

- **Transmit once per path**: the first time a path is drawn it gets a new
  image id and is sent with `a=t,t=f` — the payload is the base64-encoded
  *file path*, and the terminal reads the file itself (kitty 0.49 rejects a
  plain-text path). Nothing image-sized crosses the pty.
  `RONDINE_B64=1` switches to chunked base64 of the file content (4096-byte
  chunks) for terminals that accept the protocol but not `t=f`.
- **Place every frame**: `a=p,i=<id>,p=<n>,c=<cols>,r=<rows>,C=1` after a
  cursor move. The placement id `p` is the k-th copy of that image in the frame,
  so kitty replaces placements in place instead of leaving trails while
  scrolling.
- **Delete by id**: at `frame_end`, images placed in the previous frame but not
  in this one are deleted with `a=d,d=i,i=<id>` (or only the extra placement
  with `p`), and their path is forgotten so it is retransmitted with a new id if
  it reappears. The old approach — a global `a=d,d=A` on every layout change —
  forced retransmitting everything and flickered.
- `clear_all` (`a=d,d=A`) runs on exit and from the panic restore path.

Kitty animates APNG by itself, so GIFs converted to APNG animate with no work
in Rondine.

## Sixel renderer

Sixel has no ids or placements: pixels stay until something overwrites their
cells, and on foot writing a cell clears only that cell's pixels. Ratatui only
rewrites cells that changed, so `sixel.rs` diffs the drawn buffer against the
previous frame:

- new image, or one whose cells were rewritten by ratatui → re-emit it;
- image gone or moved → rewrite its cells that ratatui did *not* touch, from
  the current buffer via `Backend::draw` (same content ratatui believes is on
  screen, but writing it erases the pixels); still images touched by that
  repair are re-emitted;
- unchanged image over intact cells → nothing is sent.

A full clear happens only when the cell size changes. Images are always emitted
in `frame_flush`, after the last `terminal.draw`, because writing ratatui cells
afterwards would erase them, and the whole frame is wrapped in a synchronized
update (`ESC[?2026h` … `ESC[?2026l`) to avoid intermediate states.

The sixel string for a (path, cols, rows, cell size) is encoded once — PNG
scaled to `cols*cell_w × rows*cell_h` and quantised by `icy_sixel` — and kept
in an LRU cache capped at 48 MiB. Images that would touch the **last screen
row** are never emitted: foot moves the cursor to the next line after an image
and the whole screen would scroll.

## Tests

`sixel.rs` has unit tests for PNG → sixel encoding, missing files, rewritten
cell detection, wide characters, the last-row rule and LRU eviction.
