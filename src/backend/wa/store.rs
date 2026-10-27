//! L'archivio locale delle chat WhatsApp. Il server non tiene la
//! cronologia: arriva una volta con l'abbinamento (history sync) e poi
//! solo i messaggi nuovi, quindi qui si salva tutto su disco.
//!
//! - `chats.json`: le chat (nome, non letti, fissate…) e i nomi dei contatti;
//! - `msgs/<id>.jsonl`: i messaggi di una chat, uno per riga.
//!
//! In RAM restano le chat e i messaggi delle ultime chat toccate. Gli id
//! testuali di WhatsApp diventano numeri progressivi per chat (`MsgId`).
//! Niente tipi di whatsapp-rust qui: il media è il protobuf in base64.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{ChatId, ChatRef, DialogInfo, LastMsg, Seen};

/// Chat con i messaggi tenuti in RAM (le altre si rileggono dal disco).
const HOT_CHATS: usize = 8;

/// L'id stabile di una chat dal suo JID: FNV-1a, sopra gli intervalli di
/// Telegram (utenti fino a 2^40, gruppi e canali negativi).
pub fn chat_id_of(jid: &str) -> ChatId {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in jid.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    ChatId((1i64 << 41) + (h % (1u64 << 50)) as i64)
}

/// Un messaggio salvato.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredMsg {
    pub id: i32,
    /// l'id di WhatsApp
    pub wa: String,
    pub me: bool,
    /// JID di chi l'ha scritto (gruppi)
    #[serde(default)]
    pub sender: String,
    /// il nome che il mittente si è dato (push name)
    #[serde(default)]
    pub name: String,
    /// secondi Unix
    pub ts: i64,
    /// text, image, sticker, video, audio, voice, doc, note
    #[serde(default)]
    pub kind: String,
    /// il testo o la didascalia
    #[serde(default)]
    pub text: String,
    /// il messaggio protobuf in base64 (per scaricare il media)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mime: String,
    /// nome del documento
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub secs: u32,
    /// gif (video), animato (sticker)
    #[serde(default)]
    pub flag: bool,
    /// risposta a questo messaggio (id WhatsApp)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
    #[serde(default)]
    pub fwd: bool,
    #[serde(default)]
    pub edited: bool,
    #[serde(default)]
    pub deleted: bool,
    /// reazioni: (JID di chi, emoji); "me" per le mie
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<(String, String)>,
}

/// Una chat.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChatMeta {
    pub jid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub group: bool,
    #[serde(default)]
    pub unread: usize,
    #[serde(default)]
    pub pinned: bool,
    /// silenziata fino a (secondi Unix; -1 per sempre)
    #[serde(default)]
    pub muted_until: i64,
    /// ultimo messaggio (secondi Unix), per l'ordine della lista
    #[serde(default)]
    pub ts: i64,
    /// l'altro ha letto fino a questo id (spunte doppie)
    #[serde(default)]
    pub read_out: i32,
    /// il prossimo id libero
    #[serde(default = "first_id")]
    pub next_id: i32,
    /// anteprima dell'ultimo messaggio
    #[serde(default)]
    pub last: Option<(i32, String, bool)>,
    #[serde(default)]
    pub archived: bool,
    /// segnata come «da leggere»
    #[serde(default)]
    pub unread_mark: bool,
}

fn first_id() -> i32 {
    1
}

#[derive(Default, Serialize, Deserialize)]
struct Meta {
    chats: Vec<ChatMeta>,
    #[serde(default)]
    names: HashMap<String, String>,
}

pub struct Store {
    dir: PathBuf,
    chats: BTreeMap<String, ChatMeta>,
    ids: HashMap<ChatId, String>,
    names: HashMap<String, String>,
    /// messaggi in RAM per chat (ordinati per data), le più recenti in coda
    hot: HashMap<String, Vec<StoredMsg>>,
    order: VecDeque<String>,
}

impl Store {
    /// Apre (o crea) l'archivio nella cartella.
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        crate::paths::ensure_private_dir(&dir.join("msgs"))?;
        let meta: Meta = std::fs::read(dir.join("chats.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut store = Self {
            dir: dir.to_path_buf(),
            chats: BTreeMap::new(),
            ids: HashMap::new(),
            names: meta.names,
            hot: HashMap::new(),
            order: VecDeque::new(),
        };
        for c in meta.chats {
            store.ids.insert(chat_id_of(&c.jid), c.jid.clone());
            store.chats.insert(c.jid.clone(), c);
        }
        Ok(store)
    }

