//! RTete — client Telegram TUI per Linux, pensato per consumare pochissima RAM:
//! grammers (MTProto puro-Rust, niente TDLib), ratatui per l'interfaccia e il
//! kitty graphics protocol per mostrare gli sticker.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use futures_util::StreamExt;
use grammers_client::media::Media;
use grammers_client::session::types::PeerId;
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

fn row_from_msg(m: grammers_client::message::Message) -> Result<(PeerId, tg::HistoryRow), String> {
    let peer_id = m.peer_id();
    let row = tg::HistoryRow {
        id: m.id(),
        outgoing: true,
        from: tg::sender_name(&m),
        kind: tg::row_kind(m.media(), m.text().to_string()),
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
    /// id del set aperto.
    pub open_set: Option<i64>,
    /// media_key -> png convertito.
    pub previews: HashMap<String, PathBuf>,
    /// conversioni in volo.
    pub pending: HashSet<String>,
    /// il nome dell'utente connesso (etichette "Nome │ testo").
    pub me: String,
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

    /// Coda le conversioni degli sticker visibili non in cache.
    fn ensure_previews<'a>(
        &mut self,
        conv_tx: &mpsc::UnboundedSender<Media>,
        medias: impl IntoIterator<Item = &'a Media>,
    ) {
        for media in medias {
            let key = tg::media_key(media);
            if self.previews.contains_key(&key) || !self.pending.insert(key.clone()) {
                continue;
            }
            let _ = conv_tx.send(media.clone());
        }
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
) {
    match ev {
        Event::Resize(_, _) => {}
        Event::Key(k) => {
            if k.kind == KeyEventKind::Release {
                return;
            }
            // combo Ctrl: non toccano il testo composto
            if k.modifiers.contains(KeyModifiers::CONTROL) {
                if let KeyCode::Char(c) = k.code {
                    match c {
                        'c' => app.quit = true,
                        // modifica l'ultimo tuo messaggio di testo
                        'e' => ctrl_edit(app),
                        // arma la cancellazione dell'ultimo tuo messaggio
                        'd' => ctrl_delete(app),
                        // salva l'ultimo documento ricevuto in ~/Downloads
                        'o' => ctrl_save(app, save_tx),
                        // riproduce l'ultimo audio (vocale o brano)
                        'p' => ctrl_play(app, play_tx),
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
                    if !app.show_sticker && !app.show_search && app.compose.is_empty() =>
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
                    } else {
                        if app.editing.is_some() {
                            app.status = "modifica annullata".into();
                        } else if app.del_arm {
                            app.status = "cancellazione annullata".into();
                        } else {
                            app.status.clear();
                        }
                        app.editing = None;
                        app.del_arm = false;
                        app.compose.clear();
                    }
                }
                // orizzontale: solo nel pannello sticker (→ = l, ← = h)
                KeyCode::Right if app.show_sticker => grid_right(app, set_req_tx),
                KeyCode::Left if app.show_sticker => grid_left(app),
                KeyCode::Char('h') if app.show_sticker => grid_left(app),
                KeyCode::Char('l') if app.show_sticker => grid_right(app, set_req_tx),
                // Tab apre/chiude il pannello sticker: 's' torna una lettera
                KeyCode::Tab if !app.show_search && !app.del_arm => {
                    app.editing = None;
                    app.del_arm = false;
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
                    )
                    .await
                }
                KeyCode::Backspace => {
                    app.del_arm = false;
                    if app.show_search {
                        app.search_q.pop();
                    } else {
                        app.compose.pop();
                    }
                }
                KeyCode::Char(c) if !app.show_sticker => {
                    app.del_arm = false;
                    if app.show_search {
                        app.search_q.push(c);
                    } else {
                        app.compose.push(c);
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Prepara la modifica dell'ultimo messaggio di testo scritto da noi.
fn ctrl_edit(app: &mut App) {
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
    let found = ring.iter().rev().find_map(|r| match &r.kind {
        tg::RowKind::Text(text) if r.outgoing => Some(r.id).map(|id| (id, text.clone())),
        _ => None,
    });
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
    let Some(last_own) = ring.iter().rev().find(|r| r.outgoing).map(|r| r.id) else {
        app.status = "nessun tuo messaggio da cancellare".into();
        return;
    };
    app.del_arm = true;
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
    let found = ring.iter().rev().find_map(|r| match &r.kind {
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
    });
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
    let found = ring.iter().rev().find_map(|r| match &r.kind {
        tg::RowKind::Doc { media, name, .. } => Some((media.clone(), name.clone())),
        _ => None,
    });
    match found {
        Some((media, name)) => {
            let safe = name.replace('/', "_").replace("..", "_");
            app.status = format!("salvo «{safe}» in ~/Downloads…");
            let _ = save_tx.send((media, safe));
        }
        None => app.status = "nessun documento da salvare nella cronologia caricata".into(),
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
    } else {
        app.contact_cursor = app.contact_cursor.saturating_sub(1);
    }
}

fn move_down(app: &mut App) {
    app.status.clear();
    if app.show_sticker {
        if !app.set_level {
            let max = app.sets.as_ref().map(|s| s.len()).unwrap_or(0);
            app.set_cursor = (app.set_cursor + 1).min(max.saturating_sub(1));
        } else {
            app.sticker_cursor = (app.sticker_cursor + STICKER_COLS).min(grid_max(app));
        }
    } else {
        let max = app.contacts.len().saturating_sub(1);
        app.contact_cursor = (app.contact_cursor + 1).min(max);
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
                if !app.set_docs.contains_key(&set.set_id) {
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
) {
    // pannello sticker: prima di tutto
    if app.show_sticker {
        let docs = app.open_set.and_then(|id| app.set_docs.get(&id)).cloned();
        if !app.set_level {
            if let Some(sets) = app.sets.as_ref() {
                if let Some(set) = sets.get(app.set_cursor) {
                    app.set_level = true;
                    app.open_set = Some(set.set_id);
                    app.sticker_cursor = 0;
                    if !app.set_docs.contains_key(&set.set_id) {
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
            let target = app
                .histories
                .get(&chat.peer_id)
                .and_then(|ring| ring.iter().rev().find(|r| r.outgoing).map(|r| r.id));
            if let Some(id) = target {
                let _ = del_tx.send((chat.peer_ref, chat.peer_id, id));
                app.status = format!("cancello il messaggio #{id}…");
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

async fn handle_update(app: &mut App, update: grammers_client::update::Update) {
    use grammers_client::update::Update;
    match update {
        Update::NewMessage(m) => {
            let peer_id = m.peer_id();
            let outgoing = m.outgoing();
            let row = tg::HistoryRow {
                id: m.id(),
                outgoing,
                from: tg::sender_name(&m),
                kind: tg::row_kind(m.media(), m.text().to_string()),
            };
            let open_id = app.current_chat().map(|c| c.peer_id);
            if Some(peer_id) == open_id {
                app.push_row(peer_id, row);
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
            let peer_id = m.peer_id();
            let row = tg::HistoryRow {
                id: m.id(),
                outgoing: m.outgoing(),
                from: tg::sender_name(&m),
                kind: tg::row_kind(m.media(), m.text().to_string()),
            };
            app.replace_row(peer_id, row);
            if app.editing == Some(m.id()) {
                app.editing = None;
                app.compose.clear();
            }
        }
        Update::MessageDeleted(deletion) => {
            let ids: Vec<i32> = deletion.messages().to_vec();
            for ring in app.histories.values_mut() {
                ring.retain(|r| !ids.contains(&r.id));
            }
            if !ids.is_empty() {
                app.status = format!("· {} messaggi cancellati in chat", ids.len());
            }
        }
        _ => {}
    }
}

#[allow(clippy::large_enum_variant)]
enum LoopEvent {
    History((PeerId, Vec<tg::HistoryRow>)),
    Sets(Vec<tg::StickerSetInfo>),
    Docs((i64, Vec<Media>)),
    Converted((String, Result<PathBuf, String>)),
    Sent(Result<SendDone, String>),
    Deleted(Result<(), String>),
    Searched(Result<Vec<tg::SearchHit>, String>),
    Saved(Result<String, String>),
    Played(Result<String, String>),
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
    hist_rx: &mut mpsc::UnboundedReceiver<(PeerId, Vec<tg::HistoryRow>)>,
    sets_list_rx: &mut mpsc::UnboundedReceiver<Vec<tg::StickerSetInfo>>,
    docs_rx: &mut mpsc::UnboundedReceiver<(i64, Vec<Media>)>,
    done_rx: &mut mpsc::UnboundedReceiver<(String, Result<PathBuf, String>)>,
    sent_rx: &mut mpsc::UnboundedReceiver<Result<SendDone, String>>,
    deleted_rx: &mut mpsc::UnboundedReceiver<Result<(), String>>,
    searched_rx: &mut mpsc::UnboundedReceiver<Result<Vec<tg::SearchHit>, String>>,
    saved_rx: &mut mpsc::UnboundedReceiver<Result<String, String>>,
    played_rx: &mut mpsc::UnboundedReceiver<Result<String, String>>,
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
    }
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
        open_set: None,
        previews: HashMap::new(),
        pending: HashSet::new(),
        me: tg::own_name(&client).await,
        editing: None,
        del_arm: false,
        show_search: false,
        search_q: String::new(),
        search_cursor: 0,
        search_results: None,
        quit: false,
    };

    // ---------------- canali di servizio -------------------------------
    let (hist_tx, mut hist_req) = mpsc::unbounded_channel::<(PeerRefT, PeerId)>();
    let (hist_done, mut hist_rx) = mpsc::unbounded_channel::<(PeerId, Vec<tg::HistoryRow>)>();
    let (sets_tx, mut sets_rx) = mpsc::unbounded_channel::<()>();
    let (sets_done, mut sets_list_rx) = mpsc::unbounded_channel::<Vec<tg::StickerSetInfo>>();
    let (set_req_tx, mut set_req_rx) = mpsc::unbounded_channel::<(i64, String)>();
    let (docs_tx, mut docs_rx) = mpsc::unbounded_channel::<(i64, Vec<Media>)>();
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

    // cronologia: richiesta → risultato
    {
        let cl = client.clone();
        let out = hist_done;
        tokio::spawn(async move {
            while let Some((peer_ref, peer_id)) = hist_req.recv().await {
                let rows = tg::load_history(&cl, peer_ref, MAX_HISTORY)
                    .await
                    .unwrap_or_default();
                let _ = out.send((peer_id, rows));
            }
        });
    }

    // lista completa dei set installati
    {
        let cl = client.clone();
        let out = sets_done;
        tokio::spawn(async move {
            while sets_rx.recv().await.is_some() {
                let sets = tg::fetch_installed_sets(&cl).await.unwrap_or_default();
                let _ = out.send(sets);
            }
        });
    }

    // documenti on-demand di un set (via short_name)
    {
        let cl = client.clone();
        let out = docs_tx;
        tokio::spawn(async move {
            while let Some((set_id, short_name)) = set_req_rx.recv().await {
                let docs = tg::fetch_set_docs_by_name(&cl, &short_name)
                    .await
                    .unwrap_or_default();
                let media: Vec<Media> = docs.into_iter().filter_map(tg::media_from_doc).collect();
                let _ = out.send((set_id, media));
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
                    SendKind::EditText { id, text } => {
                        match tg::edit_text(&cl, job.peer, id, &text).await {
                            Ok(()) => Ok(SendDone::Edit(
                                job.peer_id,
                                tg::HistoryRow {
                                    id,
                                    outgoing: true,
                                    from: String::new(),
                                    kind: tg::RowKind::Text(text),
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

    // salvataggi documenti in ~/Downloads
    {
        let cl = client.clone();
        let out = saved_tx;
        tokio::spawn(async move {
            while let Some((media, name)) = save_req.recv().await {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                let dir = std::path::PathBuf::from(home).join("Downloads");
                let target = dir.join(&name);
                let res = tg::download_media_to(&cl, &media, target)
                    .await
                    .map(|_| format!("~/Downloads/{name}"));
                let _ = out.send(res);
            }
        });
    }

    // riproduzione audio: scarica in tmp, lancia mpv/ffplay e pulisce dopo
    {
        let cl = client.clone();
        let out = played_tx;
        tokio::spawn(async move {
            // pid del player in corso: un nuovo Ctrl-P lo sostituisce
            let current = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
            while let Some((media, fname, label)) = play_req.recv().await {
                // ferma il player già in corsa, se c'è
                let old = current.load(std::sync::atomic::Ordering::Relaxed);
                if old != 0 {
                    let _ = std::process::Command::new("kill")
                        .args(["-TERM", &old.to_string()])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                    current.store(0, std::sync::atomic::Ordering::Relaxed);
                }
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                let dir = std::path::PathBuf::from(home).join(".local/share/rtete/tmp");
                let target = dir.join(&fname);
                let dl = tg::download_media_to(&cl, &media, target.clone()).await;
                match dl {
                    Ok(()) => {
                        let player = std::env::var("RTETE_PLAYER").ok().or_else(|| {
                            ["mpv", "ffplay"]
                                .iter()
                                .find(|p| which(p))
                                .map(|p| p.to_string())
                        });
                        match player {
                            Some(p) => {
                                let out2 = out.clone();
                                let cur = current.clone();
                                let label2 = label.clone();
                                std::thread::spawn(move || {
                                    let is_mpv = std::path::Path::new(&p)
                                        .file_name()
                                        .map(|f| f == std::ffi::OsStr::new("mpv"))
                                        .unwrap_or(false);
                                    let mut cmd = std::process::Command::new(p);
                                    if is_mpv {
                                        cmd.args(["--no-video", "--no-terminal", "--really-quiet"]);
                                    } else {
                                        cmd.args(["-nodisp", "-autoexit", "-loglevel", "quiet"]);
                                    }
                                    cmd.arg(&target);
                                    cmd.stdout(std::process::Stdio::null());
                                    cmd.stderr(std::process::Stdio::null());
                                    match cmd.spawn() {
                                        Ok(mut child) => {
                                            cur.store(
                                                child.id(),
                                                std::sync::atomic::Ordering::Relaxed,
                                            );
                                            let _ = child.wait();
                                            let _ = std::fs::remove_file(&target);
                                            let _ = out2.send(Ok(label2));
                                        }
                                        Err(e) => {
                                            let _ = out2.send(Err(format!("player: {e}")));
                                        }
                                    }
                                });
                                let _ = out.send(Ok(format!("▶ {label}")));
                            }
                            None => {
                                let _ =
                                    out.send(Err("nessun player: installa mpv o ffplay".into()));
                            }
                        }
                    }
                    Err(e) => {
                        let _ = out.send(Err(e));
                    }
                }
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
    crossterm::terminal::enable_raw_mode()?;
    execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    terminal.hide_cursor()?;

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
                        &del_tx, &search_tx, &save_tx, &play_tx,
                    ).await
                }
                Some(Err(_)) => {}
                None => break,
            },
            upd = update_stream.next() => match upd {
                Ok(u) => handle_update(&mut app, u).await,
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
            ) => match done {
                LoopEvent::History((peer_id, rows)) => {
                    app.push_rows(peer_id, rows);
                    let is_open = app.current_chat().map(|c| c.peer_id) == Some(peer_id);
                    if is_open {
                        app.status = String::new();
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
                LoopEvent::Sets(sets) => {
                    let had_none = app.sets.is_none();
                    app.sets = Some(sets);
                    if had_none {
                        app.status = String::new();
                    }
                }
                LoopEvent::Docs((set_id, docs)) => {
                    app.set_docs.insert(set_id, docs);
                    if app.open_set == Some(set_id) && app.set_level {
                        app.status = String::new();
                    }
                }
                LoopEvent::Converted((key, res)) => {
                    app.pending.remove(&key);
                    if let Ok(path) = res {
                        app.previews.insert(key, path);
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
            }
        }
    }

    // ---------------- chiusura -----------------------------------------
    kitty::clear_all();
    terminal.show_cursor()?;
    crossterm::terminal::disable_raw_mode()?;
    execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen)?;
    Ok(())
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
                    let pieces = view::wrap(text, width.saturating_sub(view::LABEL_W + 3));
                    if pieces.is_empty() {
                        continue;
                    }
                    lines.push(view::HistoryLine {
                        text: format!("{label}{}", pieces[0]),
                        color: name_style,
                        sticker: false,
                        animated: false,
                        sticker_media: None,
                        rows: 1,
                        frac: 0,
                    });
                    for piece in &pieces[1..] {
                        lines.push(view::HistoryLine {
                            text: format!("{} │ {}", " ".repeat(view::LABEL_W), piece),
                            color: name_style,
                            ..view::HistoryLine::sep()
                        });
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
                    lines.push(view::HistoryLine {
                        text: format!(
                            "{label}{}{}",
                            if *gif { "🎞 gif" } else { "📷 foto" },
                            cap_txt
                        ),
                        color: name_style,
                        sticker: true,
                        animated: *gif,
                        sticker_media: Some(media.clone()),
                        rows: view::band_rows(true) + 1,
                        frac: 66,
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
                }
                tg::RowKind::Note(text) => lines.push(view::HistoryLine {
                    text: format!("{label}🎥 {text}"),
                    color: styles::ACCENT_DIM,
                    ..view::HistoryLine::sep()
                }),
                tg::RowKind::Service(text) => lines.push(view::HistoryLine {
                    text: format!("· {text}"),
                    color: styles::INCOMING,
                    ..view::HistoryLine::sep()
                }),
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
