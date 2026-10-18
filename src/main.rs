//! RTete — client Telegram TUI per Linux, pensato per consumare pochissima RAM:
//! grammers (MTProto puro-Rust, niente TDLib), ratatui per l'interfaccia e il
//! kitty graphics protocol per mostrare gli sticker.

use std::collections::{HashMap, HashSet, VecDeque};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use futures_util::StreamExt;
use grammers_client::media::Media;
use grammers_client::session::types::{PeerId, PeerKind};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

mod kitty;
mod loader;
mod styles;
mod tg;
mod view;

/// Messaggi tenuti in RAM per la chat aperta (guardia memoria).
pub const MAX_HISTORY: usize = 60;
/// Colonne della griglia sticker nel pannello.
pub const STICKER_COLS: usize = 4;

type PeerRefT = grammers_client::session::types::PeerRef;

/// Dopo `prev` fallimenti, il prossimo tentativo tra 1, 2, 4… fino a 60s:
/// backoff esponenziale unico per conversioni e richieste di rete.
fn backoff_secs(prev: u32) -> Duration {
    let secs = 1u64 << prev.min(6); // u64: nessun overflow
    Duration::from_secs(secs.min(60))
}

fn row_from_msg(m: grammers_client::message::Message) -> Result<(PeerId, tg::HistoryRow), String> {
    let peer_id = m.peer_id();
    let row = tg::HistoryRow {
        id: m.id(),
        outgoing: true,
        from: tg::sender_name(&m),
        kind: tg::row_kind(m.media(), m.text().to_string()),
        reactions: tg::collect_reactions(&m.raw),
    };
    Ok((peer_id, row))
}

/// Comando grafico da eseguire dopo il frame ratatui.
pub enum Cmd {
    Place {
        rect: ratatui::layout::Rect,
        path: PathBuf,
    },
}

pub struct App {
    pub contacts: Vec<tg::DialogInfo>,
    pub contact_cursor: usize,
    pub open: Option<usize>,
    pub histories: HashMap<PeerId, VecDeque<tg::HistoryRow>>,
    pub compose: String,
    pub status: String,
    /// true: pannello sticker visibile.
    pub show_sticker: bool,
    /// false: lista set, true: dentro un set.
    pub set_level: bool,
    /// cursore nella lista dei set.
    pub set_cursor: usize,
    /// cursore nella griglia del set aperto.
    pub sticker_cursor: usize,
    /// None: lista in caricamento, Some: set installati.
    pub sets: Option<Vec<tg::StickerSetInfo>>,
    /// set_id -> sticker del set (scaricati on-demand).
    pub set_docs: HashMap<i64, Vec<Media>>,
    /// set il cui download è in pausa di backoff: set_id → (riprova da, n).
    pub docs_failed: HashMap<i64, (Instant, u32)>,
    /// id del set aperto.
    pub open_set: Option<i64>,
    /// media_key -> png convertito.
    pub previews: HashMap<String, PathBuf>,
    /// conversioni in volo.
    pub pending: HashSet<String>,
    /// asset falliti: chiave → (prossimo tentativo, quante volte). Il
    /// backoff evita il loop infinito download/converti di
    /// refresh_preview_queue su un asset irrimediabile.
    pub failed: HashMap<String, (Instant, u32)>,
    /// il nome dell'utente connesso (etichette "Nome │ testo").
    pub me: String,
    /// messaggio selezionato con ↑/↓ nella chat (id); None = vista "live".
    pub msg_cursor: Option<i32>,
    /// id del messaggio che Ctrl-D ha scelto da cancellare (conferma Invio).
    pub del_id: Option<i32>,
    /// id del messaggio che stiamo modificando (Ctrl-E).
    pub editing: Option<i32>,
    /// eliminazione armata da Ctrl-D: il prossimo Invio la conferma.
    pub del_arm: bool,
    /// ricerca nella chat aperta (Ctrl-F).
    pub show_search: bool,
    pub search_q: String,
    pub search_cursor: usize,
    /// None: ricerca ancora da lanciare; Some: risultati già ricevuti.
    pub search_results: Option<Vec<tg::SearchHit>>,
    /// righe (y schermo, id messaggio) dell'ultimo frame: click → selezione.
    pub mouse_map: Vec<(u16, i32)>,
    /// link a schermo: (riga, x inizio, x fine, url) per il click → browser.
    pub link_map: Vec<(u16, u16, u16, String)>,
    /// foto dagli appunti pronta da inviare (percorso del file in tmp).
    pub pending_photo: Option<String>,
    /// pannello reazioni veloci aperto sul messaggio selezionato.
    pub react_open: bool,
    /// indice nella lista tg::QUICK_REACTIONS.
    pub react_idx: usize,
    /// ultimo mark_as_read mandato: soglia anti-tempesta sui burst di
    /// messaggi nuovi nella chat aperta.
    pub read_mark: Option<Instant>,
    pub quit: bool,
}

impl App {
    pub fn current_chat(&self) -> Option<&tg::DialogInfo> {
        self.open.and_then(|i| self.contacts.get(i))
    }

    pub fn open_history(&self) -> Option<&VecDeque<tg::HistoryRow>> {
        let chat = self.current_chat()?;
        self.histories.get(&chat.peer_id)
    }

    fn push_row(&mut self, peer_id: PeerId, row: tg::HistoryRow) {
        let ring = self.histories.entry(peer_id).or_default();
        // dedup: uno stesso messaggio arriva sia dalla conferma dell'invio
        // sia dalle notifiche live di Telegram (anche dai nostri stessi invii)
        if ring.iter().any(|r| r.id == row.id) {
            return;
        }
        ring.push_back(row);
        while ring.len() > MAX_HISTORY {
            ring.pop_front();
        }
    }

    fn push_rows(&mut self, peer_id: PeerId, rows: Vec<tg::HistoryRow>) {
        let ring = self.histories.entry(peer_id).or_default();
        for row in rows {
            if ring.iter().any(|r| r.id == row.id) {
                continue;
            }
            ring.push_back(row);
        }
        while ring.len() > MAX_HISTORY {
            ring.pop_front();
        }
    }

    /// Sostituisce (o inserisce in coda) una riga stessa-id: usato per gli
    /// edit e per il feedback locali.
    fn replace_row(&mut self, peer_id: PeerId, row: tg::HistoryRow) {
        let ring = self.histories.entry(peer_id).or_default();
        if let Some(old) = ring.iter_mut().find(|r| r.id == row.id) {
            *old = row;
        } else {
            ring.push_back(row);
            while ring.len() > MAX_HISTORY {
                ring.pop_front();
            }
        }
    }

    /// Sostituisce (o applica) le reazioni di un messaggio: update server
    /// o toggle locale ottimistico.
    fn apply_reactions(&mut self, peer_id: PeerId, msg_id: i32, items: Vec<tg::ReactionItem>) {
        let Some(ring) = self.histories.get_mut(&peer_id) else {
            return;
        };
        if let Some(row) = ring.iter_mut().find(|r| r.id == msg_id) {
            row.reactions = items;
        }
    }

    /// Toggle locale immediato di una reazione sull'ultimo cursore (il
    /// server conferma poi con l'update vero e proprio).
    fn optimistic_react(&mut self, emoticon: &str) {
        let Some(chat) = self.current_chat() else {
            return;
        };
        let (pid, mid) = (chat.peer_id, self.msg_cursor);
        let Some(mid) = mid else {
            return;
        };
        let Some(ring) = self.histories.get_mut(&pid) else {
            return;
        };
        let Some(row) = ring.iter_mut().find(|r| r.id == mid) else {
            return;
        };
        // Telegram senza Premium ammette una sola reazione per messaggio:
        // se il mio voto sta già su questa emoji la tolgo; se sta su
        // un'altra cambia bersaglio; il mio voto non resta mai su due item
        if let Some(i) = row
            .reactions
            .iter()
            .position(|r| r.mine && r.emoticon == emoticon)
        {
            // il mio voto cala; l'item passa solo se resta a zero
            // (prima il retain toglieva l'intera voce anche se altri hanno
            // messo la stessa reazione)
            let entry = &mut row.reactions[i];
            entry.count = entry.count.saturating_sub(1);
            if entry.count <= 0 {
                row.reactions.remove(i);
            }
            return;
        }
        // cambio bersaglio: il mio voto vecchio cala (item via se resta a zero)
        if let Some(i) = row
            .reactions
            .iter()
            .position(|r| r.mine && r.emoticon != emoticon)
        {
            let entry = &mut row.reactions[i];
            entry.count = entry.count.saturating_sub(1);
            if entry.count <= 0 {
                row.reactions.remove(i);
            }
        }
        match row.reactions.iter_mut().find(|r| r.emoticon == emoticon) {
            Some(item) => {
                item.count += 1;
                item.mine = true;
            }
            None => row.reactions.push(tg::ReactionItem {
                emoticon: emoticon.to_string(),
                count: 1,
                mine: true,
            }),
        }
    }

    /// Coda le conversioni degli sticker visibili non in cache. Un asset
    /// appena fallito non parte di nuovo finché il backoff non scade.
    fn ensure_previews<'a>(
        &mut self,
        conv_tx: &mpsc::UnboundedSender<Media>,
        medias: impl IntoIterator<Item = &'a Media>,
    ) {
        for media in medias {
            let key = tg::media_key(media);
            if self.previews.contains_key(&key) || self.pending.contains(&key) {
                continue;
            }
            if let Some(&(until, _)) = self.failed.get(&key) {
                if Instant::now() < until {
                    continue;
                }
            }
            if self.pending.insert(key) {
                let _ = conv_tx.send(media.clone());
            }
        }
    }

    /// Registra un asset fallito e fissa il prossimo tentativo col backoff
    /// esponenziale: 1s, 2s, 4s… fino a 60s. refresh_preview_queue gira a
    /// ogni cambio di layout: senza questo un singolo download guasto (rete
    /// assente, webp corrotto) verrebbe rilanciato all'infinito.
    fn conversion_failed(&mut self, key: String) {
        let prev = self.failed.remove(&key).map(|(_, n)| n).unwrap_or(0);
        self.failed
            .insert(key, (Instant::now() + backoff_secs(prev), prev + 1));
    }

    /// idem per i download di set: senza pausa, ogni freccia sul set
    /// rilanciava la richiesta e un set guasto martellava la rete; un
    /// errore non deve neanche marca il set come "caricato" (vuoto eterno)
    fn docs_cooling(&self, set_id: i64) -> bool {
        self.docs_failed
            .get(&set_id)
            .map(|&(until, _)| Instant::now() < until)
            .unwrap_or(false)
    }

    fn docs_failed(&mut self, set_id: i64) {
        let prev = self.docs_failed.remove(&set_id).map(|(_, n)| n).unwrap_or(0);
        self.docs_failed
            .insert(set_id, (Instant::now() + backoff_secs(prev), prev + 1));
    }

    fn docs_fine(&mut self, set_id: i64) {
        self.docs_failed.remove(&set_id);
    }

    /// true una volta ogni READ_MARK_GAP (o alla prima richiesta): limita i
    /// mark_as_read quando nella chat aperta arrivano raffiche di messaggi.
    fn read_mark_due(&mut self) -> bool {
        const READ_MARK_GAP: Duration = Duration::from_secs(2);
        let now = Instant::now();
        let due = self
            .read_mark
            .map(|t| now.duration_since(t) >= READ_MARK_GAP)
            .unwrap_or(true);
        if due {
            self.read_mark = Some(now);
        }
        due
    }
}

