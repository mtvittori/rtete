//! Il dispatcher WhatsApp: le richieste dell'app eseguite sull'archivio
//! locale (cronologia, ricerca) o sul server (invii, letture, media).

use std::sync::Arc;

use tokio::sync::mpsc;
use whatsapp_rust::prelude::*;
use whatsapp_rust::send::RevokeType;

use super::store::{chat_id_of, StoredMsg};
use super::{download_to, fetch_raw, now_secs, row_of, stored_from, upload_message, Wa};
use crate::app::{MAX_HISTORY, OLDER_PAGE, SHARED_PAGE};
use crate::i18n::tr;
use crate::model::{ChatRef, HistoryRow, PeerInfo, SearchHit, SharedItem, SharedTab};
use crate::send::{Request, Response, SendDone, SendJob, SendKind};
use crate::worker::{conv_job, save_job, translate_job, translate_test_job, Player};

/// Le funzioni che WhatsApp non ha (o che il prototipo non fa ancora).
fn unsupported<T>() -> Result<T, String> {
    Err(tr("non disponibile su WhatsApp").into())
}

impl Wa {
    fn jid(&self, chat: ChatRef) -> Result<String, String> {
        self.store()
            .jid_of(chat.id)
            .map(str::to_string)
            .ok_or_else(|| tr("chat sconosciuta").into())
    }

    fn rows(&self, jid: &str, msgs: Vec<StoredMsg>) -> Vec<HistoryRow> {
        let mut store = self.store();
        msgs.iter().map(|m| row_of(&mut store, jid, m)).collect()
    }

    /// Salva un messaggio mio appena inviato e lo restituisce come riga.
    fn keep_sent(&self, jid: &str, msg: &wa::Message, wa_id: &str) -> Result<SendDone, String> {
        let stored = stored_from(msg, wa_id, true, "", "", now_secs()).ok_or("messaggio vuoto")?;
        let mut store = self.store();
        let group = jid.ends_with("@g.us");
        let fresh = store.insert(jid, group, vec![stored]).map_err(|e| e.to_string())?;
        let _ = store.save_meta();
        let m = match fresh.into_iter().next() {
            Some(m) => m,
            // già arrivato come eco dal server
            None => store.by_wa(jid, wa_id).ok_or("messaggio perso")?,
        };
        Ok(SendDone::New(chat_id_of(jid), row_of(&mut store, jid, &m)))
    }

    async fn send(&self, job: SendJob) -> Result<SendDone, String> {
        let jid = self.jid(job.peer)?;
        let to: Jid = jid.parse().map_err(|e| format!("{e:?}"))?;
        // la risposta: id e mittente del messaggio citato
        let quote = job.reply_to.and_then(|id| self.store().by_id(&jid, id));
        let context = quote.map(|q| wa::ContextInfo {
            stanza_id: Some(q.wa.clone()),
            participant: Some(if q.me { self.client.pn().map(|j| j.to_non_ad_string()).unwrap_or_default() } else if q.sender.is_empty() { jid.clone() } else { q.sender.clone() }),
            quoted_message: MessageField::some(wa::Message { conversation: Some(q.text.clone()), ..Default::default() }),
            ..Default::default()
        });
        match job.kind {
            SendKind::Text(text) => {
                let msg = match context {
                    Some(ctx) => wa::Message::text_with_context(text, ctx),
                    None => wa::Message::text(text),
                };
                let sent = self.client.send_message(to, msg.clone()).await.map_err(|e| e.to_string())?;
                self.keep_sent(&jid, &msg, &sent.message_id)
            }
            SendKind::Photo { path, caption } | SendKind::File { path, caption } => {
                let p = std::path::Path::new(&path);
                let mut msg = upload_message(&self.client, p, &caption).await?;
                if let Some(ctx) = context {
                    msg.set_context_info(ctx);
                }
                let sent = self.client.send_message(to, msg.clone()).await.map_err(|e| e.to_string())?;
                // la foto degli appunti è un temporaneo: inviata, non serve più
                if path.starts_with(&*crate::paths::tmp_dir().to_string_lossy()) {
                    let _ = std::fs::remove_file(&path);
                }
                self.keep_sent(&jid, &msg, &sent.message_id)
            }
            SendKind::Sticker(_) => unsupported(),
            SendKind::EditText { id, text } => {
                let target = self.store().by_id(&jid, id).ok_or("messaggio sconosciuto")?;
                self.client
                    .edit_message(to, target.wa.clone(), wa::Message::text(text.clone()))
                    .await
                    .map_err(|e| e.to_string())?;
                let mut store = self.store();
                let m = store
                    .update(&jid, &target.wa, |m| {
                        m.text = text;
                        m.edited = true;
                    })
                    .map_err(|e| e.to_string())?
                    .ok_or("messaggio sconosciuto")?;
                Ok(SendDone::Edit(chat_id_of(&jid), row_of(&mut store, &jid, &m)))
            }
        }
    }

