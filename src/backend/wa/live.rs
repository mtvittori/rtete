//! Gli eventi di WhatsApp: salvati nell'archivio e tradotti negli eventi
//! neutri `Live` (o in una lista chat nuova, dopo un history sync).

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore::types::presence::{ChatPresence, ChatPresenceMedia, ReceiptType};

use super::store::{chat_id_of, Store};
use super::{dialog_of, row_of, stored_from};
use crate::i18n::tr;
use crate::model::{ChatAction, Live};
use crate::send::Response;

/// Quello che serve a chi gestisce gli eventi: archivio, canale delle
/// risposte e, fino alla prima connessione, l'avviso per `start`.
#[derive(Clone)]
pub struct Shared(Arc<Inner>);

struct Inner {
    store: Arc<Mutex<Store>>,
    out: mpsc::UnboundedSender<Response>,
    ready: Mutex<Option<mpsc::UnboundedSender<Result<(), String>>>>,
    pair: super::PairSink,
    /// un salvataggio con lista nuova già in programma (raffiche di eventi)
    refresh: std::sync::atomic::AtomicBool,
}

/// Il JID senza dispositivo (`393…:12@s.whatsapp.net` → `393…@s.whatsapp.net`).
fn bare(jid: &Jid) -> String {
    jid.to_non_ad_string()
}

impl Shared {
    pub fn new(
        store: Arc<Mutex<Store>>,
        out: mpsc::UnboundedSender<Response>,
        ready: mpsc::UnboundedSender<Result<(), String>>,
        pair: super::PairSink,
    ) -> Self {
        Self(Arc::new(Inner { store, out, ready: Mutex::new(Some(ready)), pair, refresh: Default::default() }))
    }

    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.0.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn live(&self, ev: Live) {
        let _ = self.0.out.send(Response::Live(ev));
    }

    /// Prima della TUI gli errori vanno a `start` (e a schermo), dopo
    /// nella riga di stato.
    fn fail(&self, why: String) {
        let ready = self.0.ready.lock().ok().and_then(|mut g| g.take());
        match ready {
            Some(tx) => {
                let _ = tx.send(Err(why));
            }
            None => {
                let _ = self.0.out.send(Response::LiveErr(why));
            }
        }
    }

    /// Un cambio di stato della chat fatto altrove: archivio locale e lista.
    fn chat_changed(&self, jid: &Jid, action: ChatAction) {
        let jid = self.store().canon(&bare(jid));
        {
            let mut store = self.store();
            if let Some(c) = store.chat_mut(&jid) {
                match action {
                    ChatAction::Archive(on) => c.archived = on,
                    ChatAction::Pin(on) => c.pinned = on,
                    ChatAction::Mute(on) => c.muted_until = if on { -1 } else { 0 },
                    ChatAction::MarkUnread(on) => c.unread_mark = on,
                    ChatAction::Ttl(_) => {}
                }
            }
        }
        self.live(Live::ChatChanged { chat: chat_id_of(&jid), action });
        // a raffica dopo una sincronizzazione completa: un salvataggio solo
        self.refresh_soon();
    }

