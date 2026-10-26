//! Sixel rendering for stickers (foot e gli altri terminali sixel).
//!
//! Stesso schema di `kitty`, ma sixel non ha id né piazzamenti: l'immagine è
//! pixel scritti nelle celle, e restano lì finché qualcuno riscrive quelle
//! celle (su foot una cella scritta cancella solo i SUOI pixel, non tutta
//! l'immagine). Ratatui riscrive solo le celle che cambiano, quindi la pulizia
//! si decide confrontando il buffer disegnato con quello del frame prima:
//!
//! - immagine nuova, o con celle sotto riscritte da ratatui → si riemette;
//! - immagine sparita o spostata → le sue celle che ratatui NON ha riscritto
//!   si ridisegnano a mano (`frame_end`, dal buffer appena disegnato, via
//!   `Backend::draw`: stesse celle e stili che ratatui crede già a schermo, ma
//!   scrivendole tolgono i pixel). Niente clear dello schermo; le immagini
//!   ferme toccate da quella riscrittura si riemettono;
//! - immagine ferma e celle intatte → non si ritrasmette niente.
//!
//! Il clear completo resta solo per il cambio di misura della cella (font,
//! zoom): tutte le immagini hanno la scala vecchia.
//!
//! Le immagini si emettono SEMPRE dopo l'ultimo `terminal.draw`: scrivere le
//! celle di ratatui dopo cancellerebbe i pixel. La stringa sixel (PNG scalato
//! a `cols*cell_w x rows*cell_h`, quantizzato da icy_sixel) è codificata una
//! volta sola per (path, cols, rows, misura cella) e tenuta in cache, con
//! sfratto LRU. Il tutto va racchiuso in un aggiornamento sincronizzato
//! (DECSET 2026) per non mostrare stati intermedi.
//!
//! Le immagini che toccherebbero l'ultima riga dello schermo NON si emettono:
//! foot, finita l'immagine, manda il cursore a capo e scorrerebbe tutto
//! (DECSDM non aiuta: ancora l'immagine all'angolo 0,0).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use icy_sixel::{EncodeOptions, SixelImage};
use ratatui::backend::{Backend, ClearType, CrosstermBackend};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::Terminal;

/// oltre questo tetto la cache sfratta le stringhe meno usate di recente
const CACHE_MAX_BYTES: usize = 48 * 1024 * 1024;

const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";

/// un'immagine piazzata: path e rettangolo in celle
#[derive(Clone, PartialEq, Eq)]
struct Placed {
    path: String,
    x: u16,
    y: u16,
    cols: u32,
    rows: u32,
}

impl Placed {
    /// il rettangolo delle celle occupate, ritagliato sull'area del buffer
    fn rect(&self, area: Rect) -> Rect {
        Rect::new(
            self.x,
            self.y,
            self.cols.min(u16::MAX as u32) as u16,
            self.rows.min(u16::MAX as u32) as u16,
        )
        .intersection(area)
    }

    /// entra nello schermo senza toccare l'ultima riga? (vedi l'intestazione)
    fn fits(&self, area: Rect) -> bool {
        self.y as u32 + self.rows < area.height as u32
            && self.x as u32 + self.cols <= area.width as u32
    }
}

#[derive(Default)]
struct State {
    /// piazzati nel frame in corso
    cur: Vec<Placed>,
    /// piazzati nel frame precedente (già a schermo)
    prev: Vec<Placed>,
    /// buffer del frame in corso (copia di quello appena disegnato)
    cur_buf: Option<Buffer>,
    /// buffer del frame precedente: ciò che ratatui credeva a schermo
    prev_buf: Option<Buffer>,
    /// schermo ridisegnato da capo: vanno riemesse tutte
    redrawn: bool,
    /// cella cambiata di misura: al prossimo frame_end ridisegno completo
    force_clear: bool,
    /// rettangoli delle immagini sparite, riscritti a mano in questo frame:
    /// le immagini ferme che li toccano hanno perso dei pixel
    touched: Vec<Rect>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State::default()))
}

/// path, colonne, righe, pixel cella (w, h)
type CacheKey = (String, u32, u32, u32, u32);

#[derive(Default)]
struct Cache {
    /// stringa (`None` = codifica fallita: non si riprova a ogni frame) e
    /// "tick" dell'ultimo uso, per lo sfratto LRU
    map: HashMap<CacheKey, (Option<Arc<String>>, u64)>,
    bytes: usize,
    tick: u64,
}

