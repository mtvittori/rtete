//! Aggiornamenti di stato indipendenti dall'interfaccia e dal backend:
//! eventi live e risposte dei worker applicati ad `App`. Li usano sia la
//! TUI sia la GUI.

use tokio::sync::mpsc;

use crate::app::{App, OLDER_PAGE};
use crate::clipboard::handle_clipboard;
use crate::i18n::tr;
use crate::model::{self, ChatId, Live, MediaRef};
use crate::send::{Request, Response, SendDone};
use crate::trf;

/// Segna la chat come letta con la richiesta fuori dal flusso eventi:
/// il worker la manda subito ma l'input non aspetta la rete.
pub fn mark_read(req: &mpsc::UnboundedSender<Request>, peer_ref: model::ChatRef) {
    let _ = req.send(Request::MarkRead(peer_ref));
}

/// La chat è stata letta altrove fino a un certo punto: restano `still`
/// messaggi non letti.
fn read_inbox(app: &mut App, chat: ChatId, still: i32) {
    if let Some(d) = app.contacts.iter_mut().find(|d| model::same_peer(d.peer_id, chat)) {
        d.unread = still.max(0) as usize;
    }
}

/// «sta scrivendo»: Telegram lo ripete ogni ~5s finché dura, quindi
/// scade da solo dopo 6s; nei gruppi col nome di chi scrive.
fn typing(app: &mut App, chat: ChatId, who: Option<ChatId>, what: Option<&'static str>) {
    let Some(what) = what else {
        app.typing.remove(&chat);
        return;
    };
    let label = match who {
        Some(w) => {
            let name = app
                .contacts
                .iter()
                .find(|d| model::same_peer(d.peer_id, w))
                .map(|d| d.name.split_whitespace().next().unwrap_or("").to_string())
                .unwrap_or_else(|| tr("qualcuno").into());
            format!("{name} {what}")
        }
        None => what.to_string(),
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(6);
    app.typing.insert(chat, (until, label));
}

/// il nome del dialogo (come in sidebar) quando il mittente non è
/// risolvibile dall'update (mappa peers mancante nel backend).
fn dialog_or(app: &App, pid: ChatId, fallback: String) -> String {
    if !fallback.is_empty() {
        return fallback;
    }
    app.contacts
        .iter()
        .find(|d| model::same_peer(d.peer_id, pid))
        .map(|d| d.name.clone())
        .unwrap_or(fallback)
}

/// Un evento live del backend applicato allo stato.
pub fn handle_live(app: &mut App, live: Live, req: &mpsc::UnboundedSender<Request>) {
    match live {
        Live::NewMessage { chat: peer_id, mut row, dialog } => {
            let outgoing = row.outgoing;
            let known = app.contacts.iter().any(|d| model::same_peer(d.peer_id, peer_id));
            if !known && dialog.is_none() {
                // chat mai vista e senza voce pronta: si rilegge la lista
                let _ = req.send(Request::Dialogs);
            }
            app.bump_chat(peer_id, if known { None } else { dialog });
            // il messaggio è arrivato: non sta più scrivendo
            if !outgoing {
                app.typing.remove(&peer_id);
            }
            row.from = dialog_or(app, peer_id, std::mem::take(&mut row.from));
            app.set_last(peer_id, &row);
            let open_id = app.current_chat().map(|c| c.peer_id);
            if !outgoing && app.translate.is_auto(peer_id) && app.histories.contains_key(&peer_id) {
                app.tr_queue.push((peer_id, row.id, true));
            }
            if Some(peer_id) == open_id {
                app.push_row(peer_id, row);
                if !outgoing && app.read_mark_due() {
                    // la chat aperta è a fuoco: i nuovi arrivati sono letti
                    // (prima mark_as_read partiva solo al load della storia)
                    if let Some(chat) = app.current_chat() {
                        mark_read(req, chat.peer_ref);
                    }
                }
            } else if !outgoing {
                for d in app.contacts.iter_mut() {
                    if model::same_peer(d.peer_id, peer_id) {
                        d.unread += 1;
                        break;
                    }
                }
            }
        }
        Live::Edited { chat: peer_id, mut row } => {
            row.from = dialog_or(app, peer_id, std::mem::take(&mut row.from));
            let id = row.id;
            app.replace_row(peer_id, row);
            if app.editing == Some(id) {
                app.editing = None;
                app.compose.clear();
            }
        }
        Live::Deleted { chat, ids } => {
            // Gli id dei messaggi nei canali/supergroup sono per-canale e
            // collidono con quelli delle chat personali: il retain su tutte
            // le cronologie rimuoveva righe della chat sbagliata.
            match chat {
                Some(pid) => {
                    if let Some(ring) = app.histories.get_mut(&pid) {
                        ring.retain(|r| !ids.contains(&r.id));
                    }
                }
                None => {
                    // personali e piccoli gruppi: id unici per account, il
                    // retain su quelle cronologie è corretto; i canali no
                    for (pid, ring) in app.histories.iter_mut() {
                        if pid.own_msg_ids() {
                            continue;
                        }
                        ring.retain(|r| !ids.contains(&r.id));
                    }
                }
            }
            if !ids.is_empty() {
                app.status = trf!("· {} messaggi cancellati in chat", ids.len());
            }
        }
        Live::Reactions { chat, msg, items } => app.apply_reactions(chat, msg, items),
        // letti da un'altra sessione (telefono, web): il badge segue
        Live::ReadInbox { chat, still } => read_inbox(app, chat, still),
        Live::ChatChanged { chat, action } => app.apply_chat_action(chat, action),
        Live::Blocked { chat, blocked } => {
            if let Some(info) = app.peer_info.get_mut(&chat) {
                info.blocked = blocked;
            }
        }
        // le spunte doppie: l'altro ha letto fino a max_id
        Live::ReadOutbox { chat, max_id } => {
            if let Some(d) = app.contacts.iter_mut().find(|d| model::same_peer(d.peer_id, chat)) {
                d.read_out = d.read_out.max(max_id);
            }
        }
        Live::Seen { chat, seen } => {
            if let Some(d) = app.contacts.iter_mut().find(|d| model::same_peer(d.peer_id, chat)) {
                d.seen = seen;
            }
        }
        Live::Typing { chat, who, what } => typing(app, chat, who, what),
    }
}

/// L'intero ramo di risposte del ciclo eventi, estratto in funzione:
/// una ricezione sola sul canale (prima: dieci canali e un waiting_pick
/// che scartava a mano il primo che rispondeva).
pub fn handle_response(app: &mut App, resp: Response, req: &mpsc::UnboundedSender<Request>) {
    match resp {
        Response::Live(live) => handle_live(app, live, req),
        Response::LiveErr(e) => app.status = trf!("rete: {}", e),
        Response::Hist { peer, rows } => {
            let is_open = app.current_chat().map(|c| c.peer_id) == Some(peer);
            let Ok(rows) = rows else {
                // storia vuota e storia guasta sono cose diverse: un errore
                // di rete non deve apparire come "nessun messaggio"
                if is_open {
                    app.status = trf!("cronologia: {}", rows.err().unwrap_or_default());
                }
                return;
            };
            app.push_rows(peer, rows);
            app.auto_translate(peer, crate::app::AUTO_TRANSLATE_LAST);
            if is_open {
                app.status = String::new();
                // la storia è arrivata e la chat è a fuoco: il badge
                // unread della barra sparisce, e anche il segno «da leggere»
                for d in app.contacts.iter_mut() {
                    if model::same_peer(d.peer_id, peer) {
                        d.unread = 0;
                        if std::mem::take(&mut d.unread_mark) {
                            let action = model::ChatAction::MarkUnread(false);
                            let _ = req.send(Request::ChatAction { peer_ref: d.peer_ref, peer, action });
                        }
                        break;
                    }
                }
                if let Some(chat) = app.current_chat() {
                    mark_read(req, chat.peer_ref);
                }
                let prefs = app.media_prefs;
                let medias: Vec<MediaRef> = app
                    .histories
                    .get(&peer)
                    .map(|ring| {
                        ring.iter()
                            .filter(|r| prefs.auto(&r.kind))
                            .filter_map(|r| crate::app::preview_media(&r.kind).cloned())
                            .collect()
                    })
                    .unwrap_or_default();
                app.ensure_previews(req, medias.iter());
            }
        }
        Response::HistOlder { peer, rows } => {
            app.older_busy.remove(&peer);
            let is_open = app.current_chat().map(|c| c.peer_id) == Some(peer);
            match rows {
                Ok(rows) => {
                    let got = rows.len();
                    let entered = app.prepend_rows(peer, rows);
                    // pagina corta = inizio della chat; ring pieno = tetto
                    if got < OLDER_PAGE || entered < got {
                        app.older_done.insert(peer);
                        if is_open {
                            app.status = tr("inizio della cronologia").into();
                        }
                    } else if is_open {
                        app.status.clear();
                    }
                }
                Err(e) => {
                    if is_open {
                        app.status = trf!("cronologia precedente: {}", e);
                    }
                }
            }
        }
        Response::Quoted { peer, rows } => {
            app.quoted_busy.remove(&peer);
            match rows {
                Ok(rows) => {
                    app.quoted_failed.remove(&peer);
                    app.quoted.entry(peer).or_default().extend(rows);
                }
                Err(e) => {
                    app.quoted_failed(peer);
                    if app.current_chat().map(|c| c.peer_id) == Some(peer) {
                        app.status = trf!("messaggi citati: {}", e);
                    }
                }
            }
        }
        Response::Sets(res) => match res {
            Ok(sets) => {
                let had_none = app.sets.is_none();
                app.sets = Some(sets);
                if had_none {
                    app.status = String::new();
                }
            }
            Err(err) => {
                // sets resta None: riaprendo il pannello con Tab ritenta
                app.status = trf!("set: {}", err);
            }
        },
        Response::Docs { set_id, docs } => match docs {
            Ok(docs) => {
                app.docs_fine(set_id);
                app.set_docs.insert(set_id, docs);
                if app.open_set == Some(set_id) && app.set_level {
                    app.status = String::new();
                }
            }
            Err(err) => {
                // senza la lista la griglia può ripartire al prossimo input:
                // non inserire il set in set_docs come "vuoto però caricato"
                app.docs_failed(set_id);
                if app.open_set == Some(set_id) {
                    app.status = trf!("set: {}", err);
                }
            }
        },
        Response::Converted((key, res)) => {
            app.pending.remove(&key);
            match res {
                Ok(path) => {
                    app.failed.remove(&key);
                    app.previews.insert(key, path);
                }
                Err(_why) => app.conversion_failed(key),
            }
        }
        Response::Sent(res) => {
            app.uploading = None;
            match res {
                Ok(SendDone::New(peer_id, row)) => {
                    app.set_last(peer_id, &row);
                    app.push_row(peer_id, row);
                    app.status = String::new();
                }
                Ok(SendDone::Edit(peer_id, row)) => {
                    app.replace_row(peer_id, row);
                    app.status = tr("modifica salvata").into();
                }
                Err(err) => app.status = trf!("invio: {}", err),
            }
        }
        Response::Deleted(res) => match res {
            Ok(()) => app.status = tr("cancellato").into(),
            Err(err) => app.status = trf!("cancellazione: {}", err),
        },
        Response::Searched(res) => match res {
            Ok(hits) => {
                let n = hits.len();
                app.search_results = Some(hits);
                app.search_cursor = 0;
                app.status = trf!("ricerca: {} risultati (↓ per muoverti, Esc chiude)", n);
            }
            Err(err) => app.status = trf!("ricerca: {}", err),
        },
        Response::Saved(res) => match res {
            Ok(path) => app.status = trf!("salvato in {}", path),
            Err(err) => app.status = trf!("salvataggio: {}", err),
        },
        Response::Played(res) => match res {
            Ok(label) => app.status = label,
            Err(err) => app.status = trf!("riproduzione: {}", err),
        },
        Response::Reacted(res) => match res {
            Ok(()) => app.status = tr("reazione salvata").into(),
            Err(err) => app.status = trf!("reazione: {}", err),
        },
        Response::Clipboard(hit) => handle_clipboard(app, hit),
        Response::Link(res) => match res {
            Ok(model::LinkHit::Chat { dialog, post }) => {
                let pid = dialog.peer_id;
                if !app.contacts.iter().any(|d| model::same_peer(d.peer_id, pid)) {
                    let pinned_len = app.contacts.iter().take_while(|c| c.pinned).count();
                    app.contacts.insert(pinned_len, dialog);
                }
                app.link_open = Some((pid, post));
                app.status.clear();
            }
            Ok(model::LinkHit::Already(pid)) => {
                if !app.contacts.iter().any(|d| model::same_peer(d.peer_id, pid)) {
                    let _ = req.send(Request::Dialogs);
                }
                app.link_open = Some((pid, None));
                app.status.clear();
            }
            Ok(model::LinkHit::Invite { hash, title, members }) => {
                app.invite = Some((hash, title, members));
                app.status.clear();
            }
            Ok(model::LinkHit::Joined(pid)) => {
                let _ = req.send(Request::Dialogs);
                app.link_open = pid.map(|p| (p, None));
                app.status = tr("sei entrato nella chat").into();
            }
            Err(err) => app.status = trf!("link: {}", err),
        },
        Response::Avatar { photo_id, path } => {
            // in caso di errore resta in avatar_asked: niente ritenti a raffica
            if let Ok(p) = path {
                app.avatars.insert(photo_id, p);
            }
        }
        Response::Forwarded(res) => match res {
            Ok(to) => app.status = trf!("inoltrato a {}", to),
            Err(err) => app.status = trf!("inoltro: {}", err),
        },
        Response::Info { peer, info } => match info {
            Ok(info) => {
                app.peer_info.insert(peer, info);
            }
            Err(err) => app.status = trf!("info: {}", err),
        },
        Response::Shared { peer, tab, items } => match items {
            Ok(items) => {
                let medias: Vec<MediaRef> = items
                    .iter()
                    .filter_map(|it| match &it.row.kind {
                        model::RowKind::Photo { media, .. } | model::RowKind::Video { media, .. } => {
                            Some(media.clone())
                        }
                        _ => None,
                    })
                    .collect();
                app.ensure_previews(req, medias.iter());
                app.shared.insert((peer, tab), items);
            }
            Err(err) => {
                // si potrà richiedere riaprendo la scheda
                app.shared_asked.remove(&(peer, tab));
                app.status = format!("{}: {err}", tab.label().to_lowercase());
            }
        },
        Response::Blocked { peer, res } => match res {
            Ok(blocked) => {
                if let Some(info) = app.peer_info.get_mut(&peer) {
                    info.blocked = blocked;
                }
                app.status = if blocked { tr("utente bloccato") } else { tr("utente sbloccato") }.into();
            }
            Err(err) => app.status = trf!("blocco: {}", err),
        },
        Response::ChatDeleted { peer, res } => match res {
            Ok(()) => {
                app.forget_chat(peer);
                app.status = tr("chat eliminata").into();
            }
            Err(err) => app.status = trf!("eliminazione chat: {}", err),
        },
        Response::Profile(res) => match res {
            Ok(p) => {
                app.me = format!("{} {}", p.first, p.last).trim().to_string();
                app.profile = Some(p);
            }
            Err(err) => app.status = trf!("profilo: {}", err),
        },
        Response::ProfileSaved(res) => match res {
            Ok(()) => {
                app.status = tr("profilo aggiornato").into();
                let _ = req.send(Request::Profile);
            }
            Err(err) => app.status = trf!("profilo: {}", err),
        },
        Response::Sessions(res) => match res {
            Ok(list) => app.sessions = Some(list),
            Err(err) => app.status = trf!("dispositivi: {}", err),
        },
        Response::SessionEnded(res) => match res {
            Ok(hash) => {
                if let Some(list) = app.sessions.as_mut() {
                    list.retain(|s| s.current || hash.is_some_and(|h| h != s.hash));
                }
                app.status = match hash {
                    Some(_) => tr("sessione chiusa").into(),
                    None => tr("tutte le altre sessioni sono state chiuse").into(),
                };
            }
            Err(err) => app.status = trf!("dispositivi: {}", err),
        },
        Response::Translated { peer, id, auto, target, res } => match res {
            Ok(t) if t.same_as(&target) => {
                app.translations.insert((peer, id), crate::app::TrState::Same);
                if !auto {
                    app.status = tr("il messaggio è già nella tua lingua").into();
                }
            }
            Ok(t) => {
                app.translations.insert((peer, id), crate::app::TrState::Shown(t));
            }
            Err(err) => {
                app.translations.remove(&(peer, id));
                app.status = trf!("traduzione: {}", err);
                if auto {
                    // niente raffiche di errori: la chat smette di tradursi
                    // da sola finché non la si riattiva
                    app.translate.set_auto(peer, false);
                }
            }
        },
        Response::ChatActionDone { peer, action, res } => match res {
            Ok(()) => {
                app.apply_chat_action(peer, action);
                app.status = match action {
                    model::ChatAction::Archive(true) => tr("chat archiviata"),
                    model::ChatAction::Archive(false) => tr("chat tolta dall'archivio"),
                    model::ChatAction::Pin(true) => tr("chat fissata in alto"),
                    model::ChatAction::Pin(false) => tr("chat non più fissata"),
                    model::ChatAction::Mute(true) => tr("notifiche silenziate"),
                    model::ChatAction::Mute(false) => tr("notifiche riattivate"),
                    model::ChatAction::MarkUnread(_) => "",
                    model::ChatAction::Ttl(0) => tr("messaggi effimeri disattivati"),
                    model::ChatAction::Ttl(_) => tr("messaggi effimeri attivati"),
                }
                .into();
            }
            Err(err) => app.status = trf!("azione sulla chat: {}", err),
        },
        Response::TranslateTest(res) => {
            app.tr_test = Some(res.map(|t| match t.source {
                Some(src) => format!("{} ({} → {})", t.text, src.to_uppercase(), app.translate.target_lang().to_uppercase()),
                None => t.text,
            }));
        }
        Response::Dialogs(res) => match res {
            Ok(mut list) => {
                crate::app::pinned_first(&mut list);
                let sel = app.contacts.get(app.contact_cursor).map(|c| c.peer_id);
                app.contacts = list;
                app.contact_cursor = sel
                    .and_then(|s| app.contacts.iter().position(|c| model::same_peer(c.peer_id, s)))
                    .unwrap_or(0);
            }
            Err(err) => app.status = trf!("lista chat: {}", err),
        },
    }
}
