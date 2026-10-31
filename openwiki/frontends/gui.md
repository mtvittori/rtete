---
type: frontend
title: Desktop GUI (iced)
description: Architecture of the rondine-gui binary — the dedicated tokio runtime bridging backends to iced, the Telegram/WhatsApp environments with parked state, unified feed and solo instances, single-instance hand-off, in-window login and pairing, settings, themes (including Omarchy) and desktop notifications.
tags: [gui, iced, tokio, environments, single-instance, notifications, themes, hyprland]
verified:
  - by: openwiki/0.7.2
    at: 2026-10-27T16:14:44.132Z
sources:
  - id: openwiki-source-f985e52353b94464277d2b2d
    resource: repo://src/gui/main.rs
  - id: openwiki-source-f57a68e955fb9bbaf4aac2fd
    resource: repo://src/gui/net.rs
  - id: openwiki-source-7a5825e480d75567b02eb04e
    resource: repo://src/gui/notify.rs
  - id: openwiki-source-73d3b88eef9cdf3ebbf1dd27
    resource: repo://src/gui/settings.rs
  - id: openwiki-source-b240b1b3f293de51103377cb
    resource: repo://src/gui/style.rs
generated: { by: "claude-code", at: "2026-10-27T16:14:44.132Z" }
---

# Desktop GUI (iced)

