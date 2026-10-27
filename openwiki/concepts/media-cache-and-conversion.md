---
type: concept
title: Media cache and conversion
description: Rondine's low-RAM media pipeline — how stickers, photos, GIFs and video thumbnails are downloaded once into an on-disk asset cache, converted to PNG/APNG off the async runtime, tracked in App state, and pruned to a size cap.
tags: [media, cache, png, apng, gif, conversion, low-ram, avatars]
verified:
  - by: openwiki/0.7.2
    at: 2026-10-27T16:14:44.132Z
sources:
  - id: openwiki-source-9fec9d1af408501c9c455161
    resource: repo://src/app.rs
  - id: openwiki-source-1317db6898e1cf0daa186e3d
    resource: repo://src/backend/tg/mod.rs
  - id: openwiki-source-f57a68e955fb9bbaf4aac2fd
    resource: repo://src/gui/net.rs
  - id: openwiki-source-73d3b88eef9cdf3ebbf1dd27
    resource: repo://src/gui/settings.rs
  - id: openwiki-source-4466ece223872f4ff62d48b9
    resource: repo://src/loader.rs
  - id: openwiki-source-514af1f9d4353b80cbbf634c
    resource: repo://src/paths.rs
generated: { by: "claude-code", at: "2026-10-27T16:14:44.132Z" }
---

# Media cache and conversion

The project's RAM goal shapes the media pipeline: no decoded image lives in the
heap longer than one conversion, and every visible asset ends up as a PNG (or
APNG) file on disk that the renderer reads by path. The kitty terminal protocol
reads the file itself; sixel and iced load it when needed (see
[Terminal graphics](../frontends/terminal-graphics.md) and
[Desktop GUI](../frontends/gui.md)).

## Flow

```
App::refresh_preview_queue / ensure_previews     (state decides what is visible)
        │ Request::Conv(MediaRef)      ── skipped if in previews, pending, or backing off
        ▼
backend dispatcher  ── Semaphore(2) ──▶ worker::conv_job(key, backend::fetch_raw)
        │                                   │
        │                     loader::convert_to_png(key, fetch)
        │                       ├─ assets/<key>.png exists? → done
        │                       ├─ fetch → assets/<key>.<ext>  (raw original, downloaded once)
        │                       └─ spawn_blocking(convert_sync) → assets/<key>.png
        ▼
Response::Converted((key, Result<PathBuf>)) ──▶ sync: previews[key] = path | conversion_failed(key)
```

## Cache layout

All files live in `paths::media_dir()` = `<data>/rondine/assets/`:

| File | Producer |
| --- | --- |
| `<key>.<ext>` | `loader::cache_raw` — the original as downloaded (`jpg`, `png`, `webp`, `gif`, `bin`) |
| `<key>.png` | `loader::cache_png` — the converted, renderable image |
| `avatars/<photo_id>.jpg` | `paths::avatar_path` — small profile pictures |

`key` comes from the backend's `MediaRef`: Telegram uses the hex MD5 of the
server media id; WhatsApp builds its own stable key. The cache is shared across
media types and across backends.

## Fetching originals

Each backend provides `fetch_raw(client, media) -> PathBuf`, which downloads
the original into `cache_raw` only if absent:

- **Telegram**: for video documents only the largest thumbnail is downloaded
  (as `jpg`), never the video.
- **WhatsApp**: for videos the JPEG thumbnail embedded in the message is
  written directly; other media are downloaded and decrypted from the CDN.

## Conversion rules (`loader::convert_sync`)

The input format is sniffed from magic bytes, not trusted from the extension:

| Input | Output |
| --- | --- |
| APNG (PNG with an `acTL` chunk in the first 1 KiB) | bytes copied unchanged — kitty animates it |
| static PNG ≤ 1000 px on the long side | bytes copied |
| static PNG larger, JPEG, WebP, other | decoded with the long side capped at 1000 px (`decode_cap`), re-encoded as RGBA PNG |
| GIF ≤ 700 px | re-encoded as APNG (max 80 frames) by `gif_to_apng` |
| GIF larger | first frame, capped and encoded as PNG |

`gif_to_apng` keeps RAM flat with two passes: the first counts frames (the APNG
header needs the exact count), the second decodes and writes one frame at a
time. A source comment notes that accumulating frames previously cost up to
~150 MB (80 RGBA frames at 700×700). Frame delays are clamped to 20–600 ms and
also applied to the first frame.

`convert_to_png` runs the synchronous conversion in `spawn_blocking`. If the
conversion fails or the blocking task panics, the partial `<key>.png` is
removed — otherwise `exists()` would return a broken file forever.

Animated Telegram stickers (TGS/Lottie) and video stickers (WebM) are not
converted: `MediaPrefs::auto` excludes animated stickers, and the README
explains that rendering them would need heavy C libraries (rlottie/libvpx).

## State tracking and retries

In `App` (see [App state and sync](../architecture/app-state-and-sync.md)):

- `previews: HashMap<key, PathBuf>` — converted files ready to draw;
- `pending: HashSet<key>` — conversions in flight (never requested twice);
- `failed: HashMap<key, (Instant, u32)>` — exponential backoff 1 s → 60 s, so a
  broken asset is not re-downloaded on every layout change.

`MediaPrefs` (GUI settings; the TUI uses defaults = all on) decides which rows
auto-convert: static stickers always, photos/GIFs if `photos`, video thumbnails
if `video_thumbs`. The GUI's "load preview" action requests a single preview
when auto-download is off.

## Concurrency

Both dispatchers limit conversions to two at a time with a `Semaphore(2)`. On
Telegram the avatar downloads share that semaphore because the whole chat list
requests avatars at startup; WhatsApp uses a separate `Semaphore(4)` for
avatar lookups.

## Avatars

`App::request_avatars` sends one `Request::Avatar` per `photo_id` not yet
known, first checking whether `avatars/<photo_id>.jpg` already exists. Telegram
downloads the small (non-"big") peer photo into a `.part` file and renames it,
so a half-written file never looks cached. Failed ids stay in `avatar_asked`
and are not retried during the session.

## Pruning

`loader::prune_cache_to(max_bytes)` lists the regular files directly inside
`assets/` (not subdirectories, so `avatars/` is not pruned here), sorts them
newest-first by mtime and deletes everything beyond the byte cap or the file
cap. The file cap is `max(200, max_bytes / 64 KiB)`.

- TUI: `prune_cache()` at startup with the default 32 MiB / 200 files, before
  any image reaches the terminal.
- GUI: `net::start` prunes to the configured `cache_mb` (choices 32, 128, 512,
  2048 MB in *Settings → Data and storage*); "prune now" applies it
  immediately. The same page shows usage per area (`dir_usage`) and can clear
  the media cache, avatars or the temp directory (`clear_dir`).

See [On-disk storage and paths](storage-and-paths.md) for the directory roots.
