//! Kitty-graphics-protocol rendering for stickers (Kitty, Ghostty, WezTerm).
//!
//! Plain escape codes over stdout — no helpers, no extra processes. Detection
//! runs once at startup, before the TUI takes over stdin: we query with
//! `a=q` and wait a moment for the terminal's reply.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use base64::Engine as _;
use crossterm::{cursor, execute, style::Print};

static SUPPORT: AtomicBool = AtomicBool::new(false);
static CELL_W: AtomicU32 = AtomicU32::new(10);
static CELL_H: AtomicU32 = AtomicU32::new(20);
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

const CHUNK: usize = 4096;

/// Ask the terminal `a=q` and look for the `_G` reply on stdin. Must run
/// before crossterm's event stream owns stdin.
async fn handshake() -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";
    let mut out = tokio::io::stdout();
    if out.write_all(query).await.is_err() || out.flush().await.is_err() {
        return false;
    }

    let mut buf = [0u8; 256];
    let mut stdin = tokio::io::stdin();
    let read = tokio::time::timeout(Duration::from_millis(400), stdin.read(&mut buf)).await;
    match read {
        Ok(Ok(n)) => {
            let hay = String::from_utf8_lossy(&buf[..n]);
            // A graphics-capable terminal echoes `i=31;…`; an erroring reply
            // (`i=31;error…`) means the sequence was understood but Gina failed.
            hay.contains("\x1b_G") && hay.contains("i=31;")
        }
        _ => false,
    }
}

fn env_terminal_supports() -> bool {
    let term = std::env::var("TERM").unwrap_or_default();
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    term == "xterm-kitty"
        || std::env::var("KITTY_WINDOW_ID").is_ok()
        || term_program.eq_ignore_ascii_case("ghostty")
        || term_program.eq_ignore_ascii_case("WezTerm")
        || std::env::var("GHOSTTY_RESOURCES_DIR").is_ok()
        || std::env::var("WEZTERM_PANE").is_ok()
}

/// Probe the terminal once. Call at startup, before raw mode.
pub async fn startup_detect() {
    let ok = handshake().await;
    SUPPORT.store(ok || env_terminal_supports(), Ordering::Relaxed);
}

pub fn supported() -> bool {
    SUPPORT.load(Ordering::Relaxed)
}

/// Estimated terminal cell size in pixels (kitty cells are ~1:2). Tune via
/// RTETE_CELL_W / RTETE_CELL_H for pixel-perfect sticker sizing.
pub fn cells() -> (f32, f32) {
    let w = std::env::var("RTETE_CELL_W")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v >= 2)
        .unwrap_or(CELL_W.load(Ordering::Relaxed)) as f32;
    let h = std::env::var("RTETE_CELL_H")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v >= 2)
        .unwrap_or(CELL_H.load(Ordering::Relaxed)) as f32;
    (w, h)
}

/// Size a physically-square image into whole cells inside `max_w x max_h`.
/// Returns (columns, rows).
pub fn square_fit(max_w: u32, max_h: u32) -> (u32, u32) {
    let (cw, ch) = cells();
    if cw < 1.0 || ch < 1.0 || max_w == 0 || max_h == 0 {
        let n = max_w.min(max_h).max(1);
        return (n, n);
    }
    let rows = (max_h as f32)
        .min((max_w as f32) * cw / ch)
        .floor()
        .max(1.0);
    let cols = (rows * ch / cw).round().max(1.0);
    (cols as u32, rows as u32)
}

/// Draw the PNG anchored at cell (x, y) sized `w x h` cells. Returns an
/// image id for later cleanup.
pub fn draw_png(path: &Path, (x, y): (u16, u16), (w, h): (u32, u32)) -> Option<u32> {
    if !supported() || w == 0 || h == 0 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

    let n_chunks = b64.len().div_ceil(CHUNK);
    let mut payload = String::with_capacity(b64.len() + n_chunks * 48);
    for (k, chunk) in b64.as_bytes().chunks(CHUNK).enumerate() {
        let chunk = std::str::from_utf8(chunk).ok()?;
        let m = if k + 1 == n_chunks { 0 } else { 1 };
        if k == 0 {
            payload.push_str(&format!(
                "\x1b_Gf=100,a=T,i={id},c={w},r={h},C=1,q=2,m={m};{chunk}\x1b\\"
            ));
        } else {
            payload.push_str(&format!("\x1b_Gm={m};{chunk}\x1b\\"));
        }
    }

    let mut out = std::io::stdout().lock();
    execute!(out, cursor::MoveTo(x, y)).ok()?;
    std::io::Write::write_all(&mut out, payload.as_bytes()).ok()?;
    std::io::Write::flush(&mut out).ok()?;
    Some(id)
}

/// Remove every displayed image. Call before full repaints and on exit.
pub fn clear_all() {
    if !supported() {
        return;
    }
    let mut out = std::io::stdout().lock();
    let _ = execute!(out, Print("\x1b_Ga=d,d=A,q=1\x1b\\"));
    let _ = std::io::Write::flush(&mut out);
}