struct SendJob {
    peer: PeerRefT,
    peer_id: PeerId,
    kind: SendKind,
}

/// esito: nuovo messaggio o modifica confermata
enum SendDone {
    New(PeerId, tg::HistoryRow),
    Edit(PeerId, tg::HistoryRow),
}

// I payload servono interi sui canali mpsc: il boxing aggiungerebbe solo
// allocazioni, la dimensione dell'enum non è un collo di bottiglia.
#[allow(clippy::large_enum_variant)]
enum SendKind {
    Text(String),
    Sticker(Media),
    /// foto dalla clipboard (file già salvato in tmp) con eventuale caption.
    Photo { path: String, caption: String },
    /// modifica di un testo già inviato (id invariato).
    EditText {
        id: i32,
        text: String,
    },
}

#[allow(clippy::too_many_arguments)]
async fn handle_event(
    app: &mut App,
    ev: Event,
    hist_tx: &mpsc::UnboundedSender<(PeerRefT, PeerId)>,
    send_tx: &mpsc::UnboundedSender<SendJob>,
    sets_tx: &mpsc::UnboundedSender<()>,
    set_req_tx: &mpsc::UnboundedSender<(i64, String)>,
    conv_tx: &mpsc::UnboundedSender<Media>,
    del_tx: &mpsc::UnboundedSender<(PeerRefT, PeerId, i32)>,
    search_tx: &mpsc::UnboundedSender<(PeerRefT, String)>,
    save_tx: &mpsc::UnboundedSender<(Media, String)>,
    play_tx: &mpsc::UnboundedSender<(Media, String, String)>,
    react_tx: &mpsc::UnboundedSender<(PeerRefT, PeerId, i32, Option<String>)>,
) {
    match ev {
        Event::Resize(_, _) => {}
        Event::Key(k) => {
            if k.kind == KeyEventKind::Release {
                return;
            }
            // cmd+c/cmd+v: col protocollo tastiera kitty SUPER arriva pulito;
            // nei terminali legacy un super degrada a ALT → medesima azione.
            // AltGr (CTRL|ALT) resta digitazione con accentate: non intercetto.
            let alt_only = k.modifiers.contains(KeyModifiers::ALT)
                && !k.modifiers.contains(KeyModifiers::CONTROL);
            if k.modifiers.contains(KeyModifiers::SUPER) || alt_only {
                if let KeyCode::Char(c) = k.code {
                    match c {
                        'c' => {
                            ctrl_copy(app);
                            return;
                        }
                        'v' => {
                            ctrl_paste(app);
                            return;
                        }
                        _ => {} // altre lettere col super: al flusso normale
                    }
                } else {
                    return;
                }
            }
            // combo Ctrl: non toccano il testo composto
            if k.modifiers.contains(KeyModifiers::CONTROL) {
                if let KeyCode::Char(c) = k.code {
                    match c {
                        // ctrl/cmd+C: copia il messaggio selezionato; senza
                        // selezione chiude (copy_or_interrupt, come in kitty)
                        'c' => {
                            if app.msg_cursor.is_some() {
                                ctrl_copy(app)
                            } else {
                                app.quit = true
                            }
                        }
                        // ctrl/cmd+V: incolla dagli appunti (via wl-paste)
                        'v' => ctrl_paste(app),
                        // modifica l'ultimo tuo messaggio di testo
                        'e' => ctrl_edit(app),
                        // arma la cancellazione dell'ultimo tuo messaggio
                        'd' => ctrl_delete(app),
                        // salva l'ultimo documento ricevuto in ~/Downloads
                        'o' => ctrl_save(app, save_tx),
                        // riproduce l'ultimo audio (vocale o brano)
                        'p' => ctrl_play(app, play_tx),
                        // copia il messaggio selezionato negli appunti
                        'y' => ctrl_copy(app),
                        // reazioni veloci sul messaggio selezionato
                        'r' => {
                            app.react_idx = 0;
                            app.react_open = !app.react_open;
                            app.status = if app.react_open {
                                "reazioni: ←/→ o 1..8 · Invio toggle · Esc chiude".into()
                            } else {
                                String::new()
                            };
                        }
                        // ricerca nella chat aperta
                        'f' if app.current_chat().is_some() => {
                            app.show_search = true;
                            app.show_sticker = false;
                            app.search_results = None;
                            app.search_cursor = 0;
                            app.search_q.clear();
                            app.del_arm = false;
                        }
                        _ => {}
                    }
                }
                return;
            }
            match k.code {
                KeyCode::Char('q')
                    if app.open.is_none()
                        && !app.show_sticker
                        && !app.show_search
                        && app.compose.is_empty() =>
                {
                    app.quit = true
                }
                // verticale: frecce sempre; j/k solo dove non si scrive
                // (pannello sticker o lista chat senza messaggio aperto)
                KeyCode::Up => {
                    if app.show_search {
                        move_search(app, false)
                    } else {
                        move_up(app)
                    }
                }
                KeyCode::Down => {
                    if app.show_search {
                        move_search(app, true)
                    } else {
                        move_down(app)
                    }
                }
                KeyCode::Char('j') if app.show_sticker || app.open.is_none() => move_down(app),
                KeyCode::Char('k') if app.show_sticker || app.open.is_none() => move_up(app),
                KeyCode::Esc => {
                    if app.show_search {
                        app.show_search = false;
                        app.search_results = None;
                        app.search_q.clear();
                    } else if app.show_sticker {
                        if app.set_level {
                            app.set_level = false;
                            app.sticker_cursor = 0;
                        } else {
                            app.show_sticker = false;
                            app.open_set = None;
                        }
                    } else if app.react_open {
                        app.status = "reazioni: chiuse".into();
                        app.react_open = false;
                    } else if app.pending_photo.is_some() {
                        app.status = "appunti: foto scartata".into();
                        if let Some(p) = app.pending_photo.take() {
                            let _ = std::fs::remove_file(p);
                        }
                        app.compose.clear();
                    } else if app.editing.is_some() {
                        app.status = "modifica annullata".into();
                        app.editing = None;
                        app.compose.clear();
                    } else if app.del_arm {
                        app.status = "cancellazione annullata".into();
                        app.del_arm = false;
                        app.del_id = None;
                    } else if app.msg_cursor.is_some() {
                        app.status = "selezione chiusa".into();
                        app.msg_cursor = None;
                    } else {
                        // chiude la chat e torna alla lista contatti
                        app.status.clear();
                        app.open = None;
                        app.search_results = None;
                        app.msg_cursor = None;
                        app.compose.clear();
                    }
                }
                // orizzontale: solo nel pannello sticker (→ = l, ← = h)
                // nel pannello reazioni: ←/→ muovono la scelta
                KeyCode::Right if app.react_open => {
                    let max = tg::QUICK_REACTIONS.len() - 1;
                    app.react_idx = (app.react_idx + 1).min(max);
                }
                KeyCode::Left if app.react_open => {
                    app.react_idx = app.react_idx.saturating_sub(1);
                }
                KeyCode::Left if app.show_sticker => grid_left(app),
                KeyCode::Right if app.show_sticker => grid_right(app, set_req_tx),
                KeyCode::Char('h') if app.show_sticker => grid_left(app),
                KeyCode::Char('l') if app.show_sticker => grid_right(app, set_req_tx),
                // Tab apre/chiude il pannello sticker: 's' torna una lettera
                KeyCode::Tab if !app.show_search && !app.del_arm => {
                    app.editing = None;
                    app.del_arm = false;
                    app.del_id = None;
                    app.react_open = false;
                    if let Some(p) = app.pending_photo.take() {
                        let _ = std::fs::remove_file(p);
                    }
                    app.show_sticker = !app.show_sticker;
                    app.set_level = false;
                    if app.show_sticker {
                        app.sticker_cursor = 0;
                        if app.sets.is_none() {
                            app.status = "scarico i set di sticker…".into();
                            let _ = sets_tx.send(());
                        }
                    }
                }
                KeyCode::Enter => {
                    on_enter(
                        app, hist_tx, send_tx, set_req_tx, conv_tx, del_tx, search_tx,
                        react_tx,
                    )
                    .await
                }
                KeyCode::Backspace => {
                    app.del_arm = false;
                    app.del_id = None;
                    if app.show_search {
                        app.search_q.pop();
                    } else {
                        app.compose.pop();
                    }
                }
                KeyCode::Char(c) if app.react_open && c.is_ascii_digit() => {
                    let n = c as usize - '1' as usize + 1;
                    if n >= 1 && n <= tg::QUICK_REACTIONS.len() {
                        app.react_idx = n - 1;
                        app.status = format!("reazioni: {} (Invio per applicare)", tg::QUICK_REACTIONS[n - 1]);
                    }
                }
                KeyCode::Char(c) if !app.show_sticker && !app.react_open => {
                    app.del_arm = false;
                    app.del_id = None;
                    if app.show_search {
                        app.search_q.push(c);
                    } else {
                        app.compose.push(c);
                    }
                }
                _ => {}
            }
        }
        Event::Mouse(m) => {
            use crossterm::event::{MouseButton, MouseEventKind};
            match m.kind {
                MouseEventKind::ScrollUp => key_updown_mouse(app, m, false),
                MouseEventKind::ScrollDown => key_updown_mouse(app, m, true),
                MouseEventKind::Down(MouseButton::Left) => click_select(app, m),
                _ => {}
            }
        }
        Event::Paste(text) => {
            app.del_arm = false;
            app.del_id = None;
            if app.show_search {
                let clean = paste_text(&text, false, 200);
                app.search_q.push_str(&clean);
            } else if app.open.is_some() {
                let cap = 4000usize.saturating_sub(app.compose.chars().count());
                let clean = paste_text(&text, true, cap.min(4000));
                app.compose.push_str(&clean);
            }
        }
        _ => {}
    }
}

