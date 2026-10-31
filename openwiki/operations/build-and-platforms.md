---
type: operations
title: Build, features and platforms
description: How to build Rondine's binaries and feature combinations, what the release profile implies, the Windows SQLite link workaround, platform differences between Linux, macOS and Windows, the runtime environment variables, external tools, and the scheduled OpenWiki workflow.
tags: [build, cargo, features, windows, linux, environment-variables, ci]
verified:
  - by: openwiki/0.7.2
    at: 2026-10-27T16:14:44.132Z
sources:
  - id: openwiki-source-6d4b4e707b8d60b6ccfa3425
    resource: repo://.github/workflows/openwiki-update.yml
  - id: openwiki-source-36f054f1f38f2197a6af5bca
    resource: repo://build.rs
  - id: openwiki-source-651d1fb6c9e49916a916ab51
    resource: repo://Cargo.toml
  - id: openwiki-source-f8b6ed185c6e26f9b0242389
    resource: repo://src/backend/wa/mod.rs
  - id: openwiki-source-0cf72d8af742ec949f01473b
    resource: repo://src/clipboard.rs
  - id: openwiki-source-6aa7b8ef076b4ef4f9683a99
    resource: repo://src/gfx.rs
  - id: openwiki-source-f57a68e955fb9bbaf4aac2fd
    resource: repo://src/gui/net.rs
  - id: openwiki-source-891fdbcad23ece13326c0724
    resource: repo://src/kitty.rs
  - id: openwiki-source-ed8bf05e307c6278442542c2
    resource: repo://src/lib.rs
  - id: openwiki-source-571998b1877b0f8daf274c97
    resource: repo://src/view.rs
  - id: openwiki-source-89402e3f86a3a076c01e7039
    resource: repo://src/worker.rs
generated: { by: "claude-code", at: "2026-10-27T16:14:44.132Z" }
---

# Build, features and platforms

