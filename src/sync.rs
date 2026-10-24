//! Aggiornamenti di stato indipendenti dall'interfaccia: update Telegram e
//! risposte dei worker applicati ad `App`. Li usano sia la TUI sia la GUI.

use grammers_client::media::Media;
use grammers_client::session::types::{PeerId, PeerKind};
use tokio::sync::mpsc;

use crate::app::{App, OLDER_PAGE};
use crate::clipboard::handle_clipboard;
use crate::send::{Request, Response, SendDone};
use crate::tg;

/// Segna la chat come letta con la richiesta fuori dal flusso eventi:
/// lo spawn async parte subito ma l'input non aspetta la rete.
pub fn mark_read(client: &grammers_client::Client, peer_ref: tg::ChatRef) {
    let cl = client.clone();
    tokio::spawn(async move {
        let _ = cl.mark_as_read(peer_ref).await;
    });
}

/// «sta scrivendo»: Telegram lo ripete ogni ~5s finché dura, quindi
/// scade da solo dopo 6s; nei gruppi col nome di chi scrive.
fn typing(
    app: &mut App,
    chat: Option<PeerId>,
    who: Option<PeerId>,
    action: &grammers_client::tl::enums::SendMessageAction,
) {
    let Some(chat) = chat else { return };
    let Some(what) = tg::typing_label(action) else {
        app.typing.remove(&chat);
        return;
    };
    let label = match who {
        Some(w) => {
            let name = app
                .contacts
                .iter()
                .find(|d| tg::same_peer(d.peer_id, w))
                .map(|d| d.name.split_whitespace().next().unwrap_or("").to_string())
                .unwrap_or_else(|| "qualcuno".into());
            format!("{name} {what}")
        }
        None => what.to_string(),
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(6);
    app.typing.insert(chat, (until, label));
}

pub async fn handle_update(
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
            let fresh = if app.contacts.iter().any(|d| tg::same_peer(d.peer_id, peer_id)) {
                None
            } else {
                tg::dialog_from_message(client, &m).await
            };
            app.bump_chat(peer_id, fresh);
            // il messaggio è arrivato: non sta più scrivendo
            if !outgoing {
                app.typing.remove(&peer_id);
            }
            let row = tg::HistoryRow::from_message(&m, dialog_or(app, peer_id, tg::sender_name(&m)));
            app.set_last(peer_id, &row);
            let open_id = app.current_chat().map(|c| c.peer_id);
            if Some(peer_id) == open_id {
                app.push_row(peer_id, row);
                if !outgoing && app.read_mark_due() {
                    // la chat aperta è a fuoco: i nuovi arrivati sono letti
                    // (prima mark_as_read partiva solo al load della storia)
                    if let Some(chat) = app.current_chat() {
                        mark_read(client, chat.peer_ref);
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
            let row = tg::HistoryRow::from_message(&m, dialog_or(app, peer_id, tg::sender_name(&m)));
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
            // le spunte doppie: l'altro ha letto fino a max_id
            let read = |app: &mut App, pid: Option<PeerId>, max_id: i32| {
                if let Some(d) = pid.and_then(|p| {
                    app.contacts.iter_mut().find(|d| tg::same_peer(d.peer_id, p))
                }) {
                    d.read_out = d.read_out.max(max_id);
                }
            };
            match raw.raw {
                tl::enums::Update::MessageReactions(u) => {
                    let items = tg::reactions_from(&u.reactions);
                    app.apply_reactions(PeerId::from(u.peer), u.msg_id, items);
                }
                tl::enums::Update::ReadHistoryOutbox(u) => {
                    read(app, Some(PeerId::from(u.peer)), u.max_id)
                }
                tl::enums::Update::ReadChannelOutbox(u) => {
                    read(app, PeerId::channel(u.channel_id), u.max_id)
                }
                tl::enums::Update::UserStatus(u) => {
                    if let Some(d) = PeerId::user(u.user_id).and_then(|p| {
                        app.contacts.iter_mut().find(|d| tg::same_peer(d.peer_id, p))
                    }) {
                        d.seen = tg::Seen::from_raw(&u.status);
                    }
                }
                tl::enums::Update::UserTyping(u) => {
                    typing(app, PeerId::user(u.user_id), None, &u.action)
                }
                tl::enums::Update::ChatUserTyping(u) => {
                    typing(app, PeerId::chat(u.chat_id), Some(PeerId::from(u.from_id)), &u.action)
                }
                tl::enums::Update::ChannelUserTyping(u) => {
                    typing(app, PeerId::channel(u.channel_id), Some(PeerId::from(u.from_id)), &u.action)
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// L'intero ramo di risposte del ciclo eventi, estratto in funzione:
/// una ricezione sola sul canale (prima: dieci canali e un waiting_pick
/// che scartava a mano il primo che rispondeva).
pub async fn handle_response(
    app: &mut App,
    resp: Response,
    client: &grammers_client::Client,
    req: &mpsc::UnboundedSender<Request>,
) {
    match resp {
        Response::Hist { peer, rows } => {
            let is_open = app.current_chat().map(|c| c.peer_id) == Some(peer);
            let Ok(rows) = rows else {
                // storia vuota e storia guasta sono cose diverse: un errore
                // di rete non deve apparire come "nessun messaggio"
                if is_open {
                    app.status = format!("cronologia: {}", rows.err().unwrap_or_default());
                }
                return;
            };
            app.push_rows(peer, rows);
            if is_open {
                app.status = String::new();
                // la storia è arrivata e la chat è a fuoco: il badge
                // unread della barra sparisce
                for d in app.contacts.iter_mut() {
                    if tg::same_peer(d.peer_id, peer) {
                        d.unread = 0;
                        break;
                    }
                }
                if let Some(chat) = app.current_chat() {
                    mark_read(client, chat.peer_ref);
                }
                let medias: Vec<Media> = app
                    .histories
                    .get(&peer)
                    .map(|ring| {
                        ring.iter()
                            .filter_map(|r| match &r.kind {
                                tg::RowKind::Sticker { media, animated, .. } => {
                                    if *animated { None } else { Some(media.clone()) }
                                }
                                tg::RowKind::Photo { media, .. } => Some(media.clone()),
                                _ => None,
                            })
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
                            app.status = "inizio della cronologia".into();
                        }
                    } else if is_open {
                        app.status.clear();
                    }
                }
                Err(e) => {
                    if is_open {
                        app.status = format!("cronologia precedente: {e}");
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
                        app.status = format!("messaggi citati: {e}");
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
                app.status = format!("set: {err}");
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
                    app.status = format!("set: {err}");
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
        Response::Sent(res) => match res {
            Ok(SendDone::New(peer_id, row)) => {
                app.set_last(peer_id, &row);
                app.push_row(peer_id, row);
                app.status = String::new();
            }
            Ok(SendDone::Edit(peer_id, row)) => {
                app.replace_row(peer_id, row);
                app.status = "modifica salvata".into();
            }
            Err(err) => app.status = format!("invio: {err}"),
        },
        Response::Deleted(res) => match res {
            Ok(()) => app.status = "cancellato".into(),
            Err(err) => app.status = format!("cancellazione: {err}"),
        },
        Response::Searched(res) => match res {
            Ok(hits) => {
                let n = hits.len();
                app.search_results = Some(hits);
                app.search_cursor = 0;
                app.status = format!("ricerca: {n} risultati (↓ per muoverti, Esc chiude)");
            }
            Err(err) => app.status = format!("ricerca: {err}"),
        },
        Response::Saved(res) => match res {
            Ok(path) => app.status = format!("salvato in {path}"),
            Err(err) => app.status = format!("salvataggio: {err}"),
        },
        Response::Played(res) => match res {
            Ok(label) => app.status = label,
            Err(err) => app.status = format!("riproduzione: {err}"),
        },
        Response::Reacted(res) => match res {
            Ok(()) => app.status = "reazione salvata".into(),
            Err(err) => app.status = format!("reazione: {err}"),
        },
        Response::Clipboard(hit) => handle_clipboard(app, hit),
        Response::Link(res) => match res {
            Ok(tg::LinkHit::Chat { dialog, post }) => {
                let pid = dialog.peer_id;
                if !app.contacts.iter().any(|d| tg::same_peer(d.peer_id, pid)) {
                    let pinned_len = app.contacts.iter().take_while(|c| c.pinned).count();
                    app.contacts.insert(pinned_len, dialog);
                }
                app.link_open = Some((pid, post));
                app.status.clear();
            }
            Ok(tg::LinkHit::Already(pid)) => {
                if !app.contacts.iter().any(|d| tg::same_peer(d.peer_id, pid)) {
                    let _ = req.send(Request::Dialogs);
                }
                app.link_open = Some((pid, None));
                app.status.clear();
            }
            Ok(tg::LinkHit::Invite { hash, title, members }) => {
                app.invite = Some((hash, title, members));
                app.status.clear();
            }
            Ok(tg::LinkHit::Joined(pid)) => {
                let _ = req.send(Request::Dialogs);
                app.link_open = pid.map(|p| (p, None));
                app.status = "sei entrato nella chat".into();
            }
            Err(err) => app.status = format!("link: {err}"),
        },
        Response::Avatar { photo_id, path } => {
            // in caso di errore resta in avatar_asked: niente ritenti a raffica
            if let Ok(p) = path {
                app.avatars.insert(photo_id, p);
            }
        }
        Response::Dialogs(res) => match res {
            Ok(mut list) => {
                crate::app::pinned_first(&mut list);
                let sel = app.contacts.get(app.contact_cursor).map(|c| c.peer_id);
                app.contacts = list;
                app.contact_cursor = sel
                    .and_then(|s| app.contacts.iter().position(|c| tg::same_peer(c.peer_id, s)))
                    .unwrap_or(0);
            }
            Err(err) => app.status = format!("lista chat: {err}"),
        },
    }
}