/// Ruota/trackpad: nel pannello ricerca scorre i risultati; nella chat,
/// a sinistra scorre i contatti, a destra usa il cursore di selezione;
/// nel pannello sticker naviga il grigliato come le frecce.
fn key_updown_mouse(app: &mut App, m: crossterm::event::MouseEvent, down: bool) {
    if app.show_search {
        move_search(app, down);
        return;
    }
    if app.show_sticker {
        if down {
            move_down(app);
        } else {
            move_up(app);
        }
        return;
    }
    // nella vista chat: la colonna dei contatti è a sinistra
    let term_w = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80);
    let side = view::side_w(term_w);
    if m.column < side {
        if down {
            move_down(app);
        } else {
            move_up(app);
        }
    } else {
        move_msg_cursor(app, down);
    }
}

/// Prepara la modifica dell'ultimo messaggio di testo scritto da noi.
fn ctrl_edit(app: &mut App) {
    app.status.clear();
    if app.show_search || app.show_sticker {
        return;
    }
    // l'edit riprende il compose: scarta un'eventuale foto pendente
    if let Some(p) = app.pending_photo.take() {
        let _ = std::fs::remove_file(p);
    }
    let Some(chat) = app.current_chat() else {
        app.status = "apri prima una chat".into();
        return;
    };
    let ring = app.histories.get(&chat.peer_id);
    let Some(ring) = ring else {
        app.status = "cronologia non ancora caricata".into();
        return;
    };
    // se c'è un cursore e punta a un tuo messaggio di testo, usa quello
    let chosen = app.msg_cursor.and_then(|cid| {
        ring.iter()
            .rev()
            .find(|r| r.id == cid && r.outgoing && matches!(r.kind, tg::RowKind::Text(_)))
    });
    let found = match chosen {
        Some(r) => Some((
            r.id,
            match &r.kind {
                tg::RowKind::Text(t) => t.clone(),
                _ => String::new(),
            },
        )),
        None => ring.iter().rev().find_map(|r| match &r.kind {
            tg::RowKind::Text(text) if r.outgoing => Some(r.id).map(|id| (id, text.clone())),
            _ => None,
        }),
    };
    match found {
        Some((id, text)) => {
            app.editing = Some(id);
            app.del_arm = false;
            app.compose = text;
            app.status = format!("✎ sto modificando il messaggio #{id} — Invio per confermare");
        }
        None => app.status = "nessun tuo messaggio di testo da modificare".into(),
    }
}

/// Arma la cancellazione dell'ultimo tuo messaggio (conferma con Invio).
fn ctrl_delete(app: &mut App) {
    app.status.clear();
    if app.show_search || app.show_sticker {
        return;
    }
    let Some(chat) = app.current_chat() else {
        app.status = "apri prima una chat".into();
        return;
    };
    let Some(ring) = app.histories.get(&chat.peer_id) else {
        app.status = "cronologia non ancora caricata".into();
        return;
    };
    // cursore attivo? cancella QUEL messaggio (solo se tuo: outgoing)
    let target = match app
        .msg_cursor
        .and_then(|cid| ring.iter().find(|r| r.id == cid))
    {
        Some(r) if r.outgoing => Some(r.id),
        Some(_) => {
            app.status = "puoi cancellare solo messaggi tuoi".into();
            return;
        }
        None => ring.iter().rev().find(|r| r.outgoing).map(|r| r.id),
    };
    let Some(last_own) = target else {
        app.status = "nessun tuo messaggio da cancellare".into();
        return;
    };
    app.del_arm = true;
    app.del_id = Some(last_own);
    app.editing = None;
    app.status = format!("⚠️ Invio = cancella il tuo messaggio #{last_own} (Esc per annullare)");
}

/// Riproduce l'ultimo audio (vocale o brano) della chat aperta.
fn ctrl_play(app: &mut App, play_tx: &mpsc::UnboundedSender<(Media, String, String)>) {
    app.status.clear();
    if app.show_search || app.show_sticker {
        return;
    }
    let Some(chat) = app.current_chat() else {
        app.status = "apri prima una chat".into();
        return;
    };
    let Some(ring) = app.histories.get(&chat.peer_id) else {
        app.status = "cronologia non ancora caricata".into();
        return;
    };
    // cursore attivo? riproduci QUEL audio; altrimenti l'ultimo
    let chosen = app.msg_cursor.and_then(|cid| {
        ring.iter()
            .rev()
            .find(|r| r.id == cid && matches!(r.kind, tg::RowKind::Audio { .. }))
    });
    let to_job = |r: &tg::HistoryRow| match &r.kind {
        tg::RowKind::Audio {
            media, name, voice, ..
        } => {
            let label = if *voice {
                "voce".to_string()
            } else {
                name.clone()
            };
            // i vocali salvarli sempre ogg; i brani col nome così com'è
            let fname = if *voice {
                format!("voce-{}.ogg", tg::media_key(media))
            } else {
                name.replace('/', "_").replace("..", "_")
            };
            Some((media.clone(), fname, label))
        }
        _ => None,
    };
    let found = chosen
        .and_then(to_job)
        .or_else(|| ring.iter().rev().find_map(to_job));
    match found {
        Some((media, fname, label)) => {
            app.status = format!("scarico «{label}» e la riproduco…");
            let _ = play_tx.send((media, fname, label));
        }
        None => app.status = "nessun audio nella cronologia caricata".into(),
    }
}

/// Salva su disco l'ultimo documento (o foto) della chat aperta.
fn ctrl_save(app: &mut App, save_tx: &mpsc::UnboundedSender<(Media, String)>) {
    app.status.clear();
    if app.show_search || app.show_sticker {
        return;
    }
    let Some(chat) = app.current_chat() else {
        app.status = "apri prima una chat".into();
        return;
    };
    let ring = app.histories.get(&chat.peer_id);
    let Some(ring) = ring else {
        app.status = "cronologia non ancora caricata".into();
        return;
    };
    // cursore attivo? salva QUEL documento; altrimenti l'ultimo della cronologia
    let chosen = app.msg_cursor.and_then(|cid| {
        ring.iter()
            .rev()
            .find(|r| r.id == cid && matches!(r.kind, tg::RowKind::Doc { .. }))
    });
    let found = match chosen {
        Some(r) => match &r.kind {
            tg::RowKind::Doc { media, name, .. } => Some((media.clone(), name.clone())),
            _ => None,
        },
        None => ring.iter().rev().find_map(|r| match &r.kind {
            tg::RowKind::Doc { media, name, .. } => Some((media.clone(), name.clone())),
            _ => None,
        }),
    };
    match found {
        Some((media, name)) => {
            let safe = name.replace('/', "_").replace("..", "_");
            app.status = format!("salvo «{safe}» in ~/Downloads…");
            let _ = save_tx.send((media, safe));
        }
        None => app.status = "nessun documento da salvare nella cronologia caricata".into(),
    }
}

/// Click sinistro su una riga del pannello messaggi: seleziona quel
/// messaggio (stessa cosa di ↑/↓).
fn click_select(app: &mut App, m: crossterm::event::MouseEvent) {
    if app.show_search || app.show_sticker || app.open.is_none() {
        return;
    }
    let term_w = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80);
    if m.column < view::side_w(term_w) {
        return; // colonna contatti: lì non seleziono messaggi
    }
    // click su un link? apri il browser di default (xdg-open) e basta
    if let Some((_, _, _, url)) = app
        .link_map
        .iter()
        .find(|(y, x0, x1, _)| *y == m.row && m.column >= *x0 && m.column < *x1)
    {
        let url = url.clone();
        app.status = format!("apro {url}…");
        std::thread::spawn(move || {
            use std::process::{Command, Stdio};
            let _ = Command::new("xdg-open")
                .arg(&url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        });
        return;
    }
    let hit = app
        .mouse_map
        .iter()
        .find(|(y, _)| *y == m.row)
        .map(|(_, id)| *id);
    match hit {
        Some(id) if id != 0 => {
            app.msg_cursor = Some(id);
            app.status = format!("selezionato #{id} — Esc esce");
        }
        _ => {}
    }
}

