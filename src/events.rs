//! Gestione degli input della TUI: tastiera, mouse, incolla (update e
//! risposte dei worker stanno in `rondine::sync`).

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;

use crate::app::{App, STICKER_COLS};
use crate::clipboard::{ctrl_copy, ctrl_paste, paste_text};
use crate::send::{Request, Response, SendJob, SendKind};
use crate::tg;
use crate::view;

pub(crate) async fn handle_event(
    app: &mut App,
    ev: Event,
    req: &mpsc::UnboundedSender<Request>,
    res: &mpsc::UnboundedSender<Response>,
) {
    match ev {
        // la finestra ha nuovi pixel per cella: sixel li usa per scalare le immagini
        Event::Resize(_, _) => crate::gfx::refresh_cells(),
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
                            ctrl_paste(app, res);
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
                        'v' => ctrl_paste(app, res),
                        // modifica l'ultimo tuo messaggio di testo
                        'e' => ctrl_edit(app),
                        // arma la cancellazione dell'ultimo tuo messaggio
                        'd' => ctrl_delete(app),
                        // salva l'ultimo documento ricevuto in ~/Downloads
                        'o' => ctrl_save(app, req),
                        // riproduce l'ultimo audio (vocale o brano)
                        'p' => ctrl_play(app, req),
                        // copia il messaggio selezionato negli appunti
                        'y' => ctrl_copy(app),
                        // risponde al messaggio selezionato
                        'r' => ctrl_reply(app),
                        // reazioni veloci sul messaggio selezionato
                        't' => {
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
                            app.disarm_delete();
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
                    } else if app.pending_photo.is_some() || app.pending_file.is_some() {
                        app.status = "appunti: foto scartata".into();
                        app.discard_pending_photo();
                        app.compose.clear();
                    } else if app.editing.is_some() {
                        app.status = "modifica annullata".into();
                        app.editing = None;
                        app.compose.clear();
                    } else if app.replying.is_some() {
                        // il testo scritto resta: si può mandarlo normale
                        app.status = "risposta annullata".into();
                        app.replying = None;
                    } else if app.del_arm {
                        app.status = "cancellazione annullata".into();
                        app.disarm_delete();
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
                KeyCode::Right if app.show_sticker => grid_right(app, req),
                KeyCode::Char('h') if app.show_sticker => grid_left(app),
                KeyCode::Char('l') if app.show_sticker => grid_right(app, req),
                // Tab apre/chiude il pannello sticker: 's' torna una lettera
                KeyCode::Tab if !app.show_search && !app.del_arm => {
                    app.editing = None;
                    app.disarm_delete();
                    app.react_open = false;
                    app.discard_pending_photo();
                    app.show_sticker = !app.show_sticker;
                    app.set_level = false;
                    if app.show_sticker {
                        app.sticker_cursor = 0;
                        if app.sets.is_none() {
                            app.status = "scarico i set di sticker…".into();
                            let _ = req.send(Request::Sets);
                        }
                    }
                }
                KeyCode::Enter => on_enter(app, req).await,
                KeyCode::Backspace => {
                    app.disarm_delete();
                    if app.show_search {
                        app.search_q.pop();
                    } else {
                        app.compose.pop();
                    }
                }
                KeyCode::Char(c) if app.react_open && c.is_ascii_digit() => {
                    // to_digit, non `c - '1'` su usize: con '0' la
                    // sottrazione underflowerava e passava la guardia
                    let n = c.to_digit(10).unwrap_or(0) as usize;
                    if (1..=tg::QUICK_REACTIONS.len()).contains(&n) {
                        app.react_idx = n - 1;
                        app.status = format!("reazioni: {} (Invio per applicare)", tg::QUICK_REACTIONS[n - 1]);
                    }
                }
                KeyCode::Char(c) if !app.show_sticker && !app.react_open => {
                    app.disarm_delete();
                    if app.show_search {
                        app.search_q.push(c);
                    } else if app.compose.chars().count() < app.compose_cap() {
                        app.compose.push(c);
                    } else {
                        app.status = format!("limite di {} caratteri", app.compose_cap());
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
            app.disarm_delete();
            if app.show_search {
                let clean = paste_text(&text, false, 200);
                app.search_q.push_str(&clean);
            } else if app.open.is_some() {
                let cap = app.compose_cap().saturating_sub(app.compose.chars().count());
                let clean = paste_text(&text, true, cap);
                app.compose.push_str(&clean);
            }
        }
        _ => {}
    }
}


/// Ruota/trackpad: nel pannello ricerca scorre i risultati; nella chat,
/// a sinistra scorre i contatti, a destra usa il cursore di selezione;
/// nel pannello sticker naviga il grigliato come le frecce.
pub(crate) fn key_updown_mouse(app: &mut App, m: crossterm::event::MouseEvent, down: bool) {
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
pub(crate) fn ctrl_edit(app: &mut App) {
    app.status.clear();
    if app.show_search || app.show_sticker {
        return;
    }
    // l'edit riprende il compose: scarta un'eventuale foto pendente
    app.discard_pending_photo();
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
    // (cursor letto prima della chiamata col prestito mutabile di App)
    let cursor = app.msg_cursor;
    let found = App::picked_row(ring, cursor, |r| {
        r.outgoing && matches!(r.kind, tg::RowKind::Text(_))
    })
    .and_then(|r| match &r.kind {
        tg::RowKind::Text(t) => Some((r.id, t.clone())),
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

/// Prepara la risposta al messaggio selezionato: il prossimo invio (testo,
/// sticker o foto) lo cita. Di nuovo Ctrl-R sullo stesso messaggio, o Esc,
/// la annulla.
pub(crate) fn ctrl_reply(app: &mut App) {
    let cursor = app.msg_cursor;
    let Some(ring) = app.chat_ready_ring() else {
        return;
    };
    let Some(id) = cursor.filter(|cid| ring.iter().any(|r| r.id == *cid)) else {
        app.status = "seleziona prima un messaggio (↑/↓)".into();
        return;
    };
    if app.replying == Some(id) {
        app.replying = None;
        app.status = "risposta annullata".into();
        return;
    }
    app.replying = Some(id);
    app.editing = None;
    app.react_open = false;
    app.disarm_delete();
    app.status = "↱ scrivi la risposta · Invio invia · Esc annulla".into();
}

/// Arma la cancellazione dell'ultimo tuo messaggio (conferma con Invio).
pub(crate) fn ctrl_delete(app: &mut App) {
    let cursor = app.msg_cursor;
    let Some(ring) = app.chat_ready_ring() else {
        return;
    };
    // cursore attivo? cancella QUEL messaggio (solo se tuo: outgoing)
    let target = match cursor
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
pub(crate) fn ctrl_play(app: &mut App, req: &mpsc::UnboundedSender<Request>) {
    // cursore pre-letto: la ring arriva col prestito mutabile di App
    let cursor = app.msg_cursor;
    let Some(ring) = app.chat_ready_ring() else {
        return;
    };
    // cursore attivo? riproduci QUEL audio; altrimenti l'ultimo
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
                tg::sanitize_file_name(name).unwrap_or_else(|| "brano".into())
            };
            Some((media.clone(), fname, label))
        }
        _ => None,
    };
    let found = App::picked_row(ring, cursor, |r| {
        matches!(r.kind, tg::RowKind::Audio { .. })
    })
    .and_then(to_job);
    match found {
        Some((media, fname, label)) => {
            app.status = format!("scarico «{label}» e la riproduco…");
            let _ = req.send(Request::Play { media, fname, label, video: false });
        }
        None => app.status = "nessun audio nella cronologia caricata".into(),
    }
}

/// Salva su disco l'ultimo documento (o foto) della chat aperta.
pub(crate) fn ctrl_save(app: &mut App, req: &mpsc::UnboundedSender<Request>) {
    // cursore pre-letto: la ring arriva col prestito mutabile di App
    let cursor = app.msg_cursor;
    let Some(ring) = app.chat_ready_ring() else {
        return;
    };
    // cursore attivo? salva QUEL documento; altrimenti l'ultimo della cronologia
    let found = App::picked_row(ring, cursor, |r| {
        matches!(r.kind, tg::RowKind::Doc { .. })
    })
    .and_then(|r| match &r.kind {
        tg::RowKind::Doc { media, name, .. } => Some((media.clone(), name.clone())),
        _ => None,
    });
    match found {
        Some((media, name)) => {
            // la pulizia del nome sta nel worker (sanitize_file_name): qui
            // l'utente vede il nome originale
            app.status = format!("salvo «{name}» in ~/Downloads…");
            let _ = req.send(Request::Save { media, name });
        }
        None => app.status = "nessun documento da salvare nella cronologia caricata".into(),
    }
}

/// Click sinistro su una riga del pannello messaggi: seleziona quel
/// messaggio (stessa cosa di ↑/↓).
pub(crate) fn click_select(app: &mut App, m: crossterm::event::MouseEvent) {
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
    if let Some(id) = hit.filter(|id| *id != 0) {
        app.msg_cursor = Some(id);
        app.status = format!("selezionato #{id} — Esc esce");
    }
}

/// muove il cursore nella lista dei risultati di ricerca
pub(crate) fn move_search(app: &mut App, down: bool) {
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
pub(crate) fn grid_max(app: &App) -> usize {
    app.open_set
        .and_then(|id| app.set_docs.get(&id))
        .map(|d| d.len())
        .unwrap_or(0)
        .saturating_sub(1)
}

pub(crate) fn move_up(app: &mut App) {
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
pub(crate) fn move_down(app: &mut App) {
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
pub(crate) fn move_msg_cursor(app: &mut App, down: bool) {
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
                Some(_) => {
                    // già sulla riga più vecchia: chiedi la pagina prima
                    app.want_older = true;
                    Some(cur)
                }
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
    let tz = jiff::tz::TimeZone::system();
    app.status = match next.and_then(|id| app.open_history()?.iter().find(|r| r.id == id)) {
        Some(row) => view::row_meta(row, &tz),
        None => String::new(),
    };
}

/// Dentro il pannello: sinistra nella griglia, o ritorno alla lista set.
pub(crate) fn grid_left(app: &mut App) {
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
pub(crate) fn grid_right(app: &mut App, req: &mpsc::UnboundedSender<Request>) {
    app.status.clear();
    if app.set_level {
        app.sticker_cursor = (app.sticker_cursor + 1).min(grid_max(app));
    } else {
        enter_set(app, req);
    }
}

/// Ingresso nel set sotto il cursore: pagina griglia vuota (o in attesa di
/// backoff) resta senza richiesta; parte solo se i doc non sono già in RAM.
fn enter_set(app: &mut App, req: &mpsc::UnboundedSender<Request>) {
    let Some(set) = app.sets.as_ref().and_then(|s| s.get(app.set_cursor)) else {
        return;
    };
    let (set_id, short, title) = (set.set_id, set.short_name.clone(), set.title.clone());
    app.set_level = true;
    app.open_set = Some(set_id);
    app.sticker_cursor = 0;
    // già qui, o in attesa di backoff da un errore recente? via
    if !app.set_docs.contains_key(&set_id) && !app.docs_cooling(set_id) {
        app.status = format!("scarico il set {title}…");
        let _ = req.send(Request::SetDocs { set_id, short });
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn on_enter(app: &mut App, req: &mpsc::UnboundedSender<Request>) {
    // reazioni veloci aperte: Invio toggle sull'emoji corrente
    if app.react_open {
        let emoticon = tg::QUICK_REACTIONS[app.react_idx].to_string();
        let mid = app.msg_cursor;
        let peer = app
            .current_chat()
            .map(|c| (c.peer_ref, c.peer_id));
        if let (Some(mid), Some((peer_ref, _))) = (mid, peer) {
            // già mia? → la tolgo
            let mine_already = app
                .open_history()
                .and_then(|ring| ring.iter().find(|r| r.id == mid))
                .map(|r| r.reactions.iter().any(|i| i.mine && i.emoticon == emoticon))
                .unwrap_or(false);
            let _ = req.send(Request::React {
                peer_ref,
                msg_id: mid,
                emoticon: if mine_already { None } else { Some(emoticon.clone()) },
            });
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
            enter_set(app, req);
        } else if let Some(docs) = docs {
            let reply_to = app.replying;
            if let Some(chat) = app.current_chat() {
                if let Some(media) = docs.get(app.sticker_cursor) {
                    let _ = req.send(Request::Send(SendJob {
                        peer: chat.peer_ref,
                        peer_id: chat.peer_id,
                        kind: SendKind::Sticker(media.clone()),
                        reply_to,
                    }));
                    app.replying = None;
                    app.status = "invio sticker…".into();
                }
            }
            // anteprime: converti tutta la griglia
            app.ensure_previews(req, docs.iter());
        } else {
            app.status = "sticker ancora in scaricamento…".into();
        }
        return;
    }

    // passare a un'altra chat: chiude edit/canc/ricerca; `open` tiene il
    // PeerId (il cursore dei contatti si sposta nelle voci nuove senza
    // scartare la chat rimasta aperta: l'indice si scalarci, l'id no)
    if let Some(chat) = app.contacts.get(app.contact_cursor) {
        let switching = app.current_chat().map(|c| c.peer_id) != Some(chat.peer_id);
        if switching {
            app.open = Some(chat.peer_id);
            let _ = req.send(Request::Hist {
                peer_ref: chat.peer_ref,
                peer: chat.peer_id,
            });
            app.status = "carico la cronologia…".into();
            app.editing = None;
            app.replying = None;
            app.disarm_delete();
            app.msg_cursor = None;
            app.pending_photo = None;
            app.pending_file = None;
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
                let _ = req.send(Request::Del { peer_ref, id });
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
            let _ = req.send(Request::Search {
                peer_ref: chat.peer_ref,
                query: app.search_q.clone(),
            });
            app.status = format!("cerco «{}»…", app.search_q);
        }
        return;
    }

    // foto dagli appunti in attesa? Invio la invia (testo = didascalia)
    if let Some(path) = app.pending_photo.take() {
        let caption_text = app.compose.clone();
        app.compose.clear();
        app.editing = None;
        let reply_to = app.replying.take();
        if let Some(chat) = app.current_chat() {
            let (peer_ref, pid) = (chat.peer_ref, chat.peer_id);
            let _ = req.send(Request::Send(SendJob {
                peer: peer_ref,
                peer_id: pid,
                kind: SendKind::Photo {
                    path,
                    caption: caption_text,
                },
                reply_to,
            }));
            app.status = "invio la foto…".into();
        } else {
            app.pending_photo = Some(path);
            app.status = "apri prima una chat".into();
        }
        return;
    }

    // file copiato dal file manager: come la foto, ma il file resta
    if let Some(path) = app.pending_file.take() {
        if let Some(chat) = app.current_chat() {
            let (peer_ref, pid) = (chat.peer_ref, chat.peer_id);
            let _ = req.send(Request::Send(SendJob {
                peer: peer_ref,
                peer_id: pid,
                kind: SendKind::File {
                    path: path.display().to_string(),
                    caption: std::mem::take(&mut app.compose),
                },
                reply_to: app.replying.take(),
            }));
            app.editing = None;
            app.status = "invio il file…".into();
        } else {
            app.pending_file = Some(path);
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
                let _ = req.send(Request::Send(SendJob {
                    peer: peer_ref,
                    peer_id,
                    kind: SendKind::EditText { id: edit_id, text },
                    reply_to: None,
                }));
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
            let _ = req.send(Request::Send(SendJob {
                peer: peer_ref,
                peer_id: peer_id.unwrap(),
                kind: SendKind::Text(text),
                reply_to: app.replying.take(),
            }));
            app.status = "invio…".into();
        }
    }
}