    /// Salva chat e nomi (scrittura atomica: temporaneo e rinomina).
    pub fn save_meta(&self) -> std::io::Result<()> {
        let meta = Meta {
            chats: self.chats.values().cloned().collect(),
            names: self.names.clone(),
        };
        let body = serde_json::to_vec(&meta).map_err(std::io::Error::other)?;
        write_atomic(&self.dir.join("chats.json"), &body)
    }

    pub fn jid_of(&self, chat: ChatId) -> Option<&str> {
        self.ids.get(&chat).map(String::as_str)
    }

    pub fn chat(&self, jid: &str) -> Option<&ChatMeta> {
        self.chats.get(jid)
    }

    pub fn chat_mut(&mut self, jid: &str) -> Option<&mut ChatMeta> {
        self.chats.get_mut(jid)
    }

    /// La chat, creata se manca; `true` se è nuova.
    pub fn ensure_chat(&mut self, jid: &str, group: bool) -> bool {
        if self.chats.contains_key(jid) {
            return false;
        }
        self.ids.insert(chat_id_of(jid), jid.to_string());
        self.chats.insert(
            jid.to_string(),
            ChatMeta { jid: jid.to_string(), group, next_id: 1, ..Default::default() },
        );
        true
    }

    /// Il nome che un contatto si è dato (push name).
    pub fn set_name(&mut self, jid: &str, name: &str) {
        if !name.trim().is_empty() {
            self.names.insert(jid.to_string(), name.trim().to_string());
        }
    }

    /// Il nome da mostrare per un JID: nome della chat, nome del contatto,
    /// altrimenti il numero.
    pub fn display_name(&self, jid: &str) -> String {
        if let Some(c) = self.chats.get(jid).filter(|c| !c.name.is_empty()) {
            return c.name.clone();
        }
        if let Some(n) = self.names.get(jid) {
            return n.clone();
        }
        let user = jid.split(['@', ':']).next().unwrap_or(jid);
        if jid.ends_with("@s.whatsapp.net") && user.chars().all(|c| c.is_ascii_digit()) {
            format!("+{user}")
        } else {
            user.to_string()
        }
    }

    /// Dimentica una chat eliminata: voce, messaggi in RAM e file.
    pub fn forget(&mut self, jid: &str) {
        let _ = std::fs::remove_file(self.msgs_path(jid));
        self.chats.remove(jid);
        self.hot.remove(jid);
        self.order.retain(|j| j != jid);
    }

    fn msgs_path(&self, jid: &str) -> PathBuf {
        self.dir.join("msgs").join(format!("{}.jsonl", chat_id_of(jid).0))
    }

    /// I messaggi di una chat (letti dal disco la prima volta), per data.
    pub fn messages(&mut self, jid: &str) -> &mut Vec<StoredMsg> {
        if !self.hot.contains_key(jid) {
            let mut msgs: Vec<StoredMsg> = std::fs::read_to_string(self.msgs_path(jid))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect();
            msgs.sort_by_key(|m| (m.ts, m.id));
            self.hot.insert(jid.to_string(), msgs);
        }
        // in fondo alla coda: è la più recente; le vecchie escono dalla RAM
        self.order.retain(|j| j != jid);
        self.order.push_back(jid.to_string());
        while self.order.len() > HOT_CHATS {
            if let Some(old) = self.order.pop_front() {
                self.hot.remove(&old);
            }
        }
        self.hot.get_mut(jid).expect("appena caricata")
    }

    fn rewrite(&mut self, jid: &str) -> std::io::Result<()> {
        let path = self.msgs_path(jid);
        let mut body = Vec::new();
        for m in self.messages(jid).iter() {
            serde_json::to_writer(&mut body, m).map_err(std::io::Error::other)?;
            body.push(b'\n');
        }
        write_atomic(&path, &body)
    }

    /// Salva i messaggi nuovi di una chat (quelli con un id WhatsApp già
    /// visto si saltano) e restituisce quelli entrati, con l'id assegnato.
    pub fn insert(&mut self, jid: &str, group: bool, msgs: Vec<StoredMsg>) -> std::io::Result<Vec<StoredMsg>> {
        self.ensure_chat(jid, group);
        let mut next = self.chats[jid].next_id;
        let existing = self.messages(jid);
        let newest = existing.last().map(|m| (m.ts, m.id));
        let mut fresh = Vec::new();
        for mut m in msgs {
            if existing.iter().any(|x| x.wa == m.wa) || fresh.iter().any(|x: &StoredMsg| x.wa == m.wa) {
                continue;
            }
            m.id = next;
            next += 1;
            fresh.push(m);
        }
        if fresh.is_empty() {
            return Ok(fresh);
        }
        // tutti più nuovi dell'ultimo: basta accodarli al file
        let append = newest.is_none_or(|n| fresh.iter().all(|m| (m.ts, m.id) > n));
        existing.extend(fresh.iter().cloned());
        existing.sort_by_key(|m| (m.ts, m.id));
        if let Some(last) = existing.last().cloned() {
            let chat = self.chats.get_mut(jid).expect("creata sopra");
            chat.next_id = next;
            if last.ts >= chat.ts {
                chat.ts = last.ts;
                chat.last = Some((last.id, summary(&last), last.me));
            }
        }
        if append {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(self.msgs_path(jid))?;
            let mut body = Vec::new();
            for m in &fresh {
                serde_json::to_writer(&mut body, m).map_err(std::io::Error::other)?;
                body.push(b'\n');
            }
            f.write_all(&body)?;
        } else {
            self.rewrite(jid)?;
        }
        Ok(fresh)
    }

