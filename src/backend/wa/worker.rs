//! Il dispatcher WhatsApp: le richieste dell'app eseguite sull'archivio
//! locale (cronologia, ricerca) o sul server (invii, letture, media).

use std::sync::Arc;

use tokio::sync::mpsc;
use whatsapp_rust::prelude::*;
use whatsapp_rust::send::RevokeType;

use super::store::{chat_id_of, StoredMsg};
use super::{download_to, fetch_raw, media_ref, now_secs, row_of, stored_from, upload_message, Wa, WaMedia};
use crate::app::{MAX_HISTORY, OLDER_PAGE, SHARED_PAGE};
use crate::i18n::tr;
use crate::model::{ChatAction, ChatRef, HistoryRow, PeerInfo, SearchHit, SharedItem, SharedTab};
use crate::send::{Request, Response, SendDone, SendJob, SendKind};
use crate::worker::{conv_job, save_job, translate_job, translate_test_job, Player};

/// Le funzioni che WhatsApp non ha (o che il prototipo non fa ancora).
fn unsupported<T>() -> Result<T, String> {
    Err(tr("non disponibile su WhatsApp").into())
}

impl Wa {
    /// La foto profilo (anteprima) di una chat: chiesta a WhatsApp,
    /// scaricata dal CDN e messa nella cache col suo id.
    async fn avatar(&self, chat: ChatRef) -> Result<std::path::PathBuf, String> {
        let jid_s = self.jid(chat)?;
        let jid: Jid = jid_s.parse().map_err(|e| format!("{e:?}"))?;
        let pic = self.client.contacts().get_profile_picture(&jid, true).await.map_err(|e| e.to_string())?;
        let set_pic = |id: &str| {
            let mut store = self.store();
            if let Some(c) = store.chat_mut(&jid_s) {
                c.pic = id.to_string();
            }
            let _ = store.save_meta();
        };
        let Some(pic) = pic else {
            set_pic("-");
            return Err(tr("nessuna foto").into());
        };
        let id = super::store::avatar_id(&jid_s, &pic.id).ok_or("id")?;
        let path = crate::paths::avatar_path(id);
        if !path.exists() {
            let url = pic.url.clone();
            let body = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
                let mut res = ureq::get(&url).call().map_err(|e| e.to_string())?;
                res.body_mut().with_config().limit(4 << 20).read_to_vec().map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| e.to_string())??;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, body).map_err(|e| e.to_string())?;
        }
        set_pic(&pic.id);
        Ok(path)
    }

    fn jid(&self, chat: ChatRef) -> Result<String, String> {
        self.store()
            .jid_of(chat.id)
            .map(str::to_string)
            .ok_or_else(|| tr("chat sconosciuta").into())
    }

    /// Archivia, fissa, silenzia, segna da leggere, timer effimero: sul
    /// server (sincronizzato con il telefono) e nell'archivio locale.
    async fn chat_action(&self, chat: ChatRef, action: ChatAction) -> Result<(), String> {
        let jid_s = self.jid(chat)?;
        let jid: Jid = jid_s.parse().map_err(|e| format!("{e:?}"))?;
        let acts = self.client.chat_actions();
        let res = match action {
            ChatAction::Archive(true) => acts.archive_chat(&jid, None).await,
            ChatAction::Archive(false) => acts.unarchive_chat(&jid, None).await,
            ChatAction::Pin(true) => acts.pin_chat(&jid).await,
            ChatAction::Pin(false) => acts.unpin_chat(&jid).await,
            ChatAction::Mute(true) => acts.mute_chat(&jid).await,
            ChatAction::Mute(false) => acts.unmute_chat(&jid).await,
            ChatAction::MarkUnread(unread) => acts.mark_chat_as_read(&jid, !unread, None).await,
            ChatAction::Ttl(secs) => {
                let secs = secs.max(0) as u32;
                return if jid_s.ends_with("@g.us") {
                    self.client.groups().set_ephemeral(jid, secs).await.map_err(|e| e.to_string())
                } else {
                    self.client.set_chat_disappearing_timer(jid, secs).await.map(drop).map_err(|e| e.to_string())
                };
            }
        };
        res.map_err(|e| e.to_string())?;
        let mut store = self.store();
        if let Some(c) = store.chat_mut(&jid_s) {
            match action {
                ChatAction::Archive(on) => c.archived = on,
                ChatAction::Pin(on) => c.pinned = on,
                ChatAction::Mute(on) => c.muted_until = if on { -1 } else { 0 },
                ChatAction::MarkUnread(on) => c.unread_mark = on,
                ChatAction::Ttl(_) => {}
            }
        }
        let _ = store.save_meta();
        Ok(())
    }

    async fn block(&self, chat: ChatRef, on: bool) -> Result<(), String> {
        let jid: Jid = self.jid(chat)?.parse().map_err(|e| format!("{e:?}"))?;
        let b = self.client.blocking();
        if on { b.block(&jid).await } else { b.unblock(&jid).await }.map_err(|e| e.to_string())
    }

    /// Elimina la chat anche dal telefono e la toglie dall'archivio locale.
    async fn delete_chat(&self, chat: ChatRef) -> Result<(), String> {
        let jid_s = self.jid(chat)?;
        let jid: Jid = jid_s.parse().map_err(|e| format!("{e:?}"))?;
        self.client.chat_actions().delete_chat(&jid, true, None).await.map_err(|e| e.to_string())?;
        let mut store = self.store();
        store.forget(&jid_s);
        let _ = store.save_meta();
        Ok(())
    }

    /// Gli sticker recenti (senza doppioni) da rimandare.
    fn recent_stickers(&self) -> Vec<crate::model::MediaRef> {
        let mut seen = std::collections::HashSet::new();
        let list = self.store().recent_stickers(48);
        list.iter()
            .filter_map(|(jid, m)| media_ref(jid, m))
            .filter(|r| {
                let sha = r.get::<WaMedia>().and_then(|w| {
                    w.msg.get_base_message().sticker_message.as_option().and_then(|s| s.file_sha256.clone())
                });
                sha.is_none_or(|s| seen.insert(s))
            })
            .take(48)
            .collect()
    }

    /// Inoltra un messaggio dell'archivio a un'altra chat (segnato come inoltrato).
    async fn forward(&self, from: ChatRef, id: i32, to: ChatRef) -> Result<(), String> {
        let from_jid = self.jid(from)?;
        let to_jid = self.jid(to)?;
        let m = self.store().by_id(&from_jid, id).ok_or_else(|| tr("messaggio sconosciuto").to_string())?;
        let ctx = wa::ContextInfo { is_forwarded: Some(true), forwarding_score: Some(1), ..Default::default() };
        let media = media_ref(&from_jid, &m).and_then(|r| r.get::<WaMedia>().map(|w| w.msg.get_base_message().clone()));
        let msg = match media {
            Some(mut msg) => {
                msg.set_context_info(ctx);
                msg
            }
            None if !m.text.is_empty() => wa::Message::text_with_context(m.text.clone(), ctx),
            None => return Err(tr("questo messaggio non si può inoltrare").into()),
        };
        let dest: Jid = to_jid.parse().map_err(|e| format!("{e:?}"))?;
        let sent = self.client.send_message(dest, msg.clone()).await.map_err(|e| e.to_string())?;
        self.keep_sent(&to_jid, &msg, &sent.message_id).map(drop)
    }

    /// Nome (nome e cognome insieme: WhatsApp ne ha uno solo) e info.
    async fn save_profile(&self, p: &crate::model::MyProfile) -> Result<(), String> {
        let name = format!("{} {}", p.first.trim(), p.last.trim()).trim().to_string();
        let profile = self.client.profile();
        profile.set_push_name(&name).await.map_err(|e| e.to_string())?;
        if !p.about.trim().is_empty() {
            profile.set_status_text(p.about.trim()).await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Nuova foto profilo: JPEG quadrato da 640 px, come vuole WhatsApp.
    async fn profile_photo(&self, path: &std::path::Path) -> Result<(), String> {
        let path = path.to_path_buf();
        let jpeg = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
            let img = image::open(&path).map_err(|e| e.to_string())?;
            let side = img.width().min(img.height());
            let square = img.crop_imm((img.width() - side) / 2, (img.height() - side) / 2, side, side);
            let small = square.resize_exact(640, 640, image::imageops::FilterType::Triangle);
            let mut out = std::io::Cursor::new(Vec::new());
            small.to_rgb8().write_to(&mut out, image::ImageFormat::Jpeg).map_err(|e| e.to_string())?;
            Ok(out.into_inner())
        })
        .await
        .map_err(|e| e.to_string())??;
        self.client.profile().set_profile_picture(jpeg).await.map(drop).map_err(|e| e.to_string())
    }

    /// Scollega questo PC: via dal telefono, via la sessione e le chat salvate.
    async fn unlink(&self) -> Result<(), String> {
        self.client.logout().await;
        let dir = crate::paths::data_dir().join("whatsapp");
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())
    }

    /// La stella di WhatsApp, sincronizzata con il telefono.
    async fn star(&self, chat: ChatRef, id: i32, on: bool) -> Result<(), String> {
        let jid_s = self.jid(chat)?;
        let m = self.store().by_id(&jid_s, id).ok_or_else(|| tr("messaggio sconosciuto").to_string())?;
        let jid: Jid = jid_s.parse().map_err(|e| format!("{e:?}"))?;
        // nei gruppi serve chi l'ha scritto, se non sono io
        let who: Option<Jid> = (jid_s.ends_with("@g.us") && !m.me && !m.sender.is_empty())
            .then(|| m.sender.parse().ok())
            .flatten();
        let acts = self.client.chat_actions();
        let res = if on {
            acts.star_message(&jid, who.as_ref(), &m.wa, m.me).await
        } else {
            acts.unstar_message(&jid, who.as_ref(), &m.wa, m.me).await
        };
        res.map_err(|e| e.to_string())
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
            SendKind::Sticker(media) => {
                // lo sticker già sul CDN di WhatsApp: si rimanda lo stesso file
                let base = media.get::<WaMedia>().ok_or("sticker non di WhatsApp")?.msg.get_base_message().clone();
                let mut msg = wa::Message { sticker_message: base.sticker_message.clone(), ..Default::default() };
                if !msg.sticker_message.is_set() {
                    return Err(tr("non è uno sticker").into());
                }
                if let Some(ctx) = context {
                    msg.set_context_info(ctx);
                }
                let sent = self.client.send_message(to, msg.clone()).await.map_err(|e| e.to_string())?;
                self.keep_sent(&jid, &msg, &sent.message_id)
            }
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
            // anche gli stati: la ricevuta va a chi l'ha pubblicato
            let who = if jid.ends_with("@g.us") || jid == super::store::STATUS_JID { m.sender } else { String::new() };
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
    // le foto profilo: tante chat all'avvio, poche richieste alla volta
    let pic_permits = Arc::new(tokio::sync::Semaphore::new(4));
    tokio::spawn(async move {
        let mut player = Player::default();
        while let Some(req) = req_rx.recv().await {
            let wa = wa.clone();
            let out = out.clone();
            match req {
                Request::Presence(on) => {
                    tokio::spawn(async move {
                        let p = wa.client.presence();
                        let _ = if on { p.set_available().await } else { p.set_unavailable().await };
                    });
                }
                Request::Typing { peer_ref, on } => {
                    tokio::spawn(async move {
                        let Some(jid) = wa.jid(peer_ref).ok().and_then(|j| j.parse::<Jid>().ok()) else { return };
                        let cs = wa.client.chatstate();
                        let _ = if on { cs.send_composing(&jid).await } else { cs.send_paused(&jid).await };
                    });
                }
                Request::Hist { peer_ref, peer } => {
                    // chat privata aperta: online e ultimo accesso dell'altro
                    if let Some(jid) = wa.jid(peer_ref).ok().filter(|j| !j.ends_with("@g.us")) {
                        let wa = wa.clone();
                        tokio::spawn(async move {
                            if let Ok(jid) = jid.parse::<Jid>() {
                                let _ = wa.client.presence().subscribe(jid).await;
                            }
                        });
                    }
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
                    let docs = if set_id == crate::model::RECENT_SET { wa.recent_stickers() } else { Vec::new() };
                    let _ = out.send(Response::Docs { set_id, docs: Ok(docs) });
                }
                Request::Avatar { peer_ref, photo_id } => {
                    let permits = Arc::clone(&pic_permits);
                    tokio::spawn(async move {
                        let _p = permits.acquire().await;
                        let _ = out.send(Response::Avatar { photo_id, path: wa.avatar(peer_ref).await });
                    });
                }
                Request::Profile => {
                    let me = wa.client.push_name();
                    let phone = wa.client.pn().map(|p| format!("+{}", p.user)).unwrap_or_default();
                    let _ = out.send(Response::Profile(Ok(crate::model::MyProfile { first: me, phone, ..Default::default() })));
                }
                Request::Link(_) | Request::Join(_) => {
                    let _ = out.send(Response::Link(unsupported()));
                }
                Request::Forward { from, id, to, to_name } => {
                    tokio::spawn(async move {
                        let res = wa.forward(from, id, to).await.map(|()| to_name);
                        let _ = out.send(Response::Forwarded(res));
                    });
                }
                Request::Block { peer_ref, peer, blocked } => {
                    tokio::spawn(async move {
                        let res = wa.block(peer_ref, blocked).await.map(|()| blocked);
                        let _ = out.send(Response::Blocked { peer, res });
                    });
                }
                Request::DeleteChat { peer_ref, peer, .. } => {
                    tokio::spawn(async move {
                        let res = wa.delete_chat(peer_ref).await;
                        let _ = out.send(Response::ChatDeleted { peer, res });
                    });
                }
                Request::SaveProfile { profile, .. } => {
                    tokio::spawn(async move {
                        let _ = out.send(Response::ProfileSaved(wa.save_profile(&profile).await));
                    });
                }
                Request::ProfilePhoto(path) => {
                    tokio::spawn(async move {
                        let _ = out.send(Response::ProfileSaved(wa.profile_photo(&path).await));
                    });
                }
                Request::Sessions => {
                    // gli altri dispositivi collegati li vede solo il telefono
                    let me = crate::model::Session {
                        hash: 0,
                        current: true,
                        device: tr("Rondine · questo PC").into(),
                        app: tr("WhatsApp, dispositivo collegato").into(),
                        place: String::new(),
                        ip: String::new(),
                        active: now_secs(),
                    };
                    let _ = out.send(Response::Sessions(Ok(vec![me])));
                }
                Request::EndSession(_) => {
                    tokio::spawn(async move {
                        let _ = out.send(Response::SessionEnded(wa.unlink().await.map(|()| None)));
                    });
                }
                Request::ChatAction { peer_ref, peer, action } => {
                    tokio::spawn(async move {
                        let res = wa.chat_action(peer_ref, action).await;
                        let _ = out.send(Response::ChatActionDone { peer, action, res });
                    });
                }
                Request::Star { peer_ref, id, on } => {
                    tokio::spawn(async move {
                        if let Err(e) = wa.star(peer_ref, id, on).await {
                            let _ = out.send(Response::LiveErr(crate::trf!("stella: {}", e)));
                        }
                    });
                }
            }
        }
    });
}
