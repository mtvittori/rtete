//! Kitty graphics protocol rendering for stickers (Kitty, Ghostty, WezTerm).
//!
//! Plain escape codes over stdout — no helpers, no extra processes. Detection
//! runs once at startup, before the TUI takes over stdin: we check the
//! environment variables instead of querying with `a=q` (a blocking probe
//! would eat the first line of typed input, e.g. the phone number at login).
//!
//! Transmission happens ONCE per image path (`a=T,t=f`: the terminal reads
//! the file itself, nothing base64'd over the pty). Every frame only re-runs
//! the cheap `a=p` placement at the current position; images that disappear
//! from the layout are deleted by id (`a=d,d=i`) — the old full `a=d,d=A`
//! wipe on every layout change forced a retransmit of everything (flicker).
//! Set `RTETE_B64=1` to fall back to the chunked base64 transmission
//! (for a terminal that accepts the protocol but not `t=f`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use base64::Engine as _;
use crossterm::{cursor, execute, style::Print};

static SUPPORT: AtomicBool = AtomicBool::new(false);
static CELL_W: AtomicU32 = AtomicU32::new(10);
static CELL_H: AtomicU32 = AtomicU32::new(20);
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

const CHUNK: usize = 4096;

/// path già trasmesso → id: il terminale ha i dati, costi solo a=p
fn sent_map() -> &'static Mutex<HashMap<String, u32>> {
    static M: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// id piazzati nel frame in corso (più recente)
fn cur_frame() -> &'static Mutex<Vec<(String, u32)>> {
    static M: OnceLock<Mutex<Vec<(String, u32)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(Vec::new()))
}

/// id piazzati nel frame precedente (per la pulizia a=id di quelli andati)
fn prev_frame() -> &'static Mutex<Vec<(String, u32)>> {
    static M: OnceLock<Mutex<Vec<(String, u32)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(Vec::new()))
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
        || std::env::var("RTETE_FORCE_KITTY").is_ok()
}