    /// Cambia un messaggio (modifica, cancellazione, reazione) e riscrive
    /// la chat; restituisce la versione nuova.
    pub fn update(&mut self, jid: &str, wa: &str, f: impl FnOnce(&mut StoredMsg)) -> std::io::Result<Option<StoredMsg>> {
        if !self.chats.contains_key(jid) {
            return Ok(None);
        }
        let Some(m) = self.messages(jid).iter_mut().find(|m| m.wa == wa) else {
            return Ok(None);
        };
        f(m);
        let out = m.clone();
        self.rewrite(jid)?;
        if self.chats.get(jid).and_then(|c| c.last.as_ref()).is_some_and(|l| l.0 == out.id) {
            if let Some(c) = self.chats.get_mut(jid) {
                c.last = Some((out.id, summary(&out), out.me));
            }
        }
        Ok(Some(out))
    }

    pub fn by_wa(&mut self, jid: &str, wa: &str) -> Option<StoredMsg> {
        if !self.chats.contains_key(jid) {
            return None;
        }
        self.messages(jid).iter().find(|m| m.wa == wa).cloned()
    }

    pub fn by_id(&mut self, jid: &str, id: i32) -> Option<StoredMsg> {
        if !self.chats.contains_key(jid) {
            return None;
        }
        self.messages(jid).iter().find(|m| m.id == id).cloned()
    }

    /// Gli ultimi `n` messaggi, dal più vecchio.
    pub fn last(&mut self, jid: &str, n: usize) -> Vec<StoredMsg> {
        if !self.chats.contains_key(jid) {
            return Vec::new();
        }
        let msgs = self.messages(jid);
        msgs[msgs.len().saturating_sub(n)..].to_vec()
    }

    /// Gli `n` messaggi prima di `before` (escluso), dal più vecchio.
    pub fn before(&mut self, jid: &str, before: i32, n: usize) -> Vec<StoredMsg> {
        if !self.chats.contains_key(jid) {
            return Vec::new();
        }
        let msgs = self.messages(jid);
        let end = msgs.iter().position(|m| m.id == before).unwrap_or(0);
        msgs[end.saturating_sub(n)..end].to_vec()
    }

    /// I messaggi in arrivo ancora da segnare come letti (gli ultimi `unread`).
    pub fn unread_tail(&mut self, jid: &str) -> Vec<StoredMsg> {
        let n = self.chats.get(jid).map(|c| c.unread).unwrap_or(0).max(1);
        let msgs = self.messages(jid);
        msgs.iter().rev().filter(|m| !m.me).take(n).cloned().collect()
    }

    /// Ricerca nel testo (dal più recente).
    pub fn search(&mut self, jid: &str, query: &str, limit: usize) -> Vec<StoredMsg> {
        let q = query.to_lowercase();
        if q.trim().is_empty() || !self.chats.contains_key(jid) {
            return Vec::new();
        }
        self.messages(jid)
            .iter()
            .rev()
            .filter(|m| !m.deleted && m.text.to_lowercase().contains(&q))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Le chat per la lista: fissate in alto, poi la più recente.
    pub fn dialogs(&self) -> Vec<DialogInfo> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut out: Vec<(&ChatMeta, DialogInfo)> = self
            .chats
            .values()
            .filter(|c| c.ts > 0 || c.last.is_some() || !c.name.is_empty())
            .map(|c| {
                let id = chat_id_of(&c.jid);
                let info = DialogInfo {
                    name: self.display_name(&c.jid),
                    peer_ref: ChatRef { id, auth: 0 },
                    peer_id: id,
                    is_group: c.group,
                    unread: c.unread,
                    pinned: c.pinned,
                    last: c.last.as_ref().map(|(id, text, me)| LastMsg {
                        id: *id,
                        text: text.clone(),
                        outgoing: *me,
                        date: c.ts,
                    }),
                    read_out: c.read_out,
                    broadcast: false,
                    muted: c.muted_until < 0 || c.muted_until > now,
                    archived: c.archived,
                    unread_mark: c.unread_mark,
                    photo_id: None,
                    seen: Seen::Unknown,
                };
                (c, info)
            })
            .collect();
        out.sort_by_key(|(c, _)| (!c.pinned, std::cmp::Reverse(c.ts)));
        out.into_iter().map(|(_, d)| d).collect()
    }
}

