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
        ring.push_back(row);
        while ring.len() > MAX_HISTORY {
            ring.pop_front();
        }
    }

    fn push_rows(&mut self, peer_id: PeerId, rows: Vec<tg::HistoryRow>) {
        let ring = self.histories.entry(peer_id).or_default();
        ring.extend(rows);
        while ring.len() > MAX_HISTORY {
            ring.pop_front();
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
    kind: SendKind,
}

// I payload servono interi sui canali mpsc: il boxing aggiungerebbe solo
// allocazioni, la dimensione dell'enum non è un collo di bottiglia.
#[allow(clippy::large_enum_variant)]
enum SendKind {
    Text(String),
    Sticker(Media),
}

async fn handle_event(
    app: &mut App,
    ev: Event,
    hist_tx: &mpsc::UnboundedSender<(PeerRefT, PeerId)>,
    send_tx: &mpsc::UnboundedSender<SendJob>,
    sets_tx: &mpsc::UnboundedSender<()>,
    set_req_tx: &mpsc::UnboundedSender<(i64, String)>,
    conv_tx: &mpsc::UnboundedSender<Media>,
) {
    match ev {
        Event::Resize(_, _) => {}
        Event::Key(k) => {
            if k.kind == KeyEventKind::Release {
                return;
            }
            if k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c')) {
                app.quit = true;
                return;
            }
            match k.code {
                KeyCode::Char('q') if !app.show_sticker && app.compose.is_empty() => {
                    app.quit = true
                }
                // verticale: frecce sempre; j/k solo dove non si scrive
                // (pannello sticker o lista chat senza messaggio aperto)
                KeyCode::Up => move_up(app),
                KeyCode::Down => move_down(app),
                KeyCode::Char('j') if app.show_sticker || app.open.is_none() => move_down(app),
                KeyCode::Char('k') if app.show_sticker || app.open.is_none() => move_up(app),
                KeyCode::Esc => {
                    if app.show_sticker {
                        if app.set_level {
                            app.set_level = false;
                            app.sticker_cursor = 0;
                        } else {
                            app.show_sticker = false;
                            app.open_set = None;
                        }
                    } else {
                        app.compose.clear();
                    }
                    app.status = String::new();
                }
                // orizzontale: solo nel pannello sticker (→ = l, ← = h)
                KeyCode::Right if app.show_sticker => grid_right(app, set_req_tx),
                KeyCode::Left if app.show_sticker => grid_left(app),
                KeyCode::Char('h') if app.show_sticker => grid_left(app),
                KeyCode::Char('l') if app.show_sticker => grid_right(app, set_req_tx),
                // Tab apre/chiude il pannello sticker: 's' torna una lettera
                KeyCode::Tab => {
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
                KeyCode::Enter => on_enter(app, hist_tx, send_tx, set_req_tx, conv_tx).await,
                KeyCode::Backspace => {
                    app.compose.pop();
                }
                KeyCode::Char(c) if !app.show_sticker => app.compose.push(c),
                _ => {}
            }
        }
        _ => {}
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

async fn on_enter(
    app: &mut App,
    hist_tx: &mpsc::UnboundedSender<(PeerRefT, PeerId)>,
    send_tx: &mpsc::UnboundedSender<SendJob>,
    set_req_tx: &mpsc::UnboundedSender<(i64, String)>,
    conv_tx: &mpsc::UnboundedSender<Media>,
) {
    app.status.clear();
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
    if app.open.is_none() || app.contact_cursor != app.open.unwrap_or(usize::MAX) {
        if let Some(chat) = app.contacts.get(app.contact_cursor) {
            app.open = Some(app.contact_cursor);
            let _ = hist_tx.send((chat.peer_ref, chat.peer_id));
            app.status = "carico la cronologia…".into();
            return;
        }
    }
    if !app.compose.trim().is_empty() {
        let chat_data = app.current_chat().map(|c| c.peer_ref);
        if let Some(peer_ref) = chat_data {
            let text = app.compose.clone();
            app.compose.clear();
            let _ = send_tx.send(SendJob {
                peer: peer_ref,
                kind: SendKind::Text(text),
            });
            app.status = "invio…".into();
        }
    }
}

async fn handle_update(app: &mut App, update: grammers_client::update::Update) {
    use grammers_client::update::Update;
    if let Update::NewMessage(m) = update {
        let peer_id = m.peer_id();
        let outgoing = m.outgoing();
        let row = tg::HistoryRow {
            id: m.id(),
            outgoing,
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
}

#[allow(clippy::large_enum_variant)]
enum LoopEvent {
    History((PeerId, Vec<tg::HistoryRow>)),
    Sets(Vec<tg::StickerSetInfo>),
    Docs((i64, Vec<Media>)),
    Converted((String, Result<PathBuf, String>)),
    Sent(Result<(PeerId, tg::HistoryRow), String>),
}

/// Primo servizio che risponde tra i cinque canali.
async fn waiting_pick(
    hist_rx: &mut mpsc::UnboundedReceiver<(PeerId, Vec<tg::HistoryRow>)>,
    sets_list_rx: &mut mpsc::UnboundedReceiver<Vec<tg::StickerSetInfo>>,
    docs_rx: &mut mpsc::UnboundedReceiver<(i64, Vec<Media>)>,
    done_rx: &mut mpsc::UnboundedReceiver<(String, Result<PathBuf, String>)>,
    sent_rx: &mut mpsc::UnboundedReceiver<Result<(PeerId, tg::HistoryRow), String>>,
) -> LoopEvent {
    tokio::select! {
        v = hist_rx.recv() => LoopEvent::History(v.expect("servizio cronologia chiuso")),
        v = sets_list_rx.recv() => LoopEvent::Sets(v.expect("servizio set chiuso")),
        v = docs_rx.recv() => LoopEvent::Docs(v.expect("servizio documenti chiuso")),
        v = done_rx.recv() => LoopEvent::Converted(v.expect("servizio conversioni chiuso")),
        v = sent_rx.recv() => LoopEvent::Sent(v.expect("servizio invii chiuso")),
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
        status: "↑↓←→ o j/k naviga · ⏎ apri/invia · Tab pannello sticker · q esci".into(),
        show_sticker: false,
        set_level: false,
        set_cursor: 0,
        sticker_cursor: 0,
        sets: None,
        set_docs: HashMap::new(),
        open_set: None,
        previews: HashMap::new(),
        pending: HashSet::new(),
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
    let (sent_tx, mut sent_rx) =
        mpsc::unbounded_channel::<Result<(PeerId, tg::HistoryRow), String>>();

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

    // invio testi e sticker
    {
        let cl = client.clone();
        let out = sent_tx;
        tokio::spawn(async move {
            while let Some(job) = send_rx.recv().await {
                let sent = match job.kind {
                    SendKind::Text(text) => tg::send_text(&cl, job.peer, &text).await,
                    SendKind::Sticker(media) => tg::send_sticker(&cl, job.peer, &media).await,
                };
                let res = match sent {
                    Ok(m) => row_from_msg(m),
                    Err(e) => Err(e.to_string()),
                };
                let _ = out.send(res);
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
                    handle_event(&mut app, e, &hist_tx, &send_tx, &sets_tx, &set_req_tx, &conv_tx).await
                }
                Some(Err(_)) => {}
                None => break,
            },
            upd = update_stream.next() => match upd {
                Ok(u) => handle_update(&mut app, u).await,
                Err(e) => app.status = format!("rete: {e}"),
            },
            done = waiting_pick(&mut hist_rx, &mut sets_list_rx, &mut docs_rx, &mut done_rx, &mut sent_rx) => match done {
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
                                    if let tg::RowKind::Sticker { media, .. } = &r.kind {
                                        Some(media.clone())
                                    } else { None }
                                }).collect()
                            })
                            .unwrap_or_default();
                        app.ensure_previews(&conv_tx, medias.iter());
                    }
                }
                LoopEvent::Sets(sets) => {
                    if !sets.is_empty() {
                        app.sets = Some(sets);
                    } else {
                        app.sets = Some(Vec::new());
                    }
                    app.status = String::new();
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
                    Ok((peer_id, row)) => {
                        app.push_row(peer_id, row);
                        app.status = String::new();
                    }
                    Err(err) => app.status = format!("errore invio: {err}"),
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
            let prefix = if row.outgoing { "Io" } else { "Tu" };
            let name_style = if row.outgoing {
                styles::OUTGOING
            } else {
                styles::FG
            };
            match &row.kind {
                tg::RowKind::Text(text) => {
                    let pieces = view::wrap(text, width.saturating_sub(6));
                    if pieces.is_empty() {
                        continue;
                    }
                    lines.push(view::HistoryLine {
                        text: format!("{prefix} │ {}", pieces[0]),
                        color: name_style,
                        sticker: false,
                        animated: false,
                        sticker_media: None,
                        rows: 1,
                    });
                    for piece in &pieces[1..] {
                        lines.push(view::HistoryLine {
                            text: format!("  │ {}", piece),
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
                            "{prefix} │ sticker {emoji}{}",
                            if *animated { " (animato)" } else { "" }
                        ),
                        color: if *animated {
                            styles::WARNING
                        } else {
                            styles::ACCENT
                        },
                        sticker: true,
                        animated: *animated,
                        sticker_media: Some(media.clone()),
                        rows: view::STICKER_ROWS + 1,
                    });
                }
                tg::RowKind::Photo { .. } => lines.push(view::HistoryLine {
                    text: format!("{prefix} │ 📷 foto"),
                    color: name_style,
                    ..view::HistoryLine::sep()
                }),
                tg::RowKind::Doc { name } => lines.push(view::HistoryLine {
                    text: format!("{prefix} │ 📎 {name}"),
                    color: name_style,
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
                tg::RowKind::Sticker { media, .. } => Some(media.clone()),
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