/// Testo "copiabile" di una riga: testo, caption (foto/doc/audio), emoji
/// degli sticker, note di servizio. None se la riga non ha testo.
fn copy_text(r: &tg::HistoryRow) -> Option<String> {
    match &r.kind {
        tg::RowKind::Text(t) => Some(t.clone()),
        tg::RowKind::Note(t) | tg::RowKind::Service(t) => {
            let t = t.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        tg::RowKind::Sticker { emoji, .. } => {
            let t = emoji.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        tg::RowKind::Photo { caption, .. } => {
            let t = caption.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        tg::RowKind::Doc { name, caption, .. } => {
            let t = pick_caption(caption, name);
            (!t.is_empty()).then(|| t.to_string())
        }
        tg::RowKind::Audio { name, caption, .. } => {
            let t = pick_caption(caption, name);
            (!t.is_empty()).then(|| t.to_string())
        }
    }
}

/// preferisce la caption; in mancanza il nome (per gli allegati).
fn pick_caption<'a>(caption: &'a str, name: &'a str) -> &'a str {
    let c = caption.trim();
    if c.is_empty() {
        name.trim()
    } else {
        c
    }
}

/// Super/Ctrl-Y: copia negli appunti il messaggio selezionato (o senza
/// selezione, l'ultimo messaggio con testo della chat). wl-copy si serve
/// da sé in background: nessuna libreria in-process.
fn ctrl_copy(app: &mut App) {
    app.status.clear();
    if app.show_search || app.show_sticker {
        return;
    }
    let Some(chat) = app.current_chat() else {
        app.status = "apri prima una chat".into();
        return;
    };
    let ring = app.histories.get(&chat.peer_id);
    let picked = match (ring, app.msg_cursor) {
        (Some(ring), Some(id)) => ring.iter().rev().find(|r| r.id == id).and_then(copy_text),
        (Some(ring), None) => ring.iter().rev().find_map(copy_text),
        (None, _) => None,
    };
    let Some(text) = picked else {
        app.status = "niente testo da copiare in questo messaggio".into();
        return;
    };
    let head: String = text.chars().take(24).collect();
    app.status = format!("copiato negli appunti: «{head}…»");
    std::thread::spawn(move || {
        // il testo va su stdin: come argomento, un messaggio che inizia
        // per "-" ("Adesso -n ciao") veniva preso per una opzione wl-copy
        use std::io::Write;
        use std::process::{Command, Stdio};
        let spawned = Command::new("wl-copy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(mut ch) => {
                if let Some(mut si) = ch.stdin.take() {
                    let _ = si.write_all(text.as_bytes());
                } // drop → EOF: wl-copy legge il contenuto e resta a servire
                  // i paste finché il clipboard non viene rimpiazzato
                let _ = ch.wait();
            }
            Err(_) => eprintln!("[rtete] wl-copy non trovato (installa wl-clipboard)"),
        }
    });
}

/// cmd+V: incolla dagli appunti. Se il clipboard contiene un'immagine
/// (image/png o image/jpeg) la salva in tmp e la prepara come foto da
/// inviare; altrimenti fa il paste di testo (via wl-paste, esterno e
/// momentaneo). Nel compose il testo multi-riga resta intatto; nella
/// ricerca gli accapo diventano spazi e il cap limita i caratteri.
fn ctrl_paste(app: &mut App) {
    if app.show_search {
        app.del_arm = false;
        app.del_id = None;
        let text = clipboard_text();
        if text.is_empty() {
            app.status = "appunti vuoti (serve wl-clipboard)".into();
            return;
        }
        let clean = paste_text(&text, false, 200);
        app.search_q.push_str(&clean);
    } else if app.open.is_some() && app.editing.is_none() {
        // immagine negli appunti? diventa la foto da inviare
        if let Some((path, kb)) = clipboard_image() {
            if let Some(old) = app.pending_photo.take() {
                let _ = std::fs::remove_file(old);
            }
            app.pending_photo = Some(path.display().to_string());
            app.del_arm = false;
            app.del_id = None;
            app.status = format!("foto ({kb} KB) pronta — Invio invia, il testo del compose è la didascalia");
            return;
        }
        app.del_arm = false;
        app.del_id = None;
        let text = clipboard_text();
        if text.is_empty() {
            app.status = "appunti vuoti (serve wl-clipboard)".into();
            return;
        }
        let cap = 4000usize.saturating_sub(app.compose.chars().count());
        let clean = paste_text(&text, true, cap.min(4000));
        app.compose.push_str(&clean);
    }
}

/// Cerca un'immagine image/png|jpeg negli appunti: la butta su file in
/// ~/.local/share/rtete/tmp/ e ritorna (percorso, KB). WL-clipboard.
fn clipboard_image() -> Option<(std::path::PathBuf, usize)> {
    use std::io::Write as _;
    let probe_wl = |mut cmd: std::process::Command| -> Option<Vec<u8>> {
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        let res = cmd.output().ok()?;
        Some(res.stdout)
    };
    let probe = |extra: &[&str]| -> Option<Vec<u8>> {
        let mut cmd = std::process::Command::new("wl-paste");
        for a in extra {
            cmd.arg(a);
        }
        probe_wl(cmd)
    };
    let raw = probe(&["--list-types"])?;
    let types = String::from_utf8_lossy(&raw);
    let mime = ["image/png", "image/jpeg"].iter().copied().find(|m| {
        types
            .lines()
            .any(|t| t.trim().eq_ignore_ascii_case(m))
    })?;
    let bytes = probe(&["--type", mime])?;
    if bytes.is_empty() {
        return None;
    }
    // tmp sta dentro data_dir (0700): uno screenshot non resta leggibile
    // dagli altri utenti come farebbe un 644
    let dir = tg::tmp_dir();
    tg::ensure_private_dir(&dir).ok()?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let ext = if mime == "image/jpeg" { "jpg" } else { "png" };
    let path = dir.join(format!("appunti-{ts}.{ext}"));
    // 0600 sin dalla creazione, non scritto con la umask e poi sistemato
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut f| f.write_all(&bytes))
        .ok()?;
    Some((path, bytes.len() / 1024))
}

/// testo degli appunti via wl-paste (legge i byte UTF-8; se non c'è, vuoto).
fn clipboard_text() -> String {
    match std::process::Command::new("wl-paste")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .spawn()
    {
        Ok(ch) => ch
            .wait_with_output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default(),
        Err(_) => String::new(),
    }
}

/// Pulisce il testo incollato: via i caratteri di controllo, le nuove
/// righe restano nel compose (messaggi multi-riga) e diventano spazi
/// nella ricerca. `cap` limita i caratteri (Telegram: 4096).
fn paste_text(raw: &str, keep_nl: bool, cap: usize) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if out.chars().count() >= cap {
            break;
        }
        match ch {
            '\r' => {}
            '\n' if keep_nl => out.push('\n'),
            '\n' => out.push(' '),
            '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    if keep_nl {
        out.trim_end().to_string()
    } else {
        while out.contains("  ") {
            out = out.replace("  ", " ");
        }
        out.trim().to_string()
    }
}

/// muove il cursore nella lista dei risultati di ricerca
fn move_search(app: &mut App, down: bool) {
    app.status.clear();
    let Some(results) = app.search_results.as_ref() else {
        return;
    };
    let last = results.len().saturating_sub(1);
    if down {
        app.search_cursor = (app.search_cursor + 1).min(last);
    } else {
        app.search_cursor = app.search_cursor.saturating_sub(1);
    }
}

/// Indice massimo valido nella griglia del set aperto.
fn grid_max(app: &App) -> usize {
    app.open_set
        .and_then(|id| app.set_docs.get(&id))
        .map(|d| d.len())
        .unwrap_or(0)
        .saturating_sub(1)
}

fn move_up(app: &mut App) {
    app.status.clear();
    if app.show_sticker {
        if !app.set_level {
            app.set_cursor = app.set_cursor.saturating_sub(1);
        } else {
            app.sticker_cursor = app.sticker_cursor.saturating_sub(STICKER_COLS);
        }
    } else if app.open.is_some() {
        // dentro una chat: selettore dei messaggi
        move_msg_cursor(app, false);
    } else {
        app.contact_cursor = app.contact_cursor.saturating_sub(1);
    }
}

/// Un passo in giù: cursore messaggi se una chat è aperta, contatti altrimenti.
fn move_down(app: &mut App) {
    app.status.clear();
    if app.show_sticker {
        if !app.set_level {
            let max = app.sets.as_ref().map(|s| s.len()).unwrap_or(0);
            app.set_cursor = (app.set_cursor + 1).min(max.saturating_sub(1));
        } else {
            app.sticker_cursor = (app.sticker_cursor + STICKER_COLS).min(grid_max(app));
        }
    } else if app.open.is_some() {
        move_msg_cursor(app, true);
    } else {
        let max = app.contacts.len().saturating_sub(1);
        app.contact_cursor = (app.contact_cursor + 1).min(max);
    }
}

/// Sposta il cursore dei messaggi (↑/↓ o rotella nella zona messaggi).
/// ↓ oltre l'ultimo = esce dalla selezione (torna alla vista "live").
fn move_msg_cursor(app: &mut App, down: bool) {
    let ids: Vec<i32> = app
        .current_chat()
        .and_then(|c| app.histories.get(&c.peer_id))
        .map(|d| d.iter().map(|r| r.id).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        app.msg_cursor = None;
        return;
    }
    let next = match app.msg_cursor {
        None => {
            if down {
                None // è già in cima alla vista (ultimo messaggio)
            } else {
                ids.last().copied()
            }
        }
        Some(cur) => {
            let pos = ids.iter().position(|i| *i == cur);
            match pos {
                Some(p) if down => ids.get(p + 1).copied(),
                Some(p) if p > 0 => Some(ids[p - 1]),
                _ => {
                    if down {
                        None
                    } else {
                        Some(ids[0])
                    }
                }
            }
        }
    };
    app.msg_cursor = next;
    match next {
        Some(id) => app.status = format!("selezionato #{id} — Esc per uscire"),
        None => app.status.clear(),
    }
}

/// Dentro il pannello: sinistra nella griglia, o ritorno alla lista set.
fn grid_left(app: &mut App) {
    app.status.clear();
    if app.set_level {
        let col = app.sticker_cursor % STICKER_COLS;
        if col == 0 {
            app.set_level = false;
            app.sticker_cursor = 0;
        } else {
            app.sticker_cursor -= 1;
        }
    }
}

/// Dentro il pannello: destra nella griglia, o ingresso nel set sotto il cursore.
fn grid_right(app: &mut App, set_req_tx: &mpsc::UnboundedSender<(i64, String)>) {
    app.status.clear();
    if !app.set_level {
        if let Some(sets) = app.sets.as_ref() {
            if let Some(set) = sets.get(app.set_cursor) {
                app.set_level = true;
                app.open_set = Some(set.set_id);
                app.sticker_cursor = 0;
                // già qui, o in attesa di backoff da un errore recente? via
                if !app.set_docs.contains_key(&set.set_id) && !app.docs_cooling(set.set_id) {
                    app.status = format!("scarico il set {}…", set.title);
                    let _ = set_req_tx.send((set.set_id, set.short_name.clone()));
                }
            }
        }
    } else {
        app.sticker_cursor = (app.sticker_cursor + 1).min(grid_max(app));
    }
}

