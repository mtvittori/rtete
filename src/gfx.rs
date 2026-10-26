//! Smistamento della grafica: kitty graphics protocol (`kitty`), sixel
//! (`sixel`, foot) o niente. È l'unico punto che `main` e `term` conoscono;
//! qui stanno anche le misure delle celle, comuni ai due protocolli.
//!
//! Il rilevamento è fatto UNA volta all'avvio dalle variabili d'ambiente, senza
//! query al terminale su stdin (vedi `kitty`: una sonda con risposta mangerebbe
//! la prima riga digitata al login). Le celle in pixel arrivano da
//! `window_size()` (ioctl `TIOCGWINSZ`, non legge stdin).

use std::io::Stdout;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;

use crate::{kitty, sixel};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    None,
    Kitty,
    Sixel,
}

static MODE: AtomicU8 = AtomicU8::new(0);
static CELL_W: AtomicU32 = AtomicU32::new(10);
static CELL_H: AtomicU32 = AtomicU32::new(20);

fn set_flag(name: &str) -> bool {
    rondine::env_var(name).is_some()
}

/// Il terminale che ospita il processo, risalendo gli antenati in /proc: il
/// primo con un nome di terminale noto. Serve per foot, che con la
/// configurazione Omarchy (`term=xterm-256color`) non mette `TERM=foot*` e non
/// esporta nessuna variabile sua. Solo Linux; altrove (o senza /proc) `None`.
/// Niente query al terminale: legge soltanto /proc.
fn host_terminal() -> Option<&'static str> {
    let mut pid = std::process::id();
    for _ in 0..32 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // "pid (comm) S ppid ...": il nome può contenere spazi e parentesi
        let ppid: u32 = stat
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        if ppid <= 1 {
            return None;
        }
        pid = ppid;
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        match comm.trim() {
            "foot" | "footclient" => return Some("foot"),
            "kitty" | "ghostty" | "wezterm-gui" | "alacritty" | "xterm" | "konsole" => return None,
            _ => {}
        }
    }
    None
}

/// Sceglie il protocollo. Ordine: le forzature esplicite, poi foot (`TERM=foot*`
/// o, se la config ha cambiato TERM, il terminale antenato), poi le variabili
/// di kitty/ghostty/wezterm. foot viene prima del test su kitty apposta:
/// KITTY_WINDOW_ID e simili sono ereditate dai figli, e un foot lanciato da
/// una shell di kitty non capirebbe i comandi `\x1b_G`.
fn detect() -> Mode {
    let term = std::env::var("TERM").unwrap_or_default();
    if set_flag("FORCE_SIXEL") {
        Mode::Sixel
    } else if set_flag("FORCE_KITTY") {
        Mode::Kitty
    } else if term.starts_with("foot") || host_terminal() == Some("foot") {
        Mode::Sixel
    } else if kitty::env_terminal_supports() {
        Mode::Kitty
    } else {
        Mode::None
    }
}

/// Da chiamare una volta, prima che la TUI prenda stdin.
pub fn startup_detect() {
    let mode = detect();
    if mode == Mode::Kitty {
        kitty::startup_detect();
    }
    MODE.store(mode as u8, Ordering::Relaxed);
    refresh_cells();
}

pub fn mode() -> Mode {
    match MODE.load(Ordering::Relaxed) {
        1 => Mode::Kitty,
        2 => Mode::Sixel,
        _ => Mode::None,
    }
}

/// C'è un protocollo grafico attivo? (kitty o sixel)
#[allow(dead_code)]
pub fn supported() -> bool {
    mode() != Mode::None
}

/// Rilegge la dimensione delle celle in pixel: pixel della finestra diviso
/// colonne e righe. Si chiama all'avvio e a ogni `Event::Resize`. Se il
/// terminale non riporta i pixel (0) restano le stime precedenti; sixel lavora
/// in pixel veri, quindi qui serve precisione.
pub fn refresh_cells() {
    let Ok(ws) = crossterm::terminal::window_size() else {
        return;
    };
    if ws.width == 0 || ws.height == 0 || ws.columns == 0 || ws.rows == 0 {
        return;
    }
    let w = (ws.width as u32 / ws.columns as u32).max(1);
    let h = (ws.height as u32 / ws.rows as u32).max(1);
    let old_w = CELL_W.swap(w, Ordering::Relaxed);
    let old_h = CELL_H.swap(h, Ordering::Relaxed);
    if (old_w, old_h) != (w, h) {
        // le immagini già a schermo hanno la vecchia scala: ridisegno completo
        sixel::invalidate();
    }
}

/// Dimensione di una cella in pixel (stima 10x20 finché la finestra non
/// riporta i pixel). RONDINE_CELL_W / RONDINE_CELL_H la forzano.
pub fn cells() -> (f32, f32) {
    let w = rondine::env_var("CELL_W")
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v >= 2)
        .unwrap_or(CELL_W.load(Ordering::Relaxed)) as f32;
    let h = rondine::env_var("CELL_H")
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

/// Prima del `terminal.draw` del frame: con sixel apre un aggiornamento
/// sincronizzato (DECSET 2026), così cancellazione, ridisegno e immagini
/// arrivano al terminale tutti insieme, senza lampi intermedi.
pub fn sync_begin() {
    if mode() == Mode::Sixel {
        sixel::sync_begin();
    }
}

/// Subito dopo ogni `terminal.draw`: con sixel serve il buffer disegnato per
/// capire quali celle ratatui ha riscritto (e quindi quali pixel sono spariti).
pub fn observe_frame(buf: &Buffer) {
    if mode() == Mode::Sixel {
        sixel::observe_frame(buf);
    }
}

/// Inizio del piazzamento immagini del frame.
pub fn frame_begin() {
    match mode() {
        Mode::Kitty => kitty::frame_begin(),
        Mode::Sixel => sixel::frame_begin(),
        Mode::None => {}
    }
}

/// Piazza il PNG alla cella (x, y) con ingombro `w x h` celle. Kitty lo
/// disegna subito; sixel lo registra e lo emette in `frame_flush`, dopo
/// l'ultimo draw di ratatui. `false` se non è stato possibile.
pub fn draw_png(path: &Path, pos: (u16, u16), size: (u32, u32)) -> bool {
    match mode() {
        Mode::Kitty => kitty::draw_png(path, pos, size).is_some(),
        Mode::Sixel => sixel::draw_png(path, pos, size),
        Mode::None => false,
    }
}

/// Fine del piazzamento, dopo il draw di ratatui. Kitty cancella per id le
/// immagini sparite. Sixel non ha id: riscrive le celle delle immagini sparite
/// o spostate che ratatui non ha toccato (serve il terminale per scrivere le
/// celle) e, solo se la misura delle celle è cambiata, fa clear e ridisegno.
pub fn frame_end(terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
    match mode() {
        Mode::Kitty => kitty::frame_end(),
        Mode::Sixel => sixel::frame_end(terminal),
        Mode::None => {}
    }
}

/// Dopo l'ultimo draw del frame: emette le immagini sixel che servono e
/// chiude l'aggiornamento sincronizzato. Su kitty non fa niente.
pub fn frame_flush() {
    if mode() == Mode::Sixel {
        sixel::frame_flush();
    }
}

/// Toglie ogni immagine all'uscita / nel panic restore.
pub fn clear_all() {
    match mode() {
        Mode::Kitty => kitty::clear_all(),
        Mode::Sixel => sixel::clear_all(),
        Mode::None => {}
    }
}