impl Cache {
    fn get(&mut self, key: &CacheKey) -> Option<Option<Arc<String>>> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(key).map(|e| {
            e.1 = t;
            e.0.clone()
        })
    }

    fn insert(&mut self, key: CacheKey, val: Option<Arc<String>>) {
        self.tick += 1;
        self.bytes += val.as_ref().map_or(0, |s| s.len());
        self.map.insert(key, (val, self.tick));
        // sfratta le meno recenti, mai quella appena inserita
        while self.bytes > CACHE_MAX_BYTES && self.map.len() > 1 {
            let Some(old) = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.1)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some((v, _)) = self.map.remove(&old) {
                self.bytes -= v.as_ref().map_or(0, |s| s.len());
            }
        }
    }
}

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Cache::default()))
}

/// Cella cambiata di misura (font, zoom): le immagini a schermo hanno la
/// scala vecchia, al prossimo frame_end si ridisegna tutto.
pub fn invalidate() {
    if let Ok(mut st) = state().lock() {
        st.force_clear = true;
    }
}

fn write_out(s: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

pub fn sync_begin() {
    write_out(SYNC_BEGIN);
}

/// Copia il buffer appena disegnato da ratatui (dopo il draw la copia
/// "corrente" interna è già azzerata, quella disegnata è `CompletedFrame`).
pub fn observe_frame(buf: &Buffer) {
    if let Ok(mut st) = state().lock() {
        match st.cur_buf.as_mut() {
            Some(b) => b.clone_from(buf),
            None => st.cur_buf = Some(buf.clone()),
        }
    }
}

pub fn frame_begin() {
    if let Ok(mut st) = state().lock() {
        st.cur.clear();
    }
}

/// Registra il piazzamento; l'emissione è rimandata a `frame_flush`.
pub fn draw_png(path: &Path, (x, y): (u16, u16), (cols, rows): (u32, u32)) -> bool {
    if cols == 0 || rows == 0 || !path.is_file() {
        return false;
    }
    let Ok(mut st) = state().lock() else {
        return false;
    };
    st.cur.push(Placed {
        path: path.to_string_lossy().into_owned(),
        x,
        y,
        cols,
        rows,
    });
    true
}

/// Almeno una cella del rettangolo è stata riscritta da ratatui?
fn any_rewritten(r: Rect, prev: &Buffer, cur: &Buffer) -> bool {
    (r.y..r.y + r.height).any(|y| (r.x..r.x + r.width).any(|x| prev[(x, y)] != cur[(x, y)]))
}

/// Celle del rettangolo `r` (già dentro il buffer) che passano `keep`, in
/// ordine di riga. Salta le celle di coda dei caratteri larghi e quelle
/// `skip`, come fa il diff di ratatui: scriverle romperebbe il glifo.
fn cells_in(r: Rect, cur: &Buffer, keep: impl Fn(u16, u16) -> bool) -> Vec<(u16, u16)> {
    let mut v = Vec::new();
    for y in r.y..r.y + r.height {
        let mut tail = 0usize;
        for x in r.x..r.x + r.width {
            if tail > 0 {
                tail -= 1;
                continue;
            }
            let cell = &cur[(x, y)];
            tail = Span::raw(cell.symbol()).width().saturating_sub(1);
            if !cell.skip && keep(x, y) {
                v.push((x, y));
            }
        }
    }
    v
}

/// Riscrive le celle date con quanto dice il buffer (cella + stile). Le celle
/// sono quelle che ratatui crede già a schermo: l'effetto visibile è solo la
/// perdita dei pixel sixel che le coprivano.
fn repaint(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    cb: &Buffer,
    v: &[(u16, u16)],
) {
    if v.is_empty() {
        return;
    }
    let backend = terminal.backend_mut();
    let _ = backend.draw(v.iter().map(|&(x, y)| (x, y, &cb[(x, y)])));
    let _ = Backend::flush(backend);
}

/// Fine del piazzamento, dopo l'ultimo `terminal.draw`: toglie i pixel delle
/// immagini sparite o spostate riscrivendo le loro celle (vedi l'intestazione).
/// Non emette ancora nessuna immagine: lo fa `frame_flush`.
pub fn frame_end(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) {
    let Ok(mut st) = state().lock() else { return };
    let st = &mut *st;
    let Some(cb) = st.cur_buf.as_ref() else {
        return;
    };
    // quel che non entra nello schermo non si emette (e non si ricorda)
    st.cur.retain(|p| p.fits(cb.area));
    st.touched.clear();

    if std::mem::take(&mut st.force_clear) && !st.prev.is_empty() {
        // le immagini a schermo hanno la scala vecchia: clear completo e
        // ridisegno di tutte le celle significative dal buffer
        let _ = terminal.backend_mut().clear_region(ClearType::All);
        let all = cells_in(cb.area, cb, |x, y| cb[(x, y)] != Cell::default());
        repaint(terminal, cb, &all);
        st.redrawn = true;
        return;
    }

    // area diversa: ratatui ha già fatto da sé il clear del resize
    let Some(pb) = st.prev_buf.as_ref().filter(|pb| pb.area == cb.area) else {
        return;
    };
    let mut dirty: Vec<(u16, u16)> = Vec::new();
    for p in st.prev.iter().filter(|p| !st.cur.contains(p)) {
        let r = p.rect(cb.area);
        // le celle già riscritte da ratatui hanno perso i pixel da sole
        dirty.extend(cells_in(r, cb, |x, y| pb[(x, y)] == cb[(x, y)]));
        st.touched.push(r);
    }
    dirty.sort_unstable_by_key(|&(x, y)| (y, x));
    dirty.dedup();
    repaint(terminal, cb, &dirty);
}

/// Emette le immagini che servono e chiude l'aggiornamento sincronizzato.
/// Va chiamata dopo `frame_end`, a ogni iterazione: il buffer del frame
/// diventa il riferimento per il confronto del prossimo.
pub fn frame_flush() {
    let mut out = String::new();
    {
        let Ok(mut st) = state().lock() else { return };
        if let Some(cb) = st.cur_buf.take() {
            // schermo ridisegnato, primo frame o resize (ratatui ha pulito):
            // niente è affidabile, si riemette tutto
            let all = std::mem::take(&mut st.redrawn)
                || st.prev_buf.as_ref().is_none_or(|pb| pb.area != cb.area);
            for p in &st.cur {
                let r = p.rect(cb.area);
                let rewritten = || {
                    st.prev_buf
                        .as_ref()
                        .is_some_and(|pb| any_rewritten(r, pb, &cb))
                        || st.touched.iter().any(|t| t.intersects(r))
                };
                if all || !st.prev.contains(p) || rewritten() {
                    if let Some(data) = sixel_for(p) {
                        // MoveTo (1-based) + immagine
                        let _ = write!(out, "\x1b[{};{}H", p.y as u32 + 1, p.x as u32 + 1);
                        out.push_str(&data);
                    }
                }
            }
            st.prev = st.cur.clone();
            st.prev_buf = Some(cb);
            st.touched.clear();
        }
    }
    out.push_str(SYNC_END);
    write_out(&out);
}

/// Stringa sixel per l'immagine piazzata, dalla cache o codificata ora.
fn sixel_for(p: &Placed) -> Option<Arc<String>> {
    let (cw, ch) = crate::gfx::cells();
    let (cw, ch) = (cw.round() as u32, ch.round() as u32);
    let key: CacheKey = (p.path.clone(), p.cols, p.rows, cw, ch);
    let mut c = cache().lock().ok()?;
    if let Some(hit) = c.get(&key) {
        return hit;
    }
    let enc = encode(Path::new(&p.path), p.cols * cw, p.rows * ch).map(Arc::new);
    c.insert(key, enc.clone());
    enc
}

/// Legge il PNG (APNG: solo il primo frame), lo scala per stare nel riquadro
/// `box_w x box_h` pixel mantenendo le proporzioni, lo centra su una tela
/// trasparente (alpha < 128 = pixel non disegnato, lo sfondo resta) e lo
/// codifica in sixel.
fn encode(path: &Path, box_w: u32, box_h: u32) -> Option<String> {
    if box_w == 0 || box_h == 0 {
        return None;
    }
    let src = image::open(path).ok()?.to_rgba8();
    let (iw, ih) = src.dimensions();
    if iw == 0 || ih == 0 {
        return None;
    }
    let scale = (box_w as f32 / iw as f32).min(box_h as f32 / ih as f32);
    let nw = ((iw as f32 * scale).round() as u32).clamp(1, box_w);
    let nh = ((ih as f32 * scale).round() as u32).clamp(1, box_h);
    let small = image::imageops::resize(&src, nw, nh, image::imageops::FilterType::Lanczos3);

    let (bw, bh) = (box_w as usize, box_h as usize);
    let mut canvas = vec![0u8; bw * bh * 4];
    let (ox, oy) = ((bw - nw as usize) / 2, (bh - nh as usize) / 2);
    for (row, line) in small.as_raw().chunks_exact(nw as usize * 4).enumerate() {
        let start = ((oy + row) * bw + ox) * 4;
        canvas[start..start + line.len()].copy_from_slice(line);
    }
    SixelImage::from_rgba(canvas, bw, bh)
        .encode_with(&EncodeOptions::default())
        .ok()
}

/// All'uscita non c'è niente da cancellare: si lascia lo schermo alternativo
/// e con lui spariscono i pixel. Si azzera lo stato e si chiude un eventuale
/// aggiornamento sincronizzato rimasto aperto (panic a metà frame).
pub fn clear_all() {
    write_out(SYNC_END);
    if let Ok(mut st) = state().lock() {
        *st = State::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PNG 16x16 con un quadrato rosso opaco su sfondo trasparente
    fn write_test_png(path: &Path) {
        let mut img = image::RgbaImage::new(16, 16);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = if (4..12).contains(&x) && (4..12).contains(&y) {
                image::Rgba([220, 20, 20, 255])
            } else {
                image::Rgba([0, 0, 0, 0])
            };
        }
        img.save(path).unwrap();
    }

    #[test]
    fn codifica_png_in_sixel() {
        let path =
            std::env::temp_dir().join(format!("rondine-sixel-test-{}.png", std::process::id()));
        write_test_png(&path);
        // riquadro 5x3 celle da 10x20 px, immagine quadrata: va centrata
        let s = encode(&path, 50, 60).expect("codifica sixel");
        let _ = std::fs::remove_file(&path);
        assert!(s.starts_with("\x1bP"), "manca il DCS iniziale");
        assert!(s.ends_with("\x1b\\"), "manca lo String Terminator");
        assert!(
            s.contains("\"1;1;50;60"),
            "attributi raster = misura del riquadro"
        );
    }

    #[test]
    fn file_inesistente_non_si_codifica() {
        assert!(encode(Path::new("/non/esiste.png"), 50, 60).is_none());
    }

    #[test]
    fn celle_riscritte_e_da_riscrivere() {
        let area = Rect::new(0, 0, 6, 4);
        let prev = Buffer::empty(area);
        let mut cur = prev.clone();
        let r = Rect::new(1, 1, 2, 2);
        assert!(!any_rewritten(r, &prev, &cur));
        cur[(1, 1)] = Cell::new("x");
        assert!(any_rewritten(r, &prev, &cur));
        // delle 4 celle una l'ha già riscritta ratatui: ne restano 3 da riscrivere
        let v = cells_in(r, &cur, |x, y| prev[(x, y)] == cur[(x, y)]);
        assert_eq!(v, vec![(2, 1), (1, 2), (2, 2)]);
    }

    #[test]
    fn caratteri_larghi_non_si_spezzano() {
        let mut b = Buffer::empty(Rect::new(0, 0, 4, 1));
        b.set_string(0, 0, "日a", ratatui::style::Style::default());
        // la coda della cella larga (x=1) non va riscritta
        assert_eq!(
            cells_in(b.area, &b, |_, _| true),
            vec![(0, 0), (2, 0), (3, 0)]
        );
    }

    #[test]
    fn immagine_sull_ultima_riga_non_entra() {
        let area = Rect::new(0, 0, 80, 24);
        let p = |y, rows| Placed {
            path: String::new(),
            x: 0,
            y,
            cols: 4,
            rows,
        };
        assert!(p(10, 3).fits(area));
        assert!(p(20, 3).fits(area)); // finisce alla riga 22 (l'ultima è la 23)
        assert!(!p(21, 3).fits(area)); // toccherebbe la riga 23
    }

    #[test]
    fn cache_lru_sfratta_il_meno_recente() {
        let mut c = Cache::default();
        let big = |n: usize| Some(Arc::new("x".repeat(n)));
        let k = |n: &str| (n.to_string(), 1, 1, 10, 20);
        c.insert(k("a"), big(CACHE_MAX_BYTES / 2));
        c.insert(k("b"), big(CACHE_MAX_BYTES / 2));
        assert!(c.get(&k("a")).is_some()); // "a" torna recente
        c.insert(k("c"), big(CACHE_MAX_BYTES / 2));
        assert!(c.get(&k("b")).is_none(), "b era la meno recente");
        assert!(c.get(&k("a")).is_some() && c.get(&k("c")).is_some());
    }
}