#[allow(clippy::too_many_arguments)]
async fn on_enter(
    app: &mut App,
    hist_tx: &mpsc::UnboundedSender<(PeerRefT, PeerId)>,
    send_tx: &mpsc::UnboundedSender<SendJob>,
    set_req_tx: &mpsc::UnboundedSender<(i64, String)>,
    conv_tx: &mpsc::UnboundedSender<Media>,
    del_tx: &mpsc::UnboundedSender<(PeerRefT, PeerId, i32)>,
    search_tx: &mpsc::UnboundedSender<(PeerRefT, String)>,
    react_tx: &mpsc::UnboundedSender<(PeerRefT, PeerId, i32, Option<String>)>,
) {
    // reazioni veloci aperte: Invio toggle sull'emoji corrente
    if app.react_open {
        let emoticon = tg::QUICK_REACTIONS[app.react_idx].to_string();
        let mid = app.msg_cursor;
        let peer = app
            .current_chat()
            .map(|c| (c.peer_ref, c.peer_id));
        if let (Some(mid), Some((peer_ref, pid))) = (mid, peer) {
            // già mia? → la tolgo
            let mine_already = app
                .open_history()
                .and_then(|ring| ring.iter().find(|r| r.id == mid))
                .map(|r| r.reactions.iter().any(|i| i.mine && i.emoticon == emoticon))
                .unwrap_or(false);
            let _ = react_tx.send((
                peer_ref,
                pid,
                mid,
                if mine_already { None } else { Some(emoticon.clone()) },
            ));
            app.optimistic_react(&emoticon);
            app.react_open = false;
            app.status = if mine_already {
                "reazione via…".into()
            } else {
                format!("reazione {emoticon} su #{mid}…")
            };
        } else {
            app.status = "seleziona prima un messaggio (↑/↓)".into();
            app.react_open = false;
        }
        return;
    }

    // pannello sticker: prima di tutto
    if app.show_sticker {
        let docs = app.open_set.and_then(|id| app.set_docs.get(&id)).cloned();
        if !app.set_level {
            if let Some(sets) = app.sets.as_ref() {
                if let Some(set) = sets.get(app.set_cursor) {
                    app.set_level = true;
                    app.open_set = Some(set.set_id);
                    app.sticker_cursor = 0;
                    // già qui, o in attesa di backoff da un errore recente? via
                    if !app.set_docs.contains_key(&set.set_id) && !app.docs_cooling(set.set_id) {
                        app.status = format!("scarico il set {}…", set.title);
                        let _ = set_req_tx.send((set.set_id, set.short_name.clone()));
                    }
                }
            }
        } else if let Some(docs) = docs {
            if let Some(chat) = app.current_chat() {
                if let Some(media) = docs.get(app.sticker_cursor) {
                    let _ = send_tx.send(SendJob {
                        peer: chat.peer_ref,
                        peer_id: chat.peer_id,
                        kind: SendKind::Sticker(media.clone()),
                    });
                    app.status = "invio sticker…".into();
                }
            }
            // anteprime: converti tutta la griglia
            app.ensure_previews(conv_tx, docs.iter());
        } else {
            app.status = "sticker ancora in scaricamento…".into();
        }
        return;
    }

    // passare a un'altra chat: chiude edit/canc/ricerca
    if app.open.is_none() || app.contact_cursor != app.open.unwrap_or(usize::MAX) {
        if let Some(chat) = app.contacts.get(app.contact_cursor) {
            app.open = Some(app.contact_cursor);
            let _ = hist_tx.send((chat.peer_ref, chat.peer_id));
            app.status = "carico la cronologia…".into();
            app.editing = None;
            app.del_arm = false;
            app.del_id = None;
            app.msg_cursor = None;
            app.pending_photo = None;
            app.show_search = false;
            app.search_results = None;
            app.search_q.clear();
            return;
        }
    }

    // conferma della cancellazione armata (Ctrl-D)
    if app.del_arm {
        app.del_arm = false;
        if let Some(chat) = app.current_chat() {
            let (peer_ref, pid) = (chat.peer_ref, chat.peer_id);
            let target = app.del_id.take().or_else(|| {
                app.histories
                    .get(&pid)
                    .and_then(|ring| ring.iter().rev().find(|r| r.outgoing).map(|r| r.id))
            });
            if let Some(id) = target {
                let _ = del_tx.send((peer_ref, pid, id));
                app.status = format!("cancello il messaggio #{id}…");
                if app.msg_cursor == Some(id) {
                    app.msg_cursor = None;
                }
            } else {
                app.status = "niente da cancellare".into();
            }
        }
        return;
    }

    // ricerca nella chat aperta (Ctrl-F): Invio la lancia sul server
    if app.show_search {
        if let Some(chat) = app.current_chat() {
            if app.search_q.trim().is_empty() {
                app.status = "scrivi cosa cercare".into();
                return;
            }
            let _ = search_tx.send((chat.peer_ref, app.search_q.clone()));
            app.status = format!("cerco «{}»…", app.search_q);
        }
        return;
    }

    // foto dagli appunti in attesa? Invio la invia (testo = didascalia)
    if let Some(path) = app.pending_photo.take() {
        let caption_text = app.compose.clone();
        app.compose.clear();
        app.editing = None;
        if let Some(chat) = app.current_chat() {
            let (peer_ref, pid) = (chat.peer_ref, chat.peer_id);
            let _ = send_tx.send(SendJob {
                peer: peer_ref,
                peer_id: pid,
                kind: SendKind::Photo {
                    path,
                    caption: caption_text,
                },
            });
            app.status = "invio la foto…".into();
        } else {
            app.pending_photo = Some(path);
            app.status = "apri prima una chat".into();
        }
        return;
    }

    // modifica di un messaggio esistente (Ctrl-E)
    if let Some(edit_id) = app.editing {
        // preleva i dati peer PRIMA di toccare app.compose
        let peer = app.current_chat().map(|c| (c.peer_ref, c.peer_id));
        if !app.compose.trim().is_empty() {
            let text = app.compose.clone();
            app.compose.clear();
            app.editing = None;
            if let Some((peer_ref, peer_id)) = peer {
                let _ = send_tx.send(SendJob {
                    peer: peer_ref,
                    peer_id,
                    kind: SendKind::EditText { id: edit_id, text },
                });
                app.status = format!("salvo la modifica #{edit_id}…");
            }
        } else {
            app.status = "testo vuoto: niente da modificare".into();
        }
        return;
    }

    if !app.compose.trim().is_empty() {
        let chat_data = app.current_chat().map(|c| c.peer_ref);
        if let Some(peer_ref) = chat_data {
            let peer_id = app.current_chat().map(|c| c.peer_id);
            let text = app.compose.clone();
            app.compose.clear();
            let _ = send_tx.send(SendJob {
                peer: peer_ref,
                peer_id: peer_id.unwrap(),
                kind: SendKind::Text(text),
            });
            app.status = "invio…".into();
        }
    }
}

async fn handle_update(
    app: &mut App,
    client: &grammers_client::Client,
    update: grammers_client::update::Update,
) {
    use grammers_client::update::Update;

    /// il nome del dialogo (come in sidebar) quando il mittente non è
    /// risolvibile dal raw update (mappa peers mancante in grammers).
    fn dialog_or(app: &App, pid: PeerId, fallback: String) -> String {
        if !fallback.is_empty() {
            return fallback;
        }
        app.contacts
            .iter()
            .find(|d| tg::same_peer(d.peer_id, pid))
            .map(|d| d.name.clone())
            .unwrap_or(fallback)
    }

    match update {
        Update::NewMessage(m) => {
            let m = m.into_inner();
            let peer_id = m.peer_id();
            let outgoing = m.outgoing();
            let row = tg::HistoryRow {
                id: m.id(),
                outgoing,
                from: dialog_or(app, peer_id, tg::sender_name(&m)),
                kind: tg::row_kind(m.media(), m.text().to_string()),
                reactions: tg::collect_reactions(&m.raw),
            };
            let open_id = app.current_chat().map(|c| c.peer_id);
            if Some(peer_id) == open_id {
                app.push_row(peer_id, row);
                if !outgoing && app.read_mark_due() {
                    // la chat aperta è a fuoco: i nuovi arrivati sono letti
                    // (prima mark_as_read partiva solo al load della storia)
                    if let Some(chat) = app.current_chat() {
                        let peer_ref = chat.peer_ref;
                        let cl = client.clone();
                        tokio::spawn(async move {
                            let _ = cl.mark_as_read(peer_ref).await;
                        });
                    }
                }
            } else if !outgoing {
                for d in app.contacts.iter_mut() {
                    if tg::same_peer(d.peer_id, peer_id) {
                        d.unread += 1;
                        break;
                    }
                }
            }
        }
        Update::MessageEdited(m) => {
            let m = m.into_inner();
            let peer_id = m.peer_id();
            let row = tg::HistoryRow {
                id: m.id(),
                outgoing: m.outgoing(),
                from: dialog_or(app, peer_id, tg::sender_name(&m)),
                kind: tg::row_kind(m.media(), m.text().to_string()),
                reactions: tg::collect_reactions(&m.raw),
            };
            app.replace_row(peer_id, row);
            if app.editing == Some(m.id()) {
                app.editing = None;
                app.compose.clear();
            }
        }
        Update::MessageDeleted(deletion) => {
            let ids: Vec<i32> = deletion.messages().to_vec();
            // Gli id dei messaggi nei canali/supergroup sono per-canale e
            // collidono con quelli delle chat personali: il retain su tutte
            // le cronologie rimuoveva righe della chat sbagliata.
            match deletion.channel_id().and_then(PeerId::channel) {
                Some(pid) => {
                    if let Some(ring) = app.histories.get_mut(&pid) {
                        ring.retain(|r| !ids.contains(&r.id));
                    }
                }
                None => {
                    // personali e piccoli gruppi: id unici per account, il
                    // retain su quelle cronologie è corretto; i canali no
                    for (pid, ring) in app.histories.iter_mut() {
                        if pid.kind() == PeerKind::Channel {
                            continue;
                        }
                        ring.retain(|r| !ids.contains(&r.id));
                    }
                }
            }
            if !ids.is_empty() {
                app.status = format!("· {} messaggi cancellati in chat", ids.len());
            }
        }
        Update::Raw(raw) => {
            // le reazioni arrivano come update grezzo: aggiorna la riga
            use grammers_client::tl;
            if let tl::enums::Update::MessageReactions(u) = raw.raw {
                let items = tg::reactions_from(&u.reactions);
                app.apply_reactions(PeerId::from(u.peer), u.msg_id, items);
            }
        }
        _ => {}
    }
}

#[allow(clippy::large_enum_variant)]
enum LoopEvent {
    History((PeerId, Result<Vec<tg::HistoryRow>, String>)),
    Sets(Result<Vec<tg::StickerSetInfo>, String>),
    Docs((i64, Result<Vec<Media>, String>)),
    Converted((String, Result<PathBuf, String>)),
    Sent(Result<SendDone, String>),
    Deleted(Result<(), String>),
    Searched(Result<Vec<tg::SearchHit>, String>),
    Saved(Result<String, String>),
    Played(Result<String, String>),
    Reacted(Result<(), String>),
}

/// Individua un comando nel PATH senza dipendenze esterne.
fn which(cmd: &str) -> bool {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|d| std::path::Path::new(d).join(cmd).exists())
}

