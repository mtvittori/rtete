//! Il dispatcher Telegram: un task per job di rete, conversioni limitate da
//! un semaforo, player audio.

use std::sync::Arc;

use grammers_client::{tl, Client};
use tokio::sync::mpsc;

use super::{row_from_message, sender_name, to_chat_id, to_peer_ref};
use crate::app::{MAX_HISTORY, OLDER_PAGE, SHARED_PAGE};
use crate::model::{ChatId, HistoryRow, RowKind};
use crate::send::{Request, Response, SendDone, SendJob, SendKind};
use crate::worker::{conv_job, save_job, translate_job, translate_test_job, Player};

pub fn row_from_msg(m: grammers_client::message::Message) -> Result<(ChatId, HistoryRow), String> {
    let peer_id = to_chat_id(m.peer_id());
    let from = sender_name(&m);
    let mut row = row_from_message(&m, from);
    row.outgoing = true;
    Ok((peer_id, row))
}

/// Un invio testi/foto/sticker o l'edit di un messaggio (era il vecchio
/// worker: solo estratto in funzione, il dispatcher lo esegue per job).
pub async fn send_job(client: &Client, job: SendJob) -> Result<SendDone, String> {
    match job.kind {
        SendKind::Text(text) => match super::send_text(client, job.peer, &text, job.reply_to).await {
            Ok(m) => row_from_msg(m).map(|(pid, row)| SendDone::New(pid, row)),
            Err(e) => Err(e.to_string()),
        },
        SendKind::Sticker(media) => super::send_sticker(client, job.peer, &media, job.reply_to)
            .await
            .and_then(|m| row_from_msg(m).map(|(pid, row)| SendDone::New(pid, row))),
        SendKind::Photo { path, caption } => {
            match super::send_photo(
                client,
                job.peer,
                std::path::Path::new(&path),
                &caption,
                job.reply_to,
            )
            .await {
                Ok(m) => {
                    // l'upload chiude il job: il temporaneo non serve più
                    let _ = std::fs::remove_file(&path);
                    row_from_msg(m).map(|(pid, row)| SendDone::New(pid, row))
                }
                Err(e) => Err(e.to_string()),
            }
        }
        SendKind::File { path, caption } => {
            let path = std::path::Path::new(&path);
            let sent = if crate::model::is_photo_file(path) {
                super::send_photo(client, job.peer, path, &caption, job.reply_to).await
            } else {
                super::send_document(client, job.peer, path, &caption, job.reply_to).await
            };
            sent.and_then(|m| row_from_msg(m).map(|(pid, row)| SendDone::New(pid, row)))
        }
        SendKind::EditText { id, text } => {
            match super::edit_text(client, job.peer, id, &text).await {
                Ok(()) => Ok(SendDone::Edit(
                    job.peer_id,
                    HistoryRow {
                        id,
                        outgoing: true,
                        from: String::new(),
                        kind: RowKind::Text(text),
                        reactions: Vec::new(),
                        date: 0,
                        reply_to: None,
                        reply_quote: None,
                        fwd: None,
                        edited: true,
                    },
                )),
                Err(e) => Err(e.to_string()),
            }
        }
    }
}

/// Avvia la sessione Telegram: il dispatcher delle richieste e lo stream
/// degli update live, che arrivano sullo stesso canale delle risposte.
pub async fn start(
    client: Client,
    updates: mpsc::Receiver<grammers_client::session::updates::UpdatesLike>,
    req_rx: mpsc::UnboundedReceiver<Request>,
    res_tx: mpsc::UnboundedSender<Response>,
) -> Result<(), String> {
    run_worker(client.clone(), req_rx, res_tx.clone());
    super::live::run(client, updates, res_tx).await
}