    /// Salva e manda la lista delle chat fra poco: le rubriche e le foto
    /// arrivano a centinaia di eventi, basta un aggiornamento solo.
    fn refresh_soon(&self) {
        use std::sync::atomic::Ordering;
        if self.0.refresh.swap(true, Ordering::AcqRel) {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            me.0.refresh.store(false, Ordering::Release);
            let store = me.store();
            me.save(&store);
            let _ = me.0.out.send(Response::Dialogs(Ok(store.dialogs())));
        });
    }

    /// Le chat rimaste col JID anonimo: il numero dalla tabella lid ↔
    /// numero che whatsapp-rust impara da solo, e i doppioni si fondono.
    fn resolve_lids(&self, client: &Arc<Client>) {
        let me = self.clone();
        let client = Arc::clone(client);
        tokio::spawn(async move {
            let lids = me.store().unlinked_lids();
            let mut changed = false;
            for lid in lids {
                let Ok(jid) = lid.parse::<Jid>() else { continue };
                if let Ok(Some(e)) = client.get_lid_pn_entry(&jid).await {
                    let pn = format!("{}@s.whatsapp.net", e.phone_number);
                    changed |= me.store().link(&lid, &pn);
                }
            }
            // anche le mie due identità sono la stessa chat («Tu»)
            if let (Some(lid), Some(pn)) = (client.lid(), client.pn()) {
                changed |= me.store().link(&bare(&lid), &bare(&pn));
            }
            if changed {
                me.refresh_soon();
            }
        });
    }

    /// Rubrica (critical_unblock_low) e archiviate, fissate, silenziate
    /// (regular_low) arrivano con la sincronizzazione dello stato
    /// all'abbinamento: chi aveva abbinato prima che Rondine le leggesse le
    /// riceve di nuovo, per intero, una volta sola.
    fn sync_contacts(&self, client: &Arc<Client>) {
        use whatsapp_rust::sync_task::MajorSyncTask;
        use whatsapp_rust::wacore::appstate::patch_decode::WAPatchName;
        {
            let mut store = self.store();
            if store.state_synced {
                return;
            }
            store.state_synced = true;
            self.save(&store);
        }
        let client = Arc::clone(client);
        tokio::spawn(async move {
            for name in [WAPatchName::CriticalUnblockLow, WAPatchName::RegularLow] {
                client.process_sync_task(MajorSyncTask::AppStateSync { name, full_sync: true }).await;
            }
        });
    }

    fn save(&self, store: &Store) {
        if let Err(e) = store.save_meta() {
            let _ = self.0.out.send(Response::LiveErr(crate::trf!("archivio: {}", e)));
        }
    }

    pub fn handle(&self, event: &Event, client: &Arc<Client>) {
        match event {
            Event::Connected(_) => {
                self.resolve_lids(client);
                self.sync_contacts(client);
                if let Some(tx) = self.0.ready.lock().ok().and_then(|mut g| g.take()) {
                    let _ = tx.send(Ok(()));
                }
            }
            Event::PairSuccess(_) => (self.0.pair)(super::Pairing::Paired),
            Event::PairError(e) => self.fail(crate::trf!("abbinamento non riuscito: {}", e.error)),
            Event::LoggedOut(_) => self.fail(tr("dispositivo scollegato dal telefono: riavvia per abbinarlo di nuovo").into()),
            Event::TemporaryBan(b) => self.fail(crate::trf!("WhatsApp ha sospeso l'account: {}", format!("{b:?}"))),
            Event::Messages(batch) => {
                for m in batch.messages.iter() {
                    self.message(&m.message, &m.info);
                }
            }
            Event::Receipt(r) => self.receipt(r),
            // archivio, fissate, silenziate e «da leggere» cambiati dal telefono
            Event::ArchiveUpdate(u) => {
                self.chat_changed(&u.jid, ChatAction::Archive(u.action.archived.unwrap_or(false)))
            }
            Event::PinUpdate(u) => self.chat_changed(&u.jid, ChatAction::Pin(u.action.pinned.unwrap_or(false))),
            Event::MuteUpdate(u) => self.chat_changed(&u.jid, ChatAction::Mute(u.action.muted.unwrap_or(false))),
            Event::MarkChatAsReadUpdate(u) => {
                self.chat_changed(&u.jid, ChatAction::MarkUnread(!u.action.read.unwrap_or(true)))
            }
            Event::ChatPresence(p) => {
                let what = match (&p.state, &p.media) {
                    (ChatPresence::Composing, ChatPresenceMedia::Audio) => Some(tr("registra un vocale")),
                    (ChatPresence::Composing, _) => Some(tr("sta scrivendo")),
                    _ => None,
                };
                let (chat, who) = {
                    let store = self.store();
                    let chat = chat_id_of(&store.canon(&bare(&p.source.chat)));
                    (chat, p.source.is_group.then(|| chat_id_of(&store.canon(&bare(&p.source.sender)))))
                };
                self.live(Live::Typing { chat, who, what });
            }
            Event::PushNameUpdate(p) => {
                let mut store = self.store();
                store.set_name(&bare(&p.jid), &p.new_push_name);
                self.save(&store);
            }
            Event::HistorySync(h) => {
                if let Some(hs) = h.get() {
                    self.history(hs);
                }
                self.resolve_lids(client);
            }
            // i nomi della rubrica del telefono (e il legame lid ↔ numero)
            Event::ContactUpdate(u) => {
                let a = &u.action;
                let mut store = self.store();
                if let (Some(lid), Some(pn)) = (&a.lid_jid, &a.pn_jid) {
                    store.link(lid, pn);
                }
                let jid = store.canon(&bare(&u.jid));
                let name = a.full_name.as_deref().or(a.first_name.as_deref()).unwrap_or("");
                if store.set_contact(&jid, name) {
                    drop(store);
                    self.refresh_soon();
                }
            }
            // foto cambiata: si richiede (id nuovo, che non è in cache)
            Event::PictureUpdate(p) => {
                let mut store = self.store();
                let jid = store.canon(&bare(&p.jid));
                if let Some(c) = store.chat_mut(&jid) {
                    c.pic = if p.removed { "-".into() } else { String::new() };
                    drop(store);
                    self.refresh_soon();
                }
            }
            _ => {}
        }
    }

    /// I JID alternativi di un messaggio (numero ↔ `@lid`): se è nuovo il
    /// legame si salva e l'eventuale chat doppia si fonde. `true` se la
    /// lista delle chat è cambiata.
    fn learn(&self, store: &mut Store, src: &whatsapp_rust::wacore::types::message::MessageSource) -> bool {
        let mut changed = false;
        if let Some(alt) = &src.sender_alt {
            changed |= store.link(&bare(&src.sender), &bare(alt));
        }
        if let (false, Some(alt)) = (src.is_group, &src.recipient_alt) {
            changed |= store.link(&bare(&src.chat), &bare(alt));
        }
        changed
    }

    /// Un messaggio (nuovo, reazione, modifica o cancellazione).
    fn message(&self, msg: &wa::Message, info: &MessageInfo) {
        let mut store = self.store();
        if self.learn(&mut store, &info.source) {
            let _ = self.0.out.send(Response::Dialogs(Ok(store.dialogs())));
        }
        let jid = store.canon(&bare(&info.source.chat));
        let chat = chat_id_of(&jid);
        let me = info.source.is_from_me;
        let sender = store.canon(&bare(&info.source.sender));
        let base = msg.get_base_message();

        if let Some(r) = base.reaction_message.as_option() {
            let Some(target) = r.key.as_option().and_then(|k| k.id.clone()) else { return };
            let who = if me { "me".to_string() } else { sender };
            let emoji = r.text.clone().unwrap_or_default();
            let res = store.update(&jid, &target, |m| {
                m.reactions.retain(|(w, _)| *w != who);
                if !emoji.is_empty() {
                    m.reactions.push((who.clone(), emoji.clone()));
                }
            });
            if let Ok(Some(m)) = res {
                let items = row_of(&mut store, &jid, &m).reactions;
                self.live(Live::Reactions { chat, msg: m.id, items });
            }
            return;
        }

        if let Some(p) = base.protocol_message.as_option() {
            let Some(target) = p.key.as_option().and_then(|k| k.id.clone()) else { return };
            use wa::message::protocol_message::Type;
            let res = match p.r#type {
                Some(Type::REVOKE) => store.update(&jid, &target, |m| m.deleted = true),
                Some(Type::MESSAGE_EDIT) => {
                    let Some(text) = p.edited_message.as_option().and_then(|e| e.text_content().or(e.get_caption()))
                    else {
                        return;
                    };
                    let text = text.to_string();
                    store.update(&jid, &target, |m| {
                        m.text = text;
                        m.edited = true;
                    })
                }
                _ => return,
            };
            if let Ok(Some(m)) = res {
                let row = row_of(&mut store, &jid, &m);
                self.save(&store);
                self.live(Live::Edited { chat, row });
            }
            return;
        }

        let ts = info.timestamp.timestamp();
        let Some(stored) = stored_from(msg, &info.id, me, &sender, &info.push_name, ts) else { return };
        if !me {
            store.set_name(&sender, &info.push_name);
        }
        let fresh_chat = store.chat(&jid).is_none();
        let fresh = match store.insert(&jid, info.source.is_group, vec![stored]) {
            Ok(f) => f,
            Err(e) => {
                let _ = self.0.out.send(Response::LiveErr(crate::trf!("archivio: {}", e)));
                return;
            }
        };
        for m in fresh {
            if !me {
                if let Some(c) = store.chat_mut(&jid) {
                    c.unread += 1;
                }
            }
            let row = row_of(&mut store, &jid, &m);
            let dialog = if fresh_chat { dialog_of(&store, &jid) } else { None };
            self.live(Live::NewMessage { chat, row, dialog });
        }
        self.save(&store);
    }

    /// Ricevute: l'altro ha letto (spunte doppie) o io ho letto altrove.
    fn receipt(&self, r: &whatsapp_rust::wacore::types::events::Receipt) {
        let mut store = self.store();
        let jid = store.canon(&bare(&r.source.chat));
        let chat = chat_id_of(&jid);
        match r.r#type {
            ReceiptType::Read | ReceiptType::Played if !r.source.is_from_me => {
                let max = r
                    .message_ids
                    .iter()
                    .filter_map(|id| store.by_wa(&jid, id))
                    .filter(|m| m.me)
                    .map(|m| m.id)
                    .max();
                let Some(max_id) = max else { return };
                if let Some(c) = store.chat_mut(&jid) {
                    c.read_out = c.read_out.max(max_id);
                }
                self.save(&store);
                self.live(Live::ReadOutbox { chat, max_id });
            }
            ReceiptType::ReadSelf => {
                if let Some(c) = store.chat_mut(&jid) {
                    c.unread = 0;
                }
                self.save(&store);
                self.live(Live::ReadInbox { chat, still: 0 });
            }
            _ => {}
        }
    }

    /// La cronologia mandata dal telefono (all'abbinamento e dopo, a
    /// pezzi): chat, nomi e messaggi nell'archivio, poi la lista nuova.
    fn history(&self, hs: &wa::HistorySync) {
        let mut store = self.store();
        for p in &hs.pushnames {
            if let (Some(id), Some(name)) = (&p.id, &p.pushname) {
                store.set_name(id, name);
            }
        }
        for m in &hs.phone_number_to_lid_mappings {
            if let (Some(pn), Some(lid)) = (&m.pn_jid, &m.lid_jid) {
                store.link(pn, lid);
            }
        }
        for conv in &hs.conversations {
            for alt in [&conv.pn_jid, &conv.lid_jid].into_iter().flatten() {
                store.link(&conv.id, alt);
            }
            let jid = store.canon(&conv.id);
            let group = jid.ends_with("@g.us");
            if !(group || jid.ends_with("@s.whatsapp.net") || jid.ends_with("@lid")) {
                continue; // stati, canali, liste broadcast
            }
            store.ensure_chat(&jid, group);
            let msgs: Vec<_> = conv
                .messages
                .iter()
                .filter_map(|hm| {
                    let w = hm.message.as_option()?;
                    let key = w.key.as_option()?;
                    let me = key.from_me.unwrap_or(false);
                    let sender = w.participant.clone().or_else(|| key.participant.clone()).unwrap_or_else(|| {
                        if me { String::new() } else { jid.clone() }
                    });
                    let ts = w.message_timestamp.unwrap_or(0) as i64;
                    let name = w.push_name.clone().unwrap_or_default();
                    stored_from(w.message.as_option()?, key.id.as_deref()?, me, &sender, &name, ts)
                })
                .collect();
            if let Err(e) = store.insert(&jid, group, msgs) {
                let _ = self.0.out.send(Response::LiveErr(crate::trf!("archivio: {}", e)));
            }
            if let Some(c) = store.chat_mut(&jid) {
                if let Some(name) = conv.name.clone().or_else(|| conv.display_name.clone()).filter(|n| !n.is_empty()) {
                    c.name = name;
                }
                if let Some(n) = conv.unread_count {
                    c.unread = n as usize;
                }
                c.pinned = conv.pinned.unwrap_or(0) > 0;
                if let Some(a) = conv.archived {
                    c.archived = a;
                }
                if let Some(u) = conv.marked_as_unread {
                    c.unread_mark = u;
                }
                c.muted_until = match conv.mute_end_time {
                    Some(t) if t == u64::MAX => -1,
                    Some(t) => (t / if t > 1 << 40 { 1000 } else { 1 }) as i64,
                    None => 0,
                };
                if let Some(t) = conv.conversation_timestamp {
                    c.ts = c.ts.max(t as i64);
                }
            }
        }
        self.save(&store);
        let _ = self.0.out.send(Response::Dialogs(Ok(store.dialogs())));
    }
}