/// Primo servizio che risponde tra i canali.
#[allow(clippy::too_many_arguments)]
async fn waiting_pick(
    hist_rx: &mut mpsc::UnboundedReceiver<(PeerId, Result<Vec<tg::HistoryRow>, String>)>,
    sets_list_rx: &mut mpsc::UnboundedReceiver<Result<Vec<tg::StickerSetInfo>, String>>,
    docs_rx: &mut mpsc::UnboundedReceiver<(i64, Result<Vec<Media>, String>)>,
    done_rx: &mut mpsc::UnboundedReceiver<(String, Result<PathBuf, String>)>,
    sent_rx: &mut mpsc::UnboundedReceiver<Result<SendDone, String>>,
    deleted_rx: &mut mpsc::UnboundedReceiver<Result<(), String>>,
    searched_rx: &mut mpsc::UnboundedReceiver<Result<Vec<tg::SearchHit>, String>>,
    saved_rx: &mut mpsc::UnboundedReceiver<Result<String, String>>,
    played_rx: &mut mpsc::UnboundedReceiver<Result<String, String>>,
    reacted_rx: &mut mpsc::UnboundedReceiver<Result<(), String>>,
) -> LoopEvent {
    tokio::select! {
        v = hist_rx.recv() => LoopEvent::History(v.expect("servizio cronologia chiuso")),
        v = sets_list_rx.recv() => LoopEvent::Sets(v.expect("servizio set chiuso")),
        v = docs_rx.recv() => LoopEvent::Docs(v.expect("servizio documenti chiuso")),
        v = done_rx.recv() => LoopEvent::Converted(v.expect("servizio conversioni chiuso")),
        v = sent_rx.recv() => LoopEvent::Sent(v.expect("servizio invii chiuso")),
        v = deleted_rx.recv() => LoopEvent::Deleted(v.expect("servizio cancellazioni chiuso")),
        v = searched_rx.recv() => LoopEvent::Searched(v.expect("servizio ricerche chiuso")),
        v = saved_rx.recv() => LoopEvent::Saved(v.expect("servizio salvataggi chiuso")),
        v = played_rx.recv() => LoopEvent::Played(v.expect("servizio riproduzioni chiuso")),
        v = reacted_rx.recv() => LoopEvent::Reacted(v.expect("servizio reazioni chiuso")),
    }
}

/// La TUI sta a schermo; consumata una volta sola dal panic hook o dal
/// Drop della guard, così non si ripristina due volte la stessa cosa.
static TUI_UP: AtomicBool = AtomicBool::new(false);

/// Ripristina raw mode, schermo alternativo, mouse e keyboard protocol.
/// Ogni istruzione è eseguita con `let _`: se una fallisce, le altre
/// partono comunque.
fn restore_terminal() {
    kitty::clear_all();
    let mut out = std::io::stdout();
    let _ = execute!(out, crossterm::cursor::Show);
    let _ = execute!(out, crossterm::event::PopKeyboardEnhancementFlags);
    let _ = execute!(out, crossterm::event::DisableMouseCapture);
    let _ = execute!(out, crossterm::event::DisableBracketedPaste);
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = execute!(out, crossterm::terminal::LeaveAlternateScreen);
}

/// Uscita pulita dal terminale per qualunque ritorno: con `Ok`, con `?`
/// o con panic. In release `panic = "abort"` lascia il terminale in
/// raw mode (il Drop della guard non viene eseguito), quindi serve
/// anche un hook — che invece gira sempre.
struct TermGuard;

impl Drop for TermGuard {
    fn drop(&mut self) {
        if TUI_UP.swap(false, Ordering::SeqCst) {
            restore_terminal();
        }
    }
}

fn panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if TUI_UP.swap(false, Ordering::SeqCst) {
            restore_terminal();
        }
        prev(info);
    }));
}