/// Dispatcher: un task per job (i lavori di rete vanno in parallelo);
/// la sola conversione resta limitata a due in trasmissione con un
/// semaforo, come i due vecchi worker (RAM piatta).
pub fn run_worker(
    client: Client,
    mut req_rx: mpsc::UnboundedReceiver<Request>,
    res_tx: mpsc::UnboundedSender<Response>,
) {
    let cl = client;
    let out = res_tx;
    let conv_permits = Arc::new(tokio::sync::Semaphore::new(2));
    // traduzioni: poche alla volta, per non sbattere sui limiti del servizio
    let tr_permits = Arc::new(tokio::sync::Semaphore::new(3));
    tokio::spawn(async move {
        let mut player = Player::default();
        while let Some(req) = req_rx.recv().await {
            let cl = cl.clone();
            let out = out.clone();
            let permits = Arc::clone(&conv_permits);
            match req {
                Request::Conv(media) => {
                    tokio::spawn(async move {
                        let _gate = permits.acquire_owned().await;
                        let key = media.key().to_string();
                        conv_job(key, super::fetch_raw(&cl, &media), &out).await;
                    });
                }
                Request::Hist { peer_ref, peer } => {
                    tokio::spawn(async move {
                        // l'errore viaggia col canale: un guasto di rete non
                        // è storia vuota ("chat senza messaggi")
                        let rows = super::load_history(&cl, peer_ref, MAX_HISTORY)
                            .await
                            .map_err(|e| e.to_string());
                        let _ = out.send(Response::Hist { peer, rows });
                    });
                }
                Request::HistOlder { peer_ref, peer, before } => {
                    tokio::spawn(async move {
                        let rows = super::load_history_before(&cl, peer_ref, before, OLDER_PAGE)
                            .await
                            .map_err(|e| e.to_string());
                        let _ = out.send(Response::HistOlder { peer, rows });
                    });
                }
                Request::Quoted { peer_ref, peer, ids } => {
                    tokio::spawn(async move {
                        let rows = super::load_by_id(&cl, peer_ref, &ids)
                            .await
                            .map(|found| ids.iter().copied().zip(found).collect())
                            .map_err(|e| e.to_string());
                        let _ = out.send(Response::Quoted { peer, rows });
                    });
                }
                Request::MarkRead(peer_ref) => {
                    // fuori dal flusso eventi: l'input non aspetta la rete
                    tokio::spawn(async move {
                        let _ = cl.mark_as_read(to_peer_ref(peer_ref)).await;
                    });
                }
                // online finché la finestra è davanti, poi «ultimo accesso»
                Request::Presence(on) => {
                    tokio::spawn(async move {
                        let _ = cl.invoke(&tl::functions::account::UpdateStatus { offline: !on }).await;
                    });
                }
                // «sta scrivendo»: dura ~6 s sul server, l'app lo ripete
                Request::Typing { peer_ref, on } => {
                    tokio::spawn(async move {
                        let action = if on {
                            tl::enums::SendMessageAction::SendMessageTypingAction
                        } else {
                            tl::enums::SendMessageAction::SendMessageCancelAction
                        };
                        let peer = to_peer_ref(peer_ref).into();
                        let _ = cl.invoke(&tl::functions::messages::SetTyping { peer, top_msg_id: None, action }).await;
                    });
                }
                Request::Sets => {
                    tokio::spawn(async move {
                        // se fallisce resta None: riaprire con Tab ritenta,
                        // invece di servire per sempre una finta lista vuota
                        let sets = super::fetch_installed_sets(&cl)
                            .await
                            .map_err(|e| e.to_string());
                        let _ = out.send(Response::Sets(sets));
                    });
                }
                Request::SetDocs { set_id, short } => {
                    tokio::spawn(async move {
                        // un errore NON marca il set come caricato: la
                        // griglia riparte al prossimo input, dopo il backoff
                        let docs = if set_id < 0 {
                            super::fetch_special_docs(&cl, set_id).await
                        } else {
                            super::fetch_set_docs_by_name(&cl, &short).await
                        };
                        let docs = docs
                            .map(|docs| {
                                docs.into_iter()
                                    .filter_map(super::media_from_doc)
                                    .map(super::to_media_ref)
                                    .collect()
                            })
                            .map_err(|e| e.to_string());
                        let _ = out.send(Response::Docs { set_id, docs });
                    });
                }
                Request::Send(job) => {
                    tokio::spawn(async move {
                        let res = send_job(&cl, job).await;
                        let _ = out.send(Response::Sent(res));
                    });
                }
                Request::Del { peer_ref, id } => {
                    tokio::spawn(async move {
                        let res = super::delete_messages(&cl, peer_ref, &[id])
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string());
                        let _ = out.send(Response::Deleted(res));
                    });
                }
                Request::Search { peer_ref, query } => {
                    tokio::spawn(async move {
                        let res = super::search_chat(&cl, peer_ref, &query, 40)
                            .await
                            .map_err(|e| e.to_string());
                        let _ = out.send(Response::Searched(res));
                    });
                }
                Request::React { peer_ref, msg_id, emoticon } => {
                    tokio::spawn(async move {
                        let res = super::send_reaction(&cl, peer_ref, msg_id, emoticon.as_deref())
                            .await
                            .map_err(|e| e.to_string());
                        let _ = out.send(Response::Reacted(res));
                    });
                }
                Request::Link(link) => {
                    tokio::spawn(async move {
                        let _ = out.send(Response::Link(super::resolve_link(&cl, link).await));
                    });
                }
                Request::Join(hash) => {
                    tokio::spawn(async move {
                        let _ = out.send(Response::Link(super::join_invite(&cl, hash).await));
                    });
                }
                Request::Avatar { peer_ref, photo_id } => {
                    tokio::spawn(async move {
                        // stesso semaforo delle conversioni: all'avvio partono
                        // tutte insieme e non devono intasare la rete
                        let _gate = permits.acquire_owned().await;
                        let path = super::download_avatar(&cl, peer_ref, photo_id).await;
                        let _ = out.send(Response::Avatar { photo_id, path });
                    });
                }
                Request::Info { peer_ref, peer } => {
                    tokio::spawn(async move {
                        let info = super::peer_info(&cl, peer_ref).await;
                        let _ = out.send(Response::Info { peer, info });
                    });
                }
                Request::Shared { peer_ref, peer, tab } => {
                    tokio::spawn(async move {
                        let items = super::shared_media(&cl, peer_ref, tab, SHARED_PAGE)
                            .await
                            .map_err(|e| e.to_string());
                        let _ = out.send(Response::Shared { peer, tab, items });
                    });
                }
                Request::Block { peer_ref, peer, blocked } => {
                    tokio::spawn(async move {
                        let res = super::set_blocked(&cl, peer_ref, blocked)
                            .await
                            .map(|()| blocked)
                            .map_err(|e| e.to_string());
                        let _ = out.send(Response::Blocked { peer, res });
                    });
                }
                // Telegram non ha la stella: resta nell'elenco locale
                Request::Star { .. } => {}
                Request::ChatAction { peer_ref, peer, action } => {
                    tokio::spawn(async move {
                        let res = super::chat_action(&cl, peer_ref, action).await.map_err(|e| e.to_string());
                        let _ = out.send(Response::ChatActionDone { peer, action, res });
                    });
                }
                Request::DeleteChat { peer_ref, peer, revoke } => {
                    tokio::spawn(async move {
                        let res = super::delete_chat(&cl, peer_ref, revoke)
                            .await
                            .map_err(|e| e.to_string());
                        let _ = out.send(Response::ChatDeleted { peer, res });
                    });
                }
                Request::Profile => {
                    tokio::spawn(async move {
                        let _ = out.send(Response::Profile(super::my_profile(&cl).await));
                    });
                }
                Request::SaveProfile { profile, old_username } => {
                    tokio::spawn(async move {
                        let res = super::save_profile(&cl, &profile, &old_username).await;
                        let _ = out.send(Response::ProfileSaved(res));
                    });
                }
                Request::ProfilePhoto(path) => {
                    tokio::spawn(async move {
                        let res = super::set_profile_photo(&cl, &path).await;
                        let _ = out.send(Response::ProfileSaved(res));
                    });
                }
                Request::Sessions => {
                    tokio::spawn(async move {
                        let _ = out.send(Response::Sessions(super::sessions(&cl).await));
                    });
                }
                Request::EndSession(hash) => {
                    tokio::spawn(async move {
                        let res = super::end_session(&cl, hash).await.map(|()| hash);
                        let _ = out.send(Response::SessionEnded(res));
                    });
                }
                Request::Translate { peer, id, text, cfg, auto } => {
                    translate_job(&tr_permits, peer, id, text, cfg, auto, out);
                }
                Request::TranslateTest(cfg) => translate_test_job(cfg, out),
                Request::Dialogs => {
                    tokio::spawn(async move {
                        let res = super::load_dialogs(&cl).await.map_err(|e| e.to_string());
                        let _ = out.send(Response::Dialogs(res));
                    });
                }
                Request::Save { media, name } => {
                    tokio::spawn(async move {
                        let res = save_job(name, |tmp| super::download_media_to(&cl, &media, tmp)).await;
                        let _ = out.send(Response::Saved(res));
                    });
                }
                Request::Forward { from, id, to, to_name } => {
                    tokio::spawn(async move {
                        let res = cl
                            .forward_messages(to_peer_ref(to), &[id], to_peer_ref(from))
                            .await
                            .map(|_| to_name)
                            .map_err(|e| e.to_string());
                        let _ = out.send(Response::Forwarded(res));
                    });
                }
                Request::Play { media, fname, label, video } => {
                    player.play(fname, label, video, out, move |target| async move {
                        super::download_media_to(&cl, &media, target).await
                    });
                }
            }
        }
    });
}
