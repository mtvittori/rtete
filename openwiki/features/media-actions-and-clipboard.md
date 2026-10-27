---
type: feature
title: Downloads, playback, clipboard and links
description: Backend-neutral user actions in Rondine — saving media to Downloads, audio/video playback through an external player, clipboard copy and paste of text, images and files, Telegram link recognition and resolution, and locally stored starred messages.
tags: [downloads, player, mpv, clipboard, wl-clipboard, arboard, links, starred]
verified:
  - by: openwiki/0.7.2
    at: 2026-10-27T16:14:44.132Z
sources:
  - id: openwiki-source-651d1fb6c9e49916a916ab51
    resource: repo://Cargo.toml
  - id: openwiki-source-9fec9d1af408501c9c455161
    resource: repo://src/app.rs
  - id: openwiki-source-1317db6898e1cf0daa186e3d
    resource: repo://src/backend/tg/mod.rs
  - id: openwiki-source-0cf72d8af742ec949f01473b
    resource: repo://src/clipboard.rs
  - id: openwiki-source-2d54f64140ade8c180921259
    resource: repo://src/links.rs
  - id: openwiki-source-38251fdb0c9989d14c9dda87
    resource: repo://src/starred.rs
  - id: openwiki-source-89402e3f86a3a076c01e7039
    resource: repo://src/worker.rs
generated: { by: "claude-code", at: "2026-10-27T16:14:44.132Z" }
---

# Downloads, playback, clipboard and links

These features are implemented once in the shared library and used by both
backends and both frontends. Backends only supply a "download to this path"
closure; everything else lives in `src/worker.rs`, `src/clipboard.rs`,
`src/links.rs` and `src/starred.rs`.

## Saving media (`worker::save_job`)

Triggered by `Request::Save { media, name }` (TUI `Ctrl-O`, GUI "save"):

1. `paths::sanitize_file_name(name)`; an unusable name is an error.
2. Download into `<downloads>/.rondine-<ms>.part` via the backend closure
   (`tg::download_media_to` / `wa::download_to`).
3. On success, rename to `paths::unique_save_path(dir, name)` — never
   overwrites, appends ` (1)`, ` (2)` …
4. Always remove the `.part`.

The result is `Response::Saved(Ok(path))`, shown as "salvato in …". See
[On-disk storage and paths](../concepts/storage-and-paths.md).

## Playback (`worker::Player`, `play_job`)

There is no audio library in the binary: playback spawns an external player in
its own process (README: "0 extra RAM").

- The player is `RONDINE_PLAYER` (or legacy `RTETE_PLAYER`) if set, otherwise
  the first of `mpv`, `ffplay` found on `PATH` (`worker::which`, which handles
  `.exe` and path separators per OS).
- Arguments: mpv `--no-video --no-terminal --really-quiet` for audio,
  `--no-terminal --really-quiet --title=Rondine` for video; ffplay
  `-nodisp -autoexit` for audio, `-autoexit` for video.
- The file is first downloaded to `<data>/rondine/tmp/play-<gen>-<name>` and
  deleted when playback ends or is replaced.
- **One track at a time**: each `Player::play` increments a global generation
  counter and signals the previous job through a `tokio::sync::watch` channel;
  the old job kills its child (`kill_on_drop` as a safety net), deletes its
  temp file and does *not* send a late "finished" status. A code comment notes
  this replaced killing by PID, which could hit an unrelated reused PID.

## Clipboard (`clipboard.rs`)

| Platform | Copy | Paste probe |
| --- | --- | --- |
| Linux | `wl-copy` with text on **stdin** (an argument starting with `-` would be parsed as an option) | `wl-paste --list-types`; then `text/uri-list` (files), then `image/png`/`image/jpeg`, then text |
| other OS | `arboard` | image (converted RGBA → PNG), then text; files copied in a file manager are not read |

`arboard` is a dependency only for `cfg(not(target_os = "linux"))`
(see [Build, features and platforms](../operations/build-and-platforms.md)).

Paste never blocks the UI loop: `ctrl_paste` runs `probe_clipboard` on a
separate thread and returns the result as `Response::Clipboard(ClipboardHit)`;
`handle_clipboard` re-reads UI state at delivery time:

- **Image** → written to `<data>/rondine/tmp/appunti-<ms>.{png,jpg}` with 0600
  permissions inside the 0700 data dir, set as `pending_photo`; Enter sends it
  as a photo with the compose text as caption, and the temp file is deleted
  after upload.
- **Files** (uri-list, `file://` with percent-decoding, host part skipped) →
  the first file becomes `pending_file` (kept on disk after sending).
- **Text** → into the search box (newlines become spaces, max 200 chars) or the
  compose box (newlines kept, capped to the remaining 4096/1024-char Telegram
  limit, control characters dropped).

`ctrl_copy` copies the selected message's text (or caption, sticker emoji,
note) — or the last message with text if nothing is selected.

## Telegram links (`links.rs`)

`parse_link` recognises Telegram URLs and returns `None` for everything else
(which goes to the browser via `open_external`: `xdg-open`, `open` on macOS,
`rundll32 url.dll,FileProtocolHandler` on Windows to avoid `cmd` splitting URLs
on `&`):

| Input | `TgLink` |
| --- | --- |
| `t.me/<name>[/<post>]`, `t.me/s/<name>`, `tg://resolve?domain=…&post=…` | `Public { name, post }` |
| `t.me/+<hash>`, `t.me/joinchat/<hash>`, `tg://join?invite=…` | `Invite(hash)` |
| `t.me/c/<id>/<post>`, `tg://privatepost?channel=…&post=…` | `Private { channel, post }` |
| service paths (`addstickers`, `share`, `proxy`, `login`, …) | `None` |

`channel_chat(id)` converts a private-link channel id into the Bot-API
`ChatId` (`-(10^12 + id)`) used by the Telegram backend, so a private link to a
chat the user already has can open it locally. Resolution is Telegram-only
(`Request::Link`/`Join`): `resolve_link` resolves usernames into a ready
`DialogInfo`, checks invites with `messages.checkChatInvite` (already a member
→ `LinkHit::Already`, otherwise `LinkHit::Invite` with title and member count
for a confirmation), and `join_invite` imports the invite. Private links to
chats the user is not in fail with "chat privata: non è fra le tue". The
WhatsApp backend answers these requests as unsupported.

In the GUI, links arriving as a command-line argument or from another instance
are delivered as `NetEvent::Link` (see [Desktop GUI](../frontends/gui.md)), and
*Settings → Links* can register `rondine-gui.desktop` as the `tg://` handler
via `xdg-mime`.

## Starred messages (`starred.rs`)

Telegram has no message stars, so starring is local: `App::toggle_star` keeps a
`Vec<Star>` (chat id, chat name, message id, date, author, a **copy of the
text** or the row summary) and saves it to `<data>/rondine/starred.json` on
every change, so the list is readable offline. On WhatsApp the backend
additionally syncs the star with the phone (`Request::Star`); the Telegram
dispatcher ignores that request.

## Tests

- `links.rs`: `parse_link` formats and `channel_chat` Bot-API mapping.
- `clipboard.rs`: `uri_list_paths` / percent decoding.
- `tg/mod.rs`: `chat_id_andata_e_ritorno` checks that channel ids match
  `links::channel_chat`.