Rondine is a single Cargo package (`rondine`, edition 2021). The README states
that only a stable Rust toolchain is needed — no system libraries (SQLite is
bundled through grammers' libsql, TLS through rustls in `ureq`).

## Binaries and features

| Command | Produces |
| --- | --- |
| `cargo build --release` | `target/release/rondine` (TUI, Telegram) |
| `cargo build --release --features whatsapp` | TUI with `--whatsapp` / `-w` |
| `cargo build --release --features gui --bin rondine-gui` | GUI, Telegram only |
| `cargo build --release --features gui,whatsapp` | GUI with both environments |

- `gui` → `iced` 0.14 (wgpu + tiny-skia renderers, Wayland and X11, `qr_code`,
  `image`, `tokio`) and `rfd` (XDG portal file picker). `rondine-gui` has
  `required-features = ["gui"]`.
- `whatsapp` → `whatsapp-rust` 0.7 (signal, tokio transport, ureq client),
  `whatsapp-rust-sqlite-storage` with default features off, `serde`, `qrcode`.
- Always on: `grammers-client`/`grammers-session` 0.11 (sqlite storage),
  `ratatui` 0.29, `crossterm` 0.28, `tokio`, `image` (webp/png/jpeg/gif only),
  `png`, `icy_sixel`, `ureq` 3 (rustls, json), `jiff`, `rpassword`, `md5`.
- `arboard` is added only for `cfg(not(target_os = "linux"))`.

Architecture background: [Architecture overview](../architecture/overview.md).

## Release profile

```
opt-level = 2, lto = true, codegen-units = 1, strip = true, panic = "abort"
```

`panic = "abort"` means destructors do not run on panic. The TUI therefore
restores the terminal from a panic hook as well as from its `TermGuard`
(see [Terminal UI](../frontends/tui.md#terminal-lifecycle-termrs)).

## SQLite: one copy, and the Windows stub

grammers' sqlite session uses libsql, which statically links SQLite. The
WhatsApp storage crate is used **without** its own SQLite copy, because two
static copies in one binary would not link (comment in `Cargo.toml`). On
Windows with `--features whatsapp`, `libsqlite3-sys` still asks the linker for
`sqlite3.lib`, which does not exist there. `build.rs` handles exactly this
case: when `CARGO_CFG_TARGET_OS == "windows"` and `CARGO_FEATURE_WHATSAPP` is
set, it writes an empty `sqlite3_stub.c` to `OUT_DIR`, compiles it with `cc`
into `sqlite3.lib`, and adds `OUT_DIR` to the native link search path. In every
other configuration `build.rs` does nothing.

## Platform differences

| Concern | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Config / data dirs | XDG or `~/.config`, `~/.local/share` | same rules (`$HOME`) | `%APPDATA%`, `%LOCALAPPDATA%` |
| File permissions | 0700 dirs, 0600 secret files | same (Unix) | inherited profile ACLs |
| Clipboard | `wl-copy` / `wl-paste` (Wayland; files via `text/uri-list`) | `arboard` | `arboard` |
| Open URL | `xdg-open` | `open` | `rundll32 url.dll,FileProtocolHandler` |
| GUI single instance | Unix socket in `$XDG_RUNTIME_DIR` | Unix socket | disabled (each launch opens a window) |
| GUI window class | `application_id` set (Hyprland rules) | — | — |
| foot detection via `/proc` | yes | no | no |

Desktop integrations in the GUI shell out to Linux tools: `notify-send` and
`gdbus` (notifications), `hyprctl` (focus an existing window), `xdg-mime`
(register the `tg://` handler). Playback needs `mpv` or `ffplay` on `PATH` on
any OS. Details in [Storage and paths](../concepts/storage-and-paths.md),
[Desktop GUI](../frontends/gui.md) and
[Downloads, playback, clipboard and links](../features/media-actions-and-clipboard.md).

## Runtime environment variables

All `RONDINE_*` variables also accept the legacy `RTETE_*` prefix
(`rondine::env_var`).

| Variable | Effect |
| --- | --- |
| `RONDINE_PLAYER` | player binary instead of mpv/ffplay |
| `RONDINE_SCALE` | TUI image size multiplier (0.5–4) |
| `RONDINE_CELL_W`, `RONDINE_CELL_H` | override terminal cell size in pixels |
| `RONDINE_FORCE_KITTY`, `RONDINE_FORCE_SIXEL` | force a graphics protocol |
| `RONDINE_B64` | kitty: send base64 image data instead of file paths |
| `RONDINE_WA_PHONE` | TUI WhatsApp: pair with an 8-char code instead of QR |
| `ICED_BACKEND` | GUI renderer (`tiny-skia` / `wgpu`); overrides the GPU setting |
| `LC_ALL`, `LC_MESSAGES`, `LANG` | UI language detection |
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_DOWNLOAD_DIR`, `XDG_RUNTIME_DIR`, `XDG_STATE_HOME` | directories (state: Omarchy theme) |

## Memory expectations

From the README: TUI RSS is typically tens of MB; the GUI uses about 75 MB with
software rendering (tiny-skia) and about 105 MB with the GPU (wgpu). The code
keeps RAM flat with bounded histories, disk-backed media and at most two
concurrent conversions (see [Media cache and conversion](../concepts/media-cache-and-conversion.md)).

## OpenWiki workflow

`.github/workflows/openwiki-update.yml` runs daily at 08:00 UTC and on manual
dispatch: full-history checkout, Node 22, `npm install -g openwiki@0.7.2`
(plus mermaid/jsdom for diagram validation), `openwiki code --update --print`
with an OpenAI provider and optional LangSmith tracing from repository secrets,
removal of `openwiki/.run.json`, then `peter-evans/create-pull-request` on
branch `openwiki/update` with the `openwiki/` directory, `AGENTS.md`, the
workflow file and `CLAUDE.md` if present. A failed run still opens a PR with the
pages completed so far, and the job then fails. There is no CI workflow that
builds or tests the Rust code.

## Tests

`cargo test` runs the inline unit tests; add `--features whatsapp` (and `gui`)
to include the tests of those modules. See [Testing](../testing/unit-tests.md).