/// Una riga che descrive il messaggio (anteprima nella lista chat).
pub fn summary(m: &StoredMsg) -> String {
    use crate::i18n::tr;
    if m.deleted {
        return tr("messaggio eliminato").into();
    }
    let what = match m.kind.as_str() {
        "image" => tr("🖼 foto"),
        "sticker" => "sticker",
        "video" => "🎥 video",
        "voice" => tr("🎤 messaggio vocale"),
        "audio" => "🎵 audio",
        "doc" => "📄",
        _ => "",
    };
    let text = match m.kind.as_str() {
        "doc" if m.text.is_empty() => m.file.clone(),
        _ => m.text.lines().next().unwrap_or("").to_string(),
    };
    match (what.is_empty(), text.is_empty()) {
        (true, _) => text,
        (false, true) => what.to_string(),
        (false, false) => format!("{what} {text}"),
    }
}

fn write_atomic(path: &Path, body: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(body)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(wa: &str, ts: i64, text: &str) -> StoredMsg {
        StoredMsg { wa: wa.into(), ts, kind: "text".into(), text: text.into(), ..Default::default() }
    }

    fn tmp_store(tag: &str) -> (PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!("rondine-wa-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(&dir).unwrap();
        (dir, store)
    }

    #[test]
    fn chat_id_stabile_e_fuori_da_telegram() {
        let a = chat_id_of("393331234567@s.whatsapp.net");
        assert_eq!(a, chat_id_of("393331234567@s.whatsapp.net"));
        assert_ne!(a, chat_id_of("393331234568@s.whatsapp.net"));
        assert!(a.0 > 1 << 40, "sopra gli utenti Telegram");
        assert!(!a.own_msg_ids());
    }

    #[test]
    fn insert_assegna_id_salta_i_doppi_e_rilegge_dal_disco() {
        let (dir, mut s) = tmp_store("ins");
        let jid = "393331234567@s.whatsapp.net";
        let got = s.insert(jid, false, vec![msg("A", 10, "ciao"), msg("B", 20, "come va")]).unwrap();
        assert_eq!(got.iter().map(|m| m.id).collect::<Vec<_>>(), [1, 2]);
        // lo stesso messaggio due volte (history sync e live) entra una volta
        assert!(s.insert(jid, false, vec![msg("B", 20, "come va")]).unwrap().is_empty());
        // uno più vecchio arrivato dopo: id nuovo, ma in ordine di data
        s.insert(jid, false, vec![msg("C", 5, "primo")]).unwrap();
        s.save_meta().unwrap();
        let mut s = Store::open(&dir).unwrap();
        let texts: Vec<String> = s.last(jid, 10).into_iter().map(|m| m.text).collect();
        assert_eq!(texts, ["primo", "ciao", "come va"]);
        assert_eq!(s.chat(jid).unwrap().last.as_ref().unwrap().1, "come va");
        assert_eq!(s.before(jid, 1, 10).len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn update_cambia_il_messaggio_e_l_anteprima() {
        let (dir, mut s) = tmp_store("upd");
        let jid = "g1@g.us";
        s.insert(jid, true, vec![msg("A", 10, "ciao")]).unwrap();
        let m = s.update(jid, "A", |m| { m.text = "ciao a tutti".into(); m.edited = true; }).unwrap().unwrap();
        assert!(m.edited);
        assert_eq!(s.chat(jid).unwrap().last.as_ref().unwrap().1, "ciao a tutti");
        s.update(jid, "A", |m| m.deleted = true).unwrap();
        assert_eq!(s.dialogs()[0].last.as_ref().unwrap().text, "messaggio eliminato");
        assert!(s.update(jid, "nessuno", |_| {}).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dialogs_fissate_poi_recenti_e_nomi() {
        let (dir, mut s) = tmp_store("dlg");
        s.insert("1@s.whatsapp.net", false, vec![msg("A", 10, "a")]).unwrap();
        s.insert("2@s.whatsapp.net", false, vec![msg("B", 30, "b")]).unwrap();
        s.insert("3@s.whatsapp.net", false, vec![msg("C", 20, "c")]).unwrap();
        s.chat_mut("1@s.whatsapp.net").unwrap().pinned = true;
        s.set_name("3@s.whatsapp.net", "Anna");
        let names: Vec<String> = s.dialogs().into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["+1", "+2", "Anna"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