async fn render(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> Result<Vec<Cmd>, Box<dyn std::error::Error>> {
    let mut cmds = Vec::new();
    terminal.draw(|f| view::draw(f, app, &mut cmds))?;
    Ok(cmds)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let creds = match tg::load_credentials() {
        Some(c) => c,
        None => tg::prompt_credentials().await?,
    };
    let (_session, client, updates) = tg::connect(&creds).await?;

    // rileva il supporto kitty graphics prima che la TUI prenda stdin
    kitty::startup_detect();

    println!("Verifico l'accesso al tuo account…");
    tg::sign_in(&client, &creds).await?;

    println!("Carico le conversazioni…");
    let mut app = App {
        contacts: tg::load_dialogs(&client).await?,
        contact_cursor: 0,
        open: None,
        histories: HashMap::new(),
        compose: String::new(),
        status: String::new(),
        show_sticker: false,
        set_level: false,
        set_cursor: 0,
        sticker_cursor: 0,
        sets: None,
        set_docs: HashMap::new(),
        docs_failed: HashMap::new(),
        open_set: None,
        previews: HashMap::new(),
        pending: HashSet::new(),
        failed: HashMap::new(),
        me: tg::own_name(&client).await,
        msg_cursor: None,
        del_id: None,
        editing: None,
        del_arm: false,
        show_search: false,
        search_q: String::new(),
        search_cursor: 0,
        search_results: None,
        mouse_map: Vec::new(),
        link_map: Vec::new(),
        pending_photo: None,
        react_open: false,
        react_idx: 0,
        read_mark: None,
        quit: false,
    };

    // ---------------- canali di servizio -------------------------------
    let (hist_tx, mut hist_req) = mpsc::unbounded_channel::<(PeerRefT, PeerId)>();
    let (hist_done, mut hist_rx) =
        mpsc::unbounded_channel::<(PeerId, Result<Vec<tg::HistoryRow>, String>)>();
    let (sets_tx, mut sets_rx) = mpsc::unbounded_channel::<()>();
    let (sets_done, mut sets_list_rx) =
        mpsc::unbounded_channel::<Result<Vec<tg::StickerSetInfo>, String>>();
    let (set_req_tx, mut set_req_rx) = mpsc::unbounded_channel::<(i64, String)>();
    let (docs_tx, mut docs_rx) = mpsc::unbounded_channel::<(i64, Result<Vec<Media>, String>)>();
    let (conv_tx, conv_rx) = mpsc::unbounded_channel::<Media>();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<(String, Result<PathBuf, String>)>();
    let (send_tx, mut send_rx) = mpsc::unbounded_channel::<SendJob>();
    let (sent_tx, mut sent_rx) = mpsc::unbounded_channel::<Result<SendDone, String>>();
    let (del_tx, mut del_req) = mpsc::unbounded_channel::<(PeerRefT, PeerId, i32)>();
    let (deleted_tx, mut deleted_rx) = mpsc::unbounded_channel::<Result<(), String>>();
    let (search_tx, mut search_req) = mpsc::unbounded_channel::<(PeerRefT, String)>();
    let (searched_tx, mut searched_rx) =
        mpsc::unbounded_channel::<Result<Vec<tg::SearchHit>, String>>();
    let (save_tx, mut save_req) = mpsc::unbounded_channel::<(Media, String)>();
    let (saved_tx, mut saved_rx) = mpsc::unbounded_channel::<Result<String, String>>();
    let (play_tx, mut play_req) = mpsc::unbounded_channel::<(Media, String, String)>();
    let (played_tx, mut played_rx) = mpsc::unbounded_channel::<Result<String, String>>();
    let (react_tx, mut react_req) = mpsc::unbounded_channel::<(PeerRefT, PeerId, i32, Option<String>)>();
    let (reacted_tx, mut reacted_rx) = mpsc::unbounded_channel::<Result<(), String>>();

    // cronologia: richiesta → risultato (l'errore viaggia col canale: un
    // guasto di rete non deve presentarsi come "chat senza messaggi")
    {
        let cl = client.clone();
        let out = hist_done;
        tokio::spawn(async move {
            while let Some((peer_ref, peer_id)) = hist_req.recv().await {
                let rows = tg::load_history(&cl, peer_ref, MAX_HISTORY)
                    .await
                    .map_err(|e| e.to_string());
                let _ = out.send((peer_id, rows));
            }
        });
    }

    // lista completa dei set installati (se fallisce resta None: frenarla
    // fissa "in caricamento"; Tab, riaprendo il pannello, ritenta)
    {
        let cl = client.clone();
        let out = sets_done;
        tokio::spawn(async move {
            while sets_rx.recv().await.is_some() {
                let sets = tg::fetch_installed_sets(&cl).await.map_err(|e| e.to_string());
                let _ = out.send(sets);
            }
        });
    }

    // documenti on-demand di un set (via short_name); un errore NON marca
    // il set come "caricato": la griglia riparte al prossimo input sul
    // pannello (con la pausa di backoff di docs_failed)
    {
        let cl = client.clone();
        let out = docs_tx;
        tokio::spawn(async move {
            while let Some((set_id, short_name)) = set_req_rx.recv().await {
                let docs = tg::fetch_set_docs_by_name(&cl, &short_name)
                    .await
                    .map(|docs| docs.into_iter().filter_map(tg::media_from_doc).collect())
                    .map_err(|e| e.to_string());
                let _ = out.send((set_id, docs));
            }
        });
    }

    // conversioni webp → png, due worker in serie tra loro (poco heap)
    {
        let shared = Arc::new(tokio::sync::Mutex::new(conv_rx));
        for _ in 0..2 {
            let rx = Arc::clone(&shared);
            let cl = client.clone();
            let out = done_tx.clone();
            tokio::spawn(async move {
                loop {
                    let media = {
                        let mut guard = rx.lock().await;
                        guard.recv().await
                    };
                    let Some(media) = media else { break };
                    let key = tg::media_key(&media);
                    let res = loader::convert_to_png(cl.clone(), &media, key.clone())
                        .await
                        .map_err(|e| e.to_string());
                    let _ = out.send((key, res));
                }
            });
        }
    }

    // invio testi/sticker e modifiche
    {
        let cl = client.clone();
        let out = sent_tx;
        tokio::spawn(async move {
            while let Some(job) = send_rx.recv().await {
                let res: Result<SendDone, String> = match job.kind {
                    SendKind::Text(text) => match tg::send_text(&cl, job.peer, &text).await {
                        Ok(m) => row_from_msg(m).map(|(pid, row)| SendDone::New(pid, row)),
                        Err(e) => Err(e.to_string()),
                    },
                    SendKind::Sticker(media) => {
                        match tg::send_sticker(&cl, job.peer, &media).await {
                            Ok(m) => row_from_msg(m).map(|(pid, row)| SendDone::New(pid, row)),
                            Err(e) => Err(e.to_string()),
                        }
                    }
                    SendKind::Photo { path, caption } => {
                        match tg::send_photo(&cl, job.peer, std::path::Path::new(&path), &caption)
                            .await
                        {
                            Ok(m) => {
                                // upload conclude il compito: il file temporaneo non serve più
                                let _ = std::fs::remove_file(&path);
                                row_from_msg(m).map(|(pid, row)| SendDone::New(pid, row))
                            }
                            Err(e) => Err(e),
                        }
                    }
                    SendKind::EditText { id, text } => {
                        match tg::edit_text(&cl, job.peer, id, &text).await {
                            Ok(()) => Ok(SendDone::Edit(
                                job.peer_id,
                                tg::HistoryRow {
                                    id,
                                    outgoing: true,
                                    from: String::new(),
                                    kind: tg::RowKind::Text(text),
                                    reactions: Vec::new(),
                                },
                            )),
                            Err(e) => Err(e.to_string()),
                        }
                    }
                };
                let _ = out.send(res);
            }
        });
    }

    // cancellazioni (revoke) dei messaggi propri
    {
        let cl = client.clone();
        let out = deleted_tx;
        tokio::spawn(async move {
            while let Some((peer_ref, _peer_id, id)) = del_req.recv().await {
                let res = tg::delete_messages(&cl, peer_ref, &[id])
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string());
                let _ = out.send(res);
            }
        });
    }

    // ricerche server-side nella chat aperta
    {
        let cl = client.clone();
        let out = searched_tx;
        tokio::spawn(async move {
            while let Some((peer_ref, query)) = search_req.recv().await {
                let res = tg::search_chat(&cl, peer_ref, &query, 40)
                    .await
                    .map_err(|e| e.to_string());
                let _ = out.send(res);
            }
        });
    }

    // reazioni veloci su un messaggio selezionato
    {
        let cl = client.clone();
        let out = reacted_tx;
        tokio::spawn(async move {
            while let Some((peer_ref, _pid, msg_id, emoticon)) = react_req.recv().await {
                let res = tg::send_reaction(&cl, peer_ref, msg_id, emoticon.as_deref())
                    .await
                    .map_err(|e| e.to_string());
                let _ = out.send(res);
            }
        });
    }

    // salvataggi documenti in ~/Downloads: no sovrascritture e niente file
    // parziali col nome finale (il download finiva direttamente sul file
    // vero; il nome vuoto finiva puntando alla cartella stessa)
    {
        let cl = client.clone();
        let out = saved_tx;
        tokio::spawn(async move {
            while let Some((media, name)) = save_req.recv().await {
                let Some(sane) = tg::sanitize_file_name(&name) else {
                    let _ = out.send(Err("nome file vuoto o inutilizzabile".into()));
                    continue;
                };
                let dir = tg::downloads_dir();
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                let tmp = dir.join(format!(".rtete-{ts}.part"));
                let res = match tg::download_media_to(&cl, &media, tmp.clone()).await {
                    Ok(()) => {
                        // scaricato per intero: trova un nome libero e sposta
                        let target = tg::unique_save_path(&dir, &sane);
                        match std::fs::rename(&tmp, &target) {
                            Ok(()) => Ok(target.display().to_string()),
                            Err(e) => Err(format!("spostamento: {e}")),
                        }
                    }
                    Err(e) => Err(e),
                };
                // il .part non utile non resta a sporcare la cartella
                let _ = tokio::fs::remove_file(&tmp).await;
                let _ = out.send(res);
            }
        });
    }

    // riproduzione audio: scarica in tmp e lancia mpv/ffplay. Niente kill
    // esterni per pid (soprattutto riusati: kill -TERM su un pid girato nel
    // frattempo finiva su processi altrui): ogni player porta una
    // generazione su un canale watch. Un nuovo Ctrl-P avanza la
    // generazione e sveglia il task vecchio, che fa kill() sull'handle del
    // figlio e NON manda l'Ok postumo (prima il thread morto manteneva
    // vivo lo stato sbagliato)
    {
        let cl = client.clone();
        let out = played_tx;
        tokio::spawn(async move {
            static NEXT_GEN: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(0);
            // il player in corsa osserva il watch: valori oltre la sua
            // generazione vuol dire "sostituito"
            let mut prev_alert: Option<tokio::sync::watch::Sender<u64>> = None;
            while let Some((media, fname, label)) = play_req.recv().await {
                let gen = NEXT_GEN.fetch_add(1, Ordering::Relaxed) + 1;
                // sveglia il player in corsa, se c'è: lui ferma figlio e file
                if let Some(prev) = prev_alert.take() {
                    prev.send_replace(gen);
                }
                let (tx, mut rx) = tokio::sync::watch::channel(gen);
                prev_alert = Some(tx);

                // nel nome del temp la generazione: Ctrl-P vicini non si
                // pestano i propri file
                let target = tg::tmp_dir().join(format!("play-{gen}-{fname}"));
                let _ = out.send(Ok(format!("▶ {label}")));
                let out2 = out.clone();
                let label2 = label.clone();
                let cl2 = cl.clone();
                tokio::spawn(async move {
                    let dl = tg::download_media_to(&cl2, &media, target.clone()).await;
                    if *rx.borrow() != gen {
                        // sostituito: via il file scaricato e chiudi
                        let _ = tokio::fs::remove_file(&target).await;
                        return;
                    }
                    if let Err(e) = dl {
                        let _ = tokio::fs::remove_file(&target).await;
                        let _ = out2.send(Err(e));
                        return;
                    }
                    let player = std::env::var("RTETE_PLAYER").ok().or_else(|| {
                        ["mpv", "ffplay"]
                            .iter()
                            .find(|p| which(p))
                            .map(|p| p.to_string())
                    });
                    let Some(p) = player else {
                        let _ = out2.send(Err("nessun player: installa mpv o ffplay".into()));
                        return;
                    };
                    let is_mpv = std::path::Path::new(&p)
                        .file_name()
                        .map(|f| f == std::ffi::OsStr::new("mpv"))
                        .unwrap_or(false);
                    let mut cmd = tokio::process::Command::new(&p);
                    if is_mpv {
                        cmd.args(["--no-video", "--no-terminal", "--really-quiet"]);
                    } else {
                        cmd.args(["-nodisp", "-autoexit", "-loglevel", "quiet"]);
                    }
                    cmd.arg(&target);
                    cmd.stdout(std::process::Stdio::null());
                    cmd.stderr(std::process::Stdio::null());
                    // cintura di sicurezza: allo drop del figlio lo termina
                    cmd.kill_on_drop(true);
                    match cmd.spawn() {
                        Ok(mut child) => {
                            tokio::select! {
                                st = child.wait() => {
                                    let _ = tokio::fs::remove_file(&target).await;
                                    // l'exit naturale conta solo se l'utente
                                    // non ha già sostituito il player
                                    if st.is_ok() && *rx.borrow() == gen {
                                        let _ = out2.send(Ok(label2));
                                    }
                                }
                                _ = rx.changed() => {
                                    // sostituito da un Ctrl-P più recente: chiudi
                                    let _ = child.kill().await;
                                    let _ = tokio::fs::remove_file(&target).await;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tokio::fs::remove_file(&target).await;
                            let _ = out2.send(Err(format!("player: {e}")));
                        }
                    }
                });
            }
        });
    }


    // stream aggiornamenti live
    let cfg = grammers_client::sender::UpdatesConfiguration { catch_up: false };
    let mut update_stream = client
        .stream_updates(updates, cfg)
        .await
        .map_err(|e| e.to_string())?;

    // ---------------- TUI ----------------------------------------------
    panic_hook();
    crossterm::terminal::enable_raw_mode()?;
    // da qui ogni uscita ripristina il terminale: guard per i `?` e per il
    // return, hook per i panic (con panic = "abort" il Drop non viene eseguito)
    let _guard = TermGuard;
    TUI_UP.store(true, Ordering::SeqCst);
    execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    terminal.hide_cursor()?;
    // trackpad/rotella: invii di scorrimento verso il cursore selezione
    execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)?;
    // il terminale avvolge gli incolla: niente Invio involontari multipli
    execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste)?;
    // kitty keyboard protocol: cmd+lettera arriva come SUPER pulito
    execute!(
        std::io::stdout(),
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        )
    )?;

    let mut events = EventStream::new();
    let mut frame_had_images = false;
    let mut frame_sig: Vec<(u16, u16, u16, u16, String)> = Vec::new();

    while !app.quit {
        let mut cmds = render(&mut terminal, &mut app).await?;
        let sig: Vec<(u16, u16, u16, u16, String)> = cmds
            .iter()
            .map(|c| {
                let Cmd::Place { rect, path } = c;
                (
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                    path.display().to_string(),
                )
            })
            .collect();

        // aggiorna il pannello: prenota le anteprime degli sticker visibili
        if !frame_had_images || sig != frame_sig {
            app.refresh_preview_queue(&conv_tx);
        }

        if frame_had_images && (sig.is_empty() || sig != frame_sig) {
            kitty::clear_all();
            frame_had_images = false;
        }
        if !cmds.is_empty() && !frame_had_images {
            for cmd in cmds.drain(..) {
                let Cmd::Place { rect, path } = cmd;
                let size = kitty::square_fit(rect.width as u32, rect.height as u32);
                if kitty::draw_png(&path, (rect.x, rect.y), size).is_some() {
                    frame_had_images = true;
                }
            }
        }
        cmds.clear();
        frame_sig = sig;

        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(e)) => {
                    handle_event(
                        &mut app, e, &hist_tx, &send_tx, &sets_tx, &set_req_tx, &conv_tx,
                        &del_tx, &search_tx, &save_tx, &play_tx, &react_tx,
                    ).await
                }
                Some(Err(_)) => {}
                None => break,
            },
            upd = update_stream.next() => match upd {
                Ok(u) => handle_update(&mut app, &client, u).await,
                Err(e) => app.status = format!("rete: {e}"),
            },
            done = waiting_pick(
                &mut hist_rx,
                &mut sets_list_rx,
                &mut docs_rx,
                &mut done_rx,
                &mut sent_rx,
                &mut deleted_rx,
                &mut searched_rx,
                &mut saved_rx,
                &mut played_rx,
                &mut reacted_rx,
            ) => match done {
                LoopEvent::History((peer_id, res)) => {
                    let is_open = app.current_chat().map(|c| c.peer_id) == Some(peer_id);
                    let Ok(rows) = res else {
                        // storia vuota e storia guasta sono cose diverse: un
                        // errore di rete non deve apparire come "nessun messaggio"
                        if is_open {
                            app.status = format!("cronologia: {}", res.err().unwrap_or_default());
                        }
                        continue;
                    };
                    app.push_rows(peer_id, rows);
                    if is_open {
                        app.status = String::new();
                        // la cronologia è arrivata e la chat è a fuoco: il
                        // badge unread della barra sparisce
                        for d in app.contacts.iter_mut() {
                            if tg::same_peer(d.peer_id, peer_id) {
                                d.unread = 0;
                                break;
                            }
                        }
                        if let Some(chat) = app.current_chat() {
                            let peer_ref = chat.peer_ref;
                            let cl = client.clone();
                            tokio::spawn(async move {
                                let _ = cl.mark_as_read(peer_ref).await;
                            });
                        }
                        let medias: Vec<Media> = app
                            .histories
                            .get(&peer_id)
                            .map(|ring| {
                                ring.iter().filter_map(|r| {
                                    match &r.kind {
                                        tg::RowKind::Sticker { media, animated, .. } => {
                                            if *animated { None } else { Some(media.clone()) }
                                        }
                                        tg::RowKind::Photo { media, .. } => Some(media.clone()),
                                        _ => None,
                                    }
                                }).collect()
                            })
                            .unwrap_or_default();
                        app.ensure_previews(&conv_tx, medias.iter());
                    }
                }
                LoopEvent::Sets(res) => match res {
                    Ok(sets) => {
                        let had_none = app.sets.is_none();
                        app.sets = Some(sets);
                        if had_none {
                            app.status = String::new();
                        }
                    }
                    Err(err) => {
                        // sets resta None: riaprire il pannello ritenta
                        app.status = format!("set: {err}");
                    }
                },
                LoopEvent::Docs((set_id, res)) => match res {
                    Ok(docs) => {
                        app.docs_fine(set_id);
                        app.set_docs.insert(set_id, docs);
                        if app.open_set == Some(set_id) && app.set_level {
                            app.status = String::new();
                        }
                    }
                    Err(err) => {
                        // senza la lista la griglia può ripartire al prossimo
                        // input: non inserire il set in set_docs come vuoto
                        app.docs_failed(set_id);
                        if app.open_set == Some(set_id) {
                            app.status = format!("set: {err}");
                        }
                    }
                },
                LoopEvent::Converted((key, res)) => {
                    app.pending.remove(&key);
                    match res {
                        Ok(path) => {
                            app.failed.remove(&key);
                            app.previews.insert(key, path);
                        }
                        Err(_why) => app.conversion_failed(key),
                    }
                }
                LoopEvent::Sent(res) => match res {
                    Ok(SendDone::New(peer_id, row)) => {
                        app.push_row(peer_id, row);
                        app.status = String::new();
                    }
                    Ok(SendDone::Edit(peer_id, row)) => {
                        app.replace_row(peer_id, row);
                        app.status = "modifica salvata".into();
                    }
                    Err(err) => app.status = format!("invio: {err}"),
                },
                LoopEvent::Deleted(res) => match res {
                    Ok(()) => app.status = "cancellato".into(),
                    Err(err) => app.status = format!("cancellazione: {err}"),
                },
                LoopEvent::Searched(res) => match res {
                    Ok(hits) => {
                        let n = hits.len();
                        app.search_results = Some(hits);
                        app.search_cursor = 0;
                        app.status = format!("ricerca: {n} risultati (↓ per muoverti, Esc chiude)");
                    }
                    Err(err) => app.status = format!("ricerca: {err}"),
                },
                LoopEvent::Saved(res) => match res {
                    Ok(path) => app.status = format!("salvato in {path}"),
                    Err(err) => app.status = format!("salvataggio: {err}"),
                },
                LoopEvent::Played(res) => match res {
                    Ok(label) => app.status = label,
                    Err(err) => app.status = format!("riproduzione: {err}"),
                },
                LoopEvent::Reacted(res) => match res {
                    Ok(()) => app.status = "reazione salvata".into(),
                    Err(err) => app.status = format!("reazione: {err}"),
                },
            }
        }
    }

    Ok(())
    // il Drop di _guard fa il restore: qualunque via d'uscita esce pulito
}