/// Rileva il supporto una volta sola tramite variabili d'ambiente.
///
/// Volutamente SENZA probe con risposta su stdin: la query `a=q` andrebbe a
/// finire nel filone di una lettura bloccante "orfana" che divorerebbe la
/// prima riga digitata dall'utente (es. il telefono al login). Il wrapper
/// lancia RTete in Kitty/ghostty/wezterm, che dichiarano già le variabili giuste.
pub fn startup_detect() {
    SUPPORT.store(env_terminal_supports(), Ordering::Relaxed);
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

/// Trasmette i byte del PNG una volta per id. Default: `t=f` — il payload è
/// il percorso del file, il terminale legge da sé (niente base64 sul pty,
/// niente mega-di-payload per ogni cambio di layout). Con RTETE_B64=1 si
/// torna alla codifica base64 a blocchi.
fn transmit(id: u32, path: &Path) -> bool {
    let legacy = std::env::var("RTETE_B64")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let payload = if legacy {
        let Ok(bytes) = std::fs::read(path) else {
            return false;
        };
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let mut p = String::with_capacity(b64.len() + 1024);
        let n_chunks = b64.len().div_ceil(CHUNK);
        for (k, chunk) in b64.as_bytes().chunks(CHUNK).enumerate() {
            let Ok(chunk) = std::str::from_utf8(chunk) else {
                return false;
            };
            let m = if k + 1 == n_chunks { 0 } else { 1 };
            // senza c/r: solo deposito, il piazzamento è un a=p separato
            p.push_str(&format!("\x1b_Gf=100,a=T,i={id},q=2,m={m};{chunk}\x1b\\"));
        }
        p
    } else {
        format!(
            "\x1b_Gf=100,a=T,t=f,i={id},C=1,q=2,m=0;{}\x1b\\",
            path.display()
        )
    };
    let mut out = std::io::stdout().lock();
    let ok = std::io::Write::write_all(&mut out, payload.as_bytes()).is_ok()
        && std::io::Write::flush(&mut out).is_ok();
    ok
}

/// Piazzamento del frame: MoveTo sulla cella e `a=p` con l'id già trasmesso.
/// Costo trascurabile a ogni frame (una manciata di escape, nessun dato).
fn place(id: u32, x: u16, y: u16, w: u32, h: u32) -> bool {
    let ctrl = format!("\x1b_Ga=p,i={id},c={w},r={h},q=2\x1b\\");
    let mut out = std::io::stdout().lock();
    if execute!(out, cursor::MoveTo(x, y)).is_err() {
        return false;
    }
    std::io::Write::write_all(&mut out, ctrl.as_bytes()).is_ok()
        && std::io::Write::flush(&mut out).is_ok()
}

/// Draw the PNG anchored at cell (x, y) sized `w x h` cells. Returns an
/// image id for later cleanup. L'id va in guarda finché il path resta
/// trasmesso: un nuovo piazzamento dello stesso path con la stessa immagine
/// non ritrasmette niente.
pub fn draw_png(path: &Path, (x, y): (u16, u16), (w, h): (u32, u32)) -> Option<u32> {
    if !supported() || w == 0 || h == 0 || !path.is_file() {
        return None;
    }
    let key = path.to_string_lossy().into_owned();
    let id = {
        let mut sent = sent_map().lock().ok()?;
        match sent.get(&key) {
            Some(&id) => id,
            None => {
                let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                if !transmit(id, path) {
                    return None;
                }
                sent.insert(key.clone(), id);
                id
            }
        }
    };
    if !place(id, x, y, w, h) {
        return None;
    }
    if let Ok(mut cur) = cur_frame().lock() {
        cur.push((key, id));
    }
    Some(id)
}

/// Inizio del frame: il registro dei piazzati correnti si riallinea.
pub fn frame_begin() {
    if !supported() {
        return;
    }
    if let Ok(mut cur) = cur_frame().lock() {
        cur.clear();
    }
}

/// Fine del frame: le immagini piazzate nel frame precedente che il frame
/// attuale non ha più vengono rimosse per id (a=d,d=i) e il loro path esce
/// dalla mappa delle trasmissioni (i dati sono andati: se tornano, si
/// ritrasmettono con un id nuovo).
pub fn frame_end() {
    if !supported() {
        return;
    }
    let Ok(cur) = cur_frame().lock() else { return };
    let Ok(mut prev) = prev_frame().lock() else { return };
    let cur_ids: std::collections::HashSet<u32> = cur.iter().map(|(_, id)| *id).collect();
    let dead: Vec<(String, u32)> = prev
        .iter()
        .filter(|(_, id)| !cur_ids.contains(id))
        .cloned()
        .collect();
    if !dead.is_empty() {
        let mut ctrl = String::new();
        for (_, id) in &dead {
            ctrl.push_str(&format!("\x1b_Ga=d,d=i,i={id},q=1\x1b\\"));
        }
        let mut out = std::io::stdout().lock();
        let _ = std::io::Write::write_all(&mut out, ctrl.as_bytes());
        let _ = std::io::Write::flush(&mut out);
        if let Ok(mut sent) = sent_map().lock() {
            for (path, _) in dead {
                sent.remove(&path);
            }
        }
    }
    *prev = cur.clone();
}

/// Remove every displayed image (and forget every transmitted path).
/// Call on exit / panic restore: la ricostruzione a frame ripare tutto.
pub fn clear_all() {
    if !supported() {
        return;
    }
    let mut out = std::io::stdout().lock();
    let _ = execute!(out, Print("\x1b_Ga=d,d=A,q=1\x1b\\"));
    let _ = std::io::Write::flush(&mut out);
    if let Ok(mut sent) = sent_map().lock() {
        sent.clear();
    }
    if let Ok(mut cur) = cur_frame().lock() {
        cur.clear();
    }
    if let Ok(mut prev) = prev_frame().lock() {
        prev.clear();
    }
}