    async fn delete(&self, chat: ChatRef, id: i32) -> Result<(), String> {
        let jid = self.jid(chat)?;
        let target = self.store().by_id(&jid, id).ok_or("messaggio sconosciuto")?;
        if !target.me {
            return Err(tr("su WhatsApp si cancellano solo i propri messaggi").into());
        }
        let to: Jid = jid.parse().map_err(|e| format!("{e:?}"))?;
        self.client
            .revoke_message(to, target.wa.clone(), RevokeType::Sender)
            .await
            .map_err(|e| e.to_string())?;
        let mut store = self.store();
        store.update(&jid, &target.wa, |m| m.deleted = true).map_err(|e| e.to_string())?;
        let _ = store.save_meta();
        Ok(())
    }

    async fn react(&self, chat: ChatRef, id: i32, emoji: Option<String>) -> Result<(), String> {
        let jid = self.jid(chat)?;
        let target = self.store().by_id(&jid, id).ok_or("messaggio sconosciuto")?;
        let to: Jid = jid.parse().map_err(|e| format!("{e:?}"))?;
        let key = wa::MessageKey {
            remote_jid: Some(jid.clone()),
            from_me: Some(target.me),
            id: Some(target.wa.clone()),
            participant: (!target.me && jid.ends_with("@g.us")).then(|| target.sender.clone()),
        };
        let emoji = emoji.unwrap_or_default();
        self.client.send_reaction(to, key, &emoji).await.map_err(|e| e.to_string())?;
        let mut store = self.store();
        store
            .update(&jid, &target.wa, |m| {
                m.reactions.retain(|(w, _)| w != "me");
                if !emoji.is_empty() {
                    m.reactions.push(("me".into(), emoji));
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Segna letti i messaggi in arrivo della chat (ricevute blu).
    async fn mark_read(&self, chat: ChatRef) -> Result<(), String> {
        let jid = self.jid(chat)?;
        let tail = {
            let mut store = self.store();
            let tail = store.unread_tail(&jid);
            if let Some(c) = store.chat_mut(&jid) {
                c.unread = 0;
            }
            let _ = store.save_meta();
            tail
        };
        let to: Jid = jid.parse().map_err(|e| format!("{e:?}"))?;
        // nei gruppi le ricevute vanno per mittente
        let mut by_sender: Vec<(String, Vec<String>)> = Vec::new();
        for m in tail {
            let who = if jid.ends_with("@g.us") { m.sender } else { String::new() };
            match by_sender.iter_mut().find(|(s, _)| *s == who) {
                Some((_, ids)) => ids.push(m.wa),
                None => by_sender.push((who, vec![m.wa])),
            }
        }
        for (who, ids) in by_sender {
            let sender: Option<Jid> = (!who.is_empty()).then(|| who.parse().ok()).flatten();
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            self.client
                .mark_as_read(&to, sender.as_ref(), &ids)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn info(&self, chat: ChatRef) -> Result<PeerInfo, String> {
        let jid = self.jid(chat)?;
        let store = self.store();
        let group = store.chat(&jid).is_some_and(|c| c.group);
        let user = jid.split('@').next().unwrap_or("");
        Ok(PeerInfo {
            phone: (!group && jid.ends_with("@s.whatsapp.net")).then(|| format!("+{user}")),
            me: self.client.pn().is_some_and(|p| p.to_non_ad_string() == jid),
            ..Default::default()
        })
    }

    fn search(&self, chat: ChatRef, query: &str) -> Result<Vec<SearchHit>, String> {
        let jid = self.jid(chat)?;
        let mut store = self.store();
        let hits = store.search(&jid, query, 40);
        Ok(hits
            .into_iter()
            .map(|m| SearchHit {
                outgoing: m.me,
                from: if m.me { String::new() } else { store.display_name(if m.sender.is_empty() { &jid } else { &m.sender }) },
                date: jiff::Timestamp::from_second(m.ts)
                    .map(|t| t.to_string().replace('T', " ").chars().take(16).collect())
                    .unwrap_or_default(),
                snippet: crate::model::caption(&m.text),
            })
            .collect())
    }

    fn shared(&self, chat: ChatRef, tab: SharedTab) -> Result<Vec<SharedItem>, String> {
        let jid = self.jid(chat)?;
        let mut store = self.store();
        let kinds: &[&str] = match tab {
            SharedTab::Media => &["image", "video"],
            SharedTab::Files => &["doc"],
            SharedTab::Audio => &["audio"],
            SharedTab::Voice => &["voice"],
            SharedTab::Links => &["text"],
        };
        let picked: Vec<StoredMsg> = store
            .messages(&jid)
            .iter()
            .rev()
            .filter(|m| !m.deleted && kinds.contains(&m.kind.as_str()))
            .filter(|m| tab != SharedTab::Links || m.text.contains("http"))
            .take(SHARED_PAGE)
            .cloned()
            .collect();
        Ok(picked
            .iter()
            .map(|m| SharedItem {
                links: if tab == SharedTab::Links {
                    m.text
                        .split_whitespace()
                        .filter(|w| w.starts_with("http://") || w.starts_with("https://"))
                        .map(str::to_string)
                        .collect()
                } else {
                    Vec::new()
                },
                row: row_of(&mut store, &jid, m),
            })
            .collect())
    }
}

/// Dispatcher: un task per job, come per Telegram; le conversioni delle
/// anteprime al massimo due alla volta.
pub fn run_worker(wa: Wa, mut req_rx: mpsc::UnboundedReceiver<Request>, out: mpsc::UnboundedSender<Response>) {
    let conv_permits = Arc::new(tokio::sync::Semaphore::new(2));
    let tr_permits = Arc::new(tokio::sync::Semaphore::new(3));
    tokio::spawn(async move {
        let mut player = Player::default();
        while let Some(req) = req_rx.recv().await {
            let wa = wa.clone();
            let out = out.clone();
            match req {
                Request::Hist { peer_ref, peer } => {
                    let rows = wa.jid(peer_ref).map(|jid| {
                        let msgs = wa.store().last(&jid, MAX_HISTORY);
                        wa.rows(&jid, msgs)
                    });
                    let _ = out.send(Response::Hist { peer, rows });
                }
                Request::HistOlder { peer_ref, peer, before } => {
                    let rows = wa.jid(peer_ref).map(|jid| {
                        let msgs = wa.store().before(&jid, before, OLDER_PAGE);
                        wa.rows(&jid, msgs)
                    });
                    let _ = out.send(Response::HistOlder { peer, rows });
                }
                Request::Quoted { peer_ref, peer, ids } => {
                    let rows = wa.jid(peer_ref).map(|jid| {
                        ids.iter()
                            .map(|id| {
                                let m = wa.store().by_id(&jid, *id);
                                (*id, m.map(|m| row_of(&mut wa.store(), &jid, &m)))
                            })
                            .collect()
                    });
                    let _ = out.send(Response::Quoted { peer, rows });
                }
                Request::MarkRead(peer_ref) => {
                    tokio::spawn(async move {
                        if let Err(e) = wa.mark_read(peer_ref).await {
                            let _ = out.send(Response::LiveErr(crate::trf!("letture: {}", e)));
                        }
                    });
                }
                Request::Send(job) => {
                    tokio::spawn(async move {
                        let _ = out.send(Response::Sent(wa.send(job).await));
                    });
                }
                Request::Del { peer_ref, id } => {
                    tokio::spawn(async move {
                        let res = wa.delete(peer_ref, id).await;
                        let ok = res.is_ok();
                        let _ = out.send(Response::Deleted(res));
                        // la riga resta, con «messaggio eliminato»
                        if ok {
                            if let Ok(jid) = wa.jid(peer_ref) {
                                let m = wa.store().by_id(&jid, id);
                                if let Some(m) = m {
                                    let row = row_of(&mut wa.store(), &jid, &m);
                                    let ev = crate::model::Live::Edited { chat: peer_ref.id, row };
                                    let _ = out.send(Response::Live(ev));
                                }
                            }
                        }
                    });
                }
                Request::React { peer_ref, msg_id, emoticon } => {
                    tokio::spawn(async move {
                        let _ = out.send(Response::Reacted(wa.react(peer_ref, msg_id, emoticon).await));
                    });
                }
                Request::Search { peer_ref, query } => {
                    let _ = out.send(Response::Searched(wa.search(peer_ref, &query)));
                }
                Request::Shared { peer_ref, peer, tab } => {
                    let _ = out.send(Response::Shared { peer, tab, items: wa.shared(peer_ref, tab) });
                }
                Request::Info { peer_ref, peer } => {
                    let _ = out.send(Response::Info { peer, info: wa.info(peer_ref) });
                }
                Request::Dialogs => {
                    let _ = out.send(Response::Dialogs(Ok(wa.store().dialogs())));
                }
                Request::Conv(media) => {
                    let permits = Arc::clone(&conv_permits);
                    tokio::spawn(async move {
                        let _gate = permits.acquire_owned().await;
                        let key = media.key().to_string();
                        conv_job(key, fetch_raw(&wa.client, &media), &out).await;
                    });
                }
                Request::Save { media, name } => {
                    tokio::spawn(async move {
                        let res = save_job(name, |tmp| download_to(&wa.client, &media, tmp)).await;
                        let _ = out.send(Response::Saved(res));
                    });
                }
                Request::Play { media, fname, label, video } => {
                    player.play(fname, label, video, out, move |target| async move {
                        download_to(&wa.client, &media, target).await
                    });
                }
                Request::Translate { peer, id, text, cfg, auto } => {
                    translate_job(&tr_permits, peer, id, text, cfg, auto, out);
                }
                Request::TranslateTest(cfg) => translate_test_job(cfg, out),
                // niente set di sticker sul server: il pannello resta vuoto
                Request::Sets => {
                    let _ = out.send(Response::Sets(Ok(Vec::new())));
                }
                Request::SetDocs { set_id, .. } => {
                    let _ = out.send(Response::Docs { set_id, docs: Ok(Vec::new()) });
                }
                Request::Avatar { photo_id, .. } => {
                    let _ = out.send(Response::Avatar { photo_id, path: unsupported() });
                }
                Request::Profile => {
                    let me = wa.client.push_name();
                    let phone = wa.client.pn().map(|p| format!("+{}", p.user)).unwrap_or_default();
                    let _ = out.send(Response::Profile(Ok(crate::model::MyProfile { first: me, phone, ..Default::default() })));
                }
                Request::Link(_) | Request::Join(_) => {
                    let _ = out.send(Response::Link(unsupported()));
                }
                Request::Forward { .. } => {
                    let _ = out.send(Response::Forwarded(unsupported()));
                }
                Request::Block { peer, .. } => {
                    let _ = out.send(Response::Blocked { peer, res: unsupported() });
                }
                Request::DeleteChat { peer, .. } => {
                    let _ = out.send(Response::ChatDeleted { peer, res: unsupported() });
                }
                Request::SaveProfile { .. } | Request::ProfilePhoto(_) => {
                    let _ = out.send(Response::ProfileSaved(unsupported()));
                }
                Request::Sessions => {
                    let _ = out.send(Response::Sessions(unsupported()));
                }
                Request::EndSession(_) => {
                    let _ = out.send(Response::SessionEnded(unsupported()));
                }
            }
        }
    });
}