impl App {
    /// Proietta la cronologia della chat aperta in righe schedulate dal TUI.
    pub fn history_lines(&self, width: usize) -> Vec<view::HistoryLine> {
        let Some(rows) = self.open_history() else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        for row in rows {
            // etichetta: nome proprio per i messaggi in uscita, mittente per gli altri
            let name = if row.outgoing {
                if self.me.is_empty() {
                    "Io"
                } else {
                    self.me.as_str()
                }
            } else if row.from.is_empty() {
                "Lui"
            } else {
                row.from.as_str()
            };
            let label = view::msg_label(name);
            let name_style = if row.outgoing {
                styles::OUTGOING
            } else {
                styles::FG
            };
            match &row.kind {
                tg::RowKind::Text(text) => {
                    let (pieces, mut span_lines) =
                        view::wrap_links(text, width.saturating_sub(view::LABEL_W + 3));
                    if pieces.is_empty() {
                        continue;
                    }
                    // le colonne dei link slittano della larghezza dell'etichetta
                    let shift = (view::LABEL_W + 3) as u16;
                    let first_spans = span_lines
                        .first_mut()
                        .map(|v| {
                            v.iter()
                                .map(|(a, b, u)| (a + shift, b + shift, u.clone()))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    lines.push(view::HistoryLine {
                        text: format!("{label}{}", pieces[0]),
                        color: name_style,
                        sticker: false,
                        animated: false,
                        sticker_media: None,
                        rows: 1,
                        frac: 0,
                        id: row.id,
                        links: first_spans,
                    });
                    for (i, piece) in pieces[1..].iter().enumerate() {
                        let links = span_lines
                            .get(i + 1)
                            .map(|v| {
                                v.iter()
                                    .map(|(a, b, u)| (a + shift, b + shift, u.clone()))
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        lines.push(view::HistoryLine {
                            text: format!("{} │ {}", " ".repeat(view::LABEL_W), piece),
                            color: name_style,
                            ..view::HistoryLine::sep()
                        });
                        if let Some(last) = lines.last_mut() {
                            last.id = row.id;
                            last.links = links;
                        }
                    }
                }
                tg::RowKind::Sticker {
                    media,
                    emoji,
                    animated,
                } => {
                    lines.push(view::HistoryLine {
                        text: format!(
                            "{label}sticker {emoji}{}",
                            if *animated {
                                " (tgs animato, non renderizzato)"
                            } else {
                                ""
                            }
                        ),
                        color: if *animated {
                            styles::WARNING
                        } else {
                            styles::ACCENT
                        },
                        sticker: !*animated,
                        animated: *animated,
                        sticker_media: Some(media.clone()),
                        rows: if *animated {
                            1
                        } else {
                            view::band_rows(false) + 1
                        },
                        frac: 50,
                        id: row.id,
                        links: Vec::new(),
                    });
                }
                tg::RowKind::Photo {
                    media,
                    caption,
                    gif,
                } => {
                    let cap_txt = if caption.is_empty() {
                        String::new()
                    } else {
                        format!(" — “{caption}”")
                    };
                    let line_text = format!(
                        "{label}{}{}",
                        if *gif { "🎞 gif" } else { "📷 foto" },
                        cap_txt
                    );
                    lines.push(view::HistoryLine {
                        text: line_text.clone(),
                        color: name_style,
                        sticker: true,
                        animated: *gif,
                        sticker_media: Some(media.clone()),
                        rows: view::band_rows(true) + 1,
                        frac: 66,
                        id: row.id,
                        links: view::scan_links(&line_text),
                    });
                }
                tg::RowKind::Doc {
                    media: _,
                    name,
                    size,
                    caption,
                } => {
                    let size_txt = size
                        .map(|s| format!(" ({})", tg::fmt_size(s as f64)))
                        .unwrap_or_default();
                    let cap_txt = if caption.is_empty() {
                        String::new()
                    } else {
                        format!(" — “{caption}”")
                    };
                    lines.push(view::HistoryLine {
                        text: format!("{label}📄 {name}{size_txt}{cap_txt} · Ctrl-O salva"),
                        color: styles::ACCENT_DIM,
                        ..view::HistoryLine::sep()
                    });
                    if let Some(last) = lines.last_mut() {
                        last.id = row.id;
                        last.links = view::scan_links(&last.text);
                    }
                }
                tg::RowKind::Audio {
                    name,
                    seconds,
                    caption,
                    voice,
                    ..
                } => {
                    let main = if *voice {
                        format!("🎙 vocale {}", tg::fmt_secs(*seconds))
                    } else {
                        format!("🎧 {}", name)
                    };
                    let cap_txt = if caption.is_empty() {
                        String::new()
                    } else {
                        format!(" — “{caption}”")
                    };
                    lines.push(view::HistoryLine {
                        text: format!("{label}{main}{cap_txt} · Ctrl-P riproduce"),
                        color: styles::ACCENT_DIM,
                        ..view::HistoryLine::sep()
                    });
                    if let Some(last) = lines.last_mut() {
                        last.links = view::scan_links(&last.text);
                    }
                }
                tg::RowKind::Note(text) => {
                    lines.push(view::HistoryLine {
                        text: format!("{label}🎥 {text}"),
                        color: styles::ACCENT_DIM,
                        ..view::HistoryLine::sep()
                    });
                    if let Some(last) = lines.last_mut() {
                        last.id = row.id;
                        last.links = view::scan_links(&last.text);
                    }
                }
                tg::RowKind::Service(text) => {
                    lines.push(view::HistoryLine {
                        text: format!("· {text}"),
                        color: styles::INCOMING,
                        ..view::HistoryLine::sep()
                    });
                    if let Some(last) = lines.last_mut() {
                        last.id = row.id;
                        last.links = view::scan_links(&last.text);
                    }
                }
            }
            // reazioni visibili sotto il messaggio
            if !row.reactions.is_empty() {
                let mut rtxt = String::new();
                for (i, r) in row.reactions.iter().enumerate() {
                    if i > 0 {
                        rtxt.push_str("  ");
                    }
                    rtxt.push_str(&format!("{} {}", r.emoticon, r.count));
                    if r.mine {
                        rtxt.push_str(" ✓");
                    }
                }
                lines.push(view::HistoryLine {
                    text: format!("{} │ {}", " ".repeat(view::LABEL_W), rtxt),
                    color: styles::FAINT,
                    ..view::HistoryLine::sep()
                });
                if let Some(last) = lines.last_mut() {
                    last.id = row.id;
                }
            }
            lines.push(view::HistoryLine::sep());
        }
        if !lines.is_empty() {
            lines.pop(); // rimuove l'ultimo separatore
        }
        lines
    }

    fn refresh_preview_queue(&mut self, conv_tx: &mpsc::UnboundedSender<Media>) {
        let mut medias: Vec<Media> = Vec::new();
        if let Some(ring) = self.open_history() {
            medias.extend(ring.iter().filter_map(|r| match &r.kind {
                tg::RowKind::Sticker {
                    media, animated, ..
                } => {
                    // tgs/webm non convertibili: solo etichetta
                    if *animated {
                        None
                    } else {
                        Some(media.clone())
                    }
                }
                tg::RowKind::Photo { media, .. } => Some(media.clone()),
                _ => None,
            }));
        }
        if self.show_sticker && self.set_level {
            if let Some(docs) = self.open_set.and_then(|id| self.set_docs.get(&id)).cloned() {
                medias.extend(docs);
            }
            medias.truncate(24);
        }
        self.ensure_previews(conv_tx, medias.iter());
    }
}