`rondine-gui` (`--features gui`, entry `src/gui/main.rs`) reuses the whole
shared library — backends, workers, media cache, `App` and `sync` — and
replaces the TUI with an [iced](https://iced.rs) 0.14 interface: chat list on
the left, bubble history, real images for stickers and photos, a chat info
panel and Telegram-like settings pages.

| File | Role |
| --- | --- |
| `gui/main.rs` | `Gui` state, `Msg` enum, `update`/`handle`, environment switching, startup, single instance |
| `gui/net.rs` | bridge between tokio backends and iced: runtime, event channel, login and pairing flows |
| `gui/ui.rs` | all views (sidebar, chat, bubbles, composer, stickers, settings, info panel, dialogs) |
| `gui/settings.rs` | `Settings` persisted in `gui.conf`; solo mode; `tg://` handler via `xdg-mime` |
| `gui/style.rs` | global hot-swappable palette (Rondine, WhatsApp, Omarchy) and widget styles |
| `gui/notify.rs` | desktop notifications through `notify-send` |

## Startup

`main()`:

1. `paths::migrate_old_dirs()`; set the UI language from settings.
2. Renderer: unless `ICED_BACKEND` is already set, the `gpu = false` setting
   forces `ICED_BACKEND=tiny-skia` (software). README figures: ~75 MB with
   tiny-skia, ~105 MB with wgpu.
3. `--telegram`/`-t` or `--whatsapp`/`-w` (whatsapp builds only) select a
   **solo** environment; the first non-flag argument is a link (ignored in a
   WhatsApp-only instance).
4. **Single instance**: try `net::hand_off` to the sockets of instances that
   would share the same sessions (`rondine-gui`, `rondine-telegram`,
   `rondine-whatsapp`); if one answers, pass it the link, focus it and exit.
5. `net::start(app_id, telegram, link, cache_bytes)` and, if a WhatsApp session
   exists, `net::start_whatsapp(None)`.
6. Run the iced application with window class / Wayland app id `app_id()`
   (`rondine-gui`, `rondine-telegram` or `rondine-whatsapp`), 1100×760.

### Single instance (Unix only)

`net::listen` binds `$XDG_RUNTIME_DIR/<app_id>.sock` (fallback temp dir),
removing a stale socket first; each line received is a link forwarded as
`NetEvent::Link`. `focus_existing` asks Hyprland (`hyprctl dispatch …`) to focus
the window class. On Windows `hand_off` always returns false: every launch
opens its own window.

## Network bridge (`net.rs`)

iced runs its own executor, so the network lives on a separate multi-threaded
tokio runtime with 2 worker threads stored in the `RT` `OnceLock`. All backend
output for both environments flows into one
`mpsc::UnboundedSender<(Env, NetEvent)>`:

```
Telegram run() ─┐                              ┌─ NetEvent::Status / Login / Ready / Failed
WhatsApp run_wa()┼─▶ (Env, NetEvent) channel ──▶ Subscription::run(net::events) ─▶ Msg::Net(Carrier)
socket listener ─┤                              └─ NetEvent::Response(Response) per backend
notify click ───┘  (net::open_peer)
```

`Carrier` wraps the event in `Arc<Mutex<Option<…>>>` because iced messages must
be `Clone` while `NetEvent` is not; the receiver `take()`s it once.

`NetEvent` variants: `Status(step, text)` (startup steps 0–2), `Ready { link,
contacts, me }` (with the per-backend `Request` sender), `Failed`,
`Response`, `Link`, `OpenPeer`, `Pair(Pairing)`, `Login(Login)`.

**Telegram in the window** (`run`): ask for API ID + hash if no credentials
file exists (validated: numeric id, hash ≥ 16 chars), connect with doubling
retry, and if not authorised drive `tg::sign_in_steps` — questions are sent as
`NetEvent::Login(Phone | Code | Password)` and answers return through
`net::answer`. Then load dialogs, start the Telegram worker, emit `Ready`, and
forward every `Response`. See [Telegram backend](../backends/telegram.md).

**WhatsApp** (`run_wa`): call `wa::start_with` with a `PairSink` that maps QR /
code / paired / status into `NetEvent::Pair`; optional phone number for code
pairing. See [WhatsApp backend](../backends/whatsapp.md).

## Environments: active and parked

A `Gui` shows one environment (`env: Env`, which is `UiStyle::Telegram` or
`UiStyle::WhatsApp`) and keeps the other as `parked: Option<Parked>` — its own
`App`, `Link` and `Phase`. `Phase` is `Connecting(step, text)`, `Failed`,
`Ready`, `Linking` (WhatsApp not linked yet), `Login(..)` or
`Pairing { qr, code }`.

- `switch_env` (env rail, Ctrl+1/Ctrl+2, notification click) swaps `app`,
  `link` and `phase` with the parked slot, moves the shared starred list
  across, stops a pending "typing" on the old service, saves the style, applies
  that environment's theme and closes panels that belonged to the other one.
- `in_env(env, f)` applies a network event to the hidden environment by
  swapping it in temporarily with `background = true` (scroll/focus tasks are
  discarded).
- **Unified feed** (`Settings::mixed()`: `unified` on, not solo, whatsapp
  feature): both chat lists appear in one list ordered by date with a service
  badge; actions on the other environment's chats are wrapped as
  `Msg::InEnv(env, msg)`. The feed has its own style and theme
  (`style_mix`, `theme_mix`).
- **Solo instances** keep their own window, socket and theme; `Settings::save`
  in a solo instance preserves the other instance's style/theme on disk.
- If WhatsApp is unlinked from the devices page (`SessionEnded` in the
  WhatsApp environment), its `App` is reset and the phase returns to `Linking`.

## Update loop

`update` wraps `handle(msg)` and starts animations (sticker panel, message
action bar) from state changes. After message handling, `after_change` runs the
same state-driven requests the TUI loop runs: `request_older`,
`request_quoted`, `request_avatars`, `refresh_preview_queue`,
`flush_translations` (see [App state and sync](../architecture/app-state-and-sync.md)).
Responses are applied with the shared `handle_live` / `handle_response`; the
history snaps to the bottom on the first page or when already at the bottom.

Subscriptions: the network stream, keyboard, window focus/unfocus, file drop,
Ctrl-V outside text inputs; per-frame ticks only while an animation runs; a
timer every 80 ms while connecting (spinner), 400 ms while someone is typing,
2 s when the Omarchy theme is active, none otherwise — an idle window does no
periodic work.

Presence and typing: `Request::Presence(focused && show_online)` is sent to
both environments on focus changes; typing is sent at most every 4 s while
composing (Telegram expires it after ~6 s) and cancelled after 5 s without
keys, on blur, on send or when the compose box empties.

## Settings (`settings.rs`)

`Settings` is stored in `<config>/rondine/gui.conf` as Italian `key = value`
lines (`tema`, `scala`, `gpu`, `notifiche`, `foto_auto`, `cache_mb`, `lingua`,
`stile`, `feed_unico`, `mostra_online`, `mostra_scrivo`, …). Notable fields:
per-style theme (`theme`, `theme_wa`), favourites (Bot-API ids for the
"Favourites" filter), UI scale, links-in-app, media auto-download preferences
(mapped to `App::media_prefs`), photo size (220/320/440), cache cap
(32/128/512/2048 MB), env rail, unified feed, show online, show typing. Media and
cache behaviour is described in
[Media cache and conversion](../concepts/media-cache-and-conversion.md).

## Themes (`style.rs`)

The palette is a global `RwLock<Pal>` swapped at runtime: `RONDINE` (dark,
modelled on Telegram Desktop), `WHATSAPP` (green) or **Omarchy**, read from
`$XDG_STATE_HOME/omarchy/current/theme/colors.toml` (`background`,
`foreground`, `accent`/`blue`, optional `selection`, colour names for sender
names). The mtime of that file is polled every 2 s so a system theme change
applies live. A separate flag selects the WhatsApp layout (bubble tails, round
input, green send).

## Notifications (`notify.rs`)

New incoming messages trigger `notify-send` unless notifications are off, the
chat is muted, it is the WhatsApp status chat, or it is the open chat in the
focused window. One notification per chat is kept (`-r <id>` replaces it), the
contact avatar is the icon, the app name tells the environment, and the body is
the message text or "nuovo messaggio" if text previews are disabled. A thread
per notification waits for the "Apri" action and then opens that chat (switching
environment if needed) and focuses the window. Opening the chat or reading it
elsewhere closes the notification through `gdbus` `CloseNotification`.

## Related

- [Terminal UI](tui.md) — the other frontend over the same core.
- [Architecture overview](../architecture/overview.md).
