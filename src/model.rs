//! I tipi neutri che l'interfaccia e lo stato (`App`) usano, uguali per
//! ogni backend (Telegram, WhatsApp): nessun tipo di grammers qui dentro.
//! Ogni backend traduce i suoi oggetti in questi e li riceve indietro
//! nelle `Request` (vedi `send.rs`).

use std::any::Any;
use std::sync::Arc;

use crate::i18n::tr;

/// Id di un messaggio dentro una chat (su WhatsApp il backend mappa gli
/// id testuali su questi numeri).
pub type MsgId = i32;

/// L'identità di una chat. Per Telegram è l'id «Bot API» (tipo di peer
/// impacchettato nel numero); gli altri backend scelgono numeri stabili
/// fuori dagli intervalli di Telegram. È anche la chiave persistita (chat a
/// traduzione automatica), quindi non deve cambiare fra un avvio e l'altro.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChatId(pub i64);

impl ChatId {
    /// Gli id dei messaggi valgono solo dentro questa chat (canali e
    /// supergruppi Telegram): una cancellazione senza chat non la tocca.
    pub fn own_msg_ids(self) -> bool {
        (-1997852516352..=-1000000000001).contains(&self.0)
            || (-4000000000000..=-2002147483649).contains(&self.0)
    }
}

/// Riferimento a una chat da passare al backend: identità più il permesso
/// del server (`access_hash` su Telegram, 0 dove non serve).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ChatRef {
    pub id: ChatId,
    pub auth: i64,
}

/// Un media del backend (foto, documento, sticker…) visto da fuori: la
/// chiave della cache, l'estensione dell'originale e l'oggetto del backend,
/// che solo lui sa scaricare o reinviare.
#[derive(Clone)]
pub struct MediaRef(Arc<MediaInner>);

struct MediaInner {
    key: String,
    ext: &'static str,
    emoji: String,
    inner: Box<dyn Any + Send + Sync>,
}

impl MediaRef {
    /// `key`: nome stabile per la cache; `ext`: estensione dell'originale
    /// ("jpg", "png", "webp", "gif" o "bin"); `emoji`: quella dello sticker.
    pub fn new<T: Any + Send + Sync>(key: String, ext: &'static str, emoji: String, inner: T) -> Self {
        Self(Arc::new(MediaInner { key, ext, emoji, inner: Box::new(inner) }))
    }

    pub fn key(&self) -> &str {
        &self.0.key
    }

    pub fn ext(&self) -> &'static str {
        self.0.ext
    }

    /// L'emoji dello sticker (vuota per gli altri media).
    pub fn emoji(&self) -> &str {
        &self.0.emoji
    }

    /// L'oggetto del backend, se è di quel tipo.
    pub fn get<T: Any>(&self) -> Option<&T> {
        self.0.inner.downcast_ref()
    }
}

impl std::fmt::Debug for MediaRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MediaRef({})", self.0.key)
    }
}

/// One conversation row in the sidebar.
#[derive(Clone)]
pub struct DialogInfo {
    pub name: String,
    pub peer_ref: ChatRef,
    pub peer_id: ChatId,
    pub is_group: bool,
    pub unread: usize,
    /// chat fissata in alto (preferiti): resta sopra le altre.
    pub pinned: bool,
    /// ultimo messaggio, per l'anteprima nella lista chat.
    pub last: Option<LastMsg>,
    /// i miei messaggi fino a questo id sono stati letti (spunte doppie).
    pub read_out: i32,
    /// canale (solo gli amministratori scrivono), non gruppo.
    pub broadcast: bool,
    /// silenziata: niente notifiche.
    pub muted: bool,
    /// foto profilo o della chat (id per la cache e il download).
    pub photo_id: Option<i64>,
    /// ultimo accesso (solo chat private).
    pub seen: Seen,
}

/// Lo stato di un utente come lo mostra Telegram.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub enum Seen {
    #[default]
    Unknown,
    /// online fino a (secondi Unix)
    Online(i64),
    /// ultimo accesso (secondi Unix)
    Offline(i64),
    Recently,
    LastWeek,
    LastMonth,
}

impl Seen {
    /// online adesso (l'«online» scade da solo dopo `expires`)
    pub fn online(&self, now: i64) -> bool {
        matches!(self, Seen::Online(until) if *until > now)
    }
}

/// L'anteprima dell'ultimo messaggio di una chat.
#[derive(Clone)]
pub struct LastMsg {
    pub id: i32,
    pub text: String,
    pub outgoing: bool,
    /// secondi Unix (0 = ignoto)
    pub date: i64,
}

impl LastMsg {
    pub fn from_row(r: &HistoryRow) -> Self {
        Self {
            id: r.id,
            text: r.kind.summary(),
            outgoing: r.outgoing,
            date: r.date,
        }
    }
}

/// One message rendered in the history pane.
pub struct HistoryRow {
    pub id: i32,
    pub outgoing: bool,
    /// nome del mittente (vuoto per i messaggi propri: si usa quello dell'utente).
    pub from: String,
    pub kind: RowKind,
    /// reazioni visibili: emoji standard (quelle custom si scartano).
    pub reactions: Vec<ReactionItem>,
    /// istante di invio, secondi Unix (0 = ignoto: messaggi locali).
    pub date: i64,
    /// id del messaggio a cui questo risponde.
    pub reply_to: Option<i32>,
    /// citazione parziale scelta da chi risponde (`quote_text`): se c'è, la
    /// riga di citazione mostra questa invece dell'inizio del messaggio.
    pub reply_quote: Option<String>,
    /// origine di un inoltro: nome dell'autore originale, vuoto se nascosto.
    pub fwd: Option<String>,
    pub edited: bool,
}

/// conteggio di una reazione standard sulla riga.
#[derive(Clone, Debug)]
pub struct ReactionItem {
    pub emoticon: String,
    pub count: i32,
    /// seleziona questa reazione al momento.
    pub mine: bool,
}

/// gli otto empresi veloci della scelta (tasti 1..8).
pub const QUICK_REACTIONS: [&str; 8] = ["👍", "❤", "🔥", "😂", "🎉", "😮", "🙏", "👎"];

pub enum RowKind {
    Text(String),
    Sticker {
        media: MediaRef,
        emoji: String,
        animated: bool,
    },
    /// foto Telegram, allegato immagine o gif: renderizzabili in kitty.
    Photo {
        media: MediaRef,
        caption: String,
        gif: bool,
    },
    /// documento scaricabile (Invio = salva in ~/Downloads).
    Doc {
        media: MediaRef,
        name: String,
        size: Option<usize>,
        caption: String,
    },
    /// audio riproducibile (messaggio vocale o brano): Ctrl-P lo riproduce.
    Audio {
        media: MediaRef,
        /// nome per i music/documenti; vuoto per i vocali.
        name: String,
        seconds: u32,
        caption: String,
        /// true: vocale (ogg opus senza nome).
        voice: bool,
    },
    /// video (anche tondo): miniatura in GUI, `note` è l'etichetta per la TUI.
    Video {
        media: MediaRef,
        seconds: u32,
        caption: String,
        round: bool,
        note: String,
    },
    /// etichetta informativa: vocale, contatti, sondaggi…
    Note(String),
    Service(String),
}

impl RowKind {
    /// Una riga sola che descrive il messaggio: anteprime e citazioni.
    pub fn summary(&self) -> String {
        match self {
            RowKind::Text(t) => t.lines().next().unwrap_or("").to_string(),
            RowKind::Sticker { emoji, .. } => format!("{emoji} sticker"),
            RowKind::Photo { caption, gif, .. } => {
                let what = if *gif { "GIF" } else { tr("🖼 foto") };
                if caption.is_empty() {
                    what.into()
                } else {
                    format!("{what} {caption}")
                }
            }
            RowKind::Doc { name, .. } => format!("📄 {name}"),
            RowKind::Audio { voice: true, .. } => tr("🎤 messaggio vocale").into(),
            RowKind::Audio { name, .. } => format!("🎵 {name}"),
            RowKind::Video { round: true, .. } => tr("📹 videomessaggio").into(),
            RowKind::Video { caption, .. } if !caption.is_empty() => format!("🎥 {caption}"),
            RowKind::Video { .. } => "🎥 video".into(),
            RowKind::Note(s) | RowKind::Service(s) => s.clone(),
        }
    }
}

/// Caption ripulita: righe collassate, tagliata a lunghezza ragionevole.
pub fn caption(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 48;
    let mut cut = String::new();
    for ch in one_line.chars() {
        if cut.chars().count() >= MAX {
            cut.push('…');
            break;
        }
        cut.push(ch);
    }
    cut
}

/// Dimensioni umane: 342 kB, 1,2 MB…
pub fn fmt_size(mut bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut unit = 0;
    while bytes >= 1024.0 && unit + 1 < UNITS.len() {
        bytes /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        // "1,2 MB" col separatore decimale della lingua: format! usa
        // sempre il punto, quindi arrotondo alla prima decimale a mano
        let round10 = (bytes * 10.0).round();
        let whole = (round10 / 10.0).trunc() as u64;
        let frac = round10 as i64 % 10;
        format!("{whole}{}{frac} {}", crate::i18n::decimal_sep(), UNITS[unit])
    }
}

/// durata in "m:ss" (o "h:mm:ss")
pub fn fmt_secs(seconds: u32) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// PNG e JPEG partono come foto (compresse dal servizio), il resto come file.
pub fn is_photo_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
}

/// Confronto fra chat (uguaglianza degli id).
pub fn same_peer(a: ChatId, b: ChatId) -> bool {
    a == b
}

/// One installed sticker set.
pub struct StickerSetInfo {
    pub set_id: i64,
    pub title: String,
    pub short_name: String,
}

/// Id finti per i «set» speciali: preferiti e usati di recente.
pub const FAVED_SET: i64 = -1;
pub const RECENT_SET: i64 = -2;

/// La scheda di una chat: descrizione, contatti, membri, blocco.
#[derive(Clone, Debug, Default)]
pub struct PeerInfo {
    /// bio dell'utente o descrizione del gruppo/canale
    pub about: String,
    pub username: Option<String>,
    pub phone: Option<String>,
    /// membri (gruppi e canali)
    pub members: Option<i32>,
    /// l'utente è bloccato da me (solo chat private)
    pub blocked: bool,
    pub bot: bool,
    /// la chat con me stesso (Messaggi salvati)
    pub me: bool,
}

/// Le schede dei contenuti condivisi di una chat.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SharedTab {
    Media,
    Files,
    Links,
    Audio,
    Voice,
}

impl SharedTab {
    pub const ALL: [SharedTab; 5] =
        [SharedTab::Media, SharedTab::Files, SharedTab::Links, SharedTab::Audio, SharedTab::Voice];

    pub fn label(self) -> &'static str {
        match self {
            SharedTab::Media => "Media",
            SharedTab::Files => tr("File"),
            SharedTab::Links => "Link",
            SharedTab::Audio => tr("Musica"),
            SharedTab::Voice => tr("Vocali"),
        }
    }
}

/// Un elemento dei contenuti condivisi: la riga e, per i link, gli URL.
pub struct SharedItem {
    pub row: HistoryRow,
    pub links: Vec<String>,
}

/// Il mio profilo, per le impostazioni.
#[derive(Clone, Debug, Default)]
pub struct MyProfile {
    pub first: String,
    pub last: String,
    pub username: String,
    pub phone: String,
    pub about: String,
    /// foto profilo piccola già in cache
    pub photo: Option<std::path::PathBuf>,
}

/// Una sessione attiva dell'account (un dispositivo collegato).
#[derive(Clone, Debug)]
pub struct Session {
    pub hash: i64,
    pub current: bool,
    pub device: String,
    pub app: String,
    pub place: String,
    pub ip: String,
    /// ultima attività, secondi Unix
    pub active: i64,
}

/// Un colpo da ricerca: id, data, autore e testo potentemente tagsliato.
pub struct SearchHit {
    pub outgoing: bool,
    pub from: String,
    pub date: String,
    pub snippet: String,
}

/// Esito della risoluzione di un link.
pub enum LinkHit {
    /// chat da aprire (dialogo nuovo se non era fra i tuoi)
    Chat { dialog: DialogInfo, post: Option<i32> },
    /// invito a una chat di cui sei già membro
    Already(ChatId),
    /// invito da accettare: titolo e membri per la conferma
    Invite { hash: String, title: String, members: i32 },
    /// invito accettato: la chat (se l'update la nomina)
    Joined(Option<ChatId>),
}

/// Quello che succede sul server mentre l'app è aperta, tradotto dal
/// backend: messaggi nuovi, modifiche, letture, «sta scrivendo»…
// i payload viaggiano interi, una volta per evento (come in send.rs)
#[allow(clippy::large_enum_variant)]
pub enum Live {
    /// messaggio nuovo (anche mio, da un altro dispositivo); `dialog` è la
    /// voce di lista pronta se il backend l'ha già (chat mai vista)
    NewMessage {
        chat: ChatId,
        row: HistoryRow,
        dialog: Option<DialogInfo>,
    },
    Edited { chat: ChatId, row: HistoryRow },
    /// messaggi cancellati; senza chat valgono per tutte quelle con id
    /// condivisi (vedi `ChatId::own_msg_ids`)
    Deleted { chat: Option<ChatId>, ids: Vec<i32> },
    Reactions { chat: ChatId, msg: i32, items: Vec<ReactionItem> },
    /// letta altrove (telefono, web): restano `still` non letti
    ReadInbox { chat: ChatId, still: i32 },
    /// l'altro ha letto i miei messaggi fino a `max_id` (spunte doppie)
    ReadOutbox { chat: ChatId, max_id: i32 },
    Blocked { chat: ChatId, blocked: bool },
    Seen { chat: ChatId, seen: Seen },
    /// «sta scrivendo»: `who` nei gruppi, `what` = None se ha smesso
    Typing {
        chat: ChatId,
        who: Option<ChatId>,
        what: Option<&'static str>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- fmt_size / fmt_secs ----------
    #[test]
    fn size_unit() {
        assert_eq!(fmt_size(342.0), "342 B");
        assert_eq!(fmt_size(1024.0), "1,0 kB");
        assert_eq!(fmt_size(1024.0 * 1024.0 * 1.2), "1,2 MB");
        assert_eq!(fmt_size(1024.0 * 1024.0 * 1024.0 * 15.75), "15,8 GB");
        assert_eq!(fmt_size(0.0), "0 B");
    }

    #[test]
    fn secs_clock() {
        assert_eq!(fmt_secs(0), "0:00");
        assert_eq!(fmt_secs(91), "1:31");
        assert_eq!(fmt_secs(3800), "1:03:20");
    }

    // ---------- caption ----------
    #[test]
    fn caption_collapses_and_cuts() {
        assert_eq!(caption("  a   b\t c "), "a b c");
        let long = "x".repeat(200);
        let cut = caption(&long);
        assert!(cut.ends_with('…'));
        assert!(cut.chars().count() < 200);
    }

    #[test]
    fn own_msg_ids_solo_canali() {
        // utente, gruppo piccolo, canale (formato Bot API)
        assert!(!ChatId(777).own_msg_ids());
        assert!(!ChatId(-4242).own_msg_ids());
        assert!(ChatId(-1001234567890).own_msg_ids());
    }

    #[test]
    fn media_ref_porta_l_oggetto_del_backend() {
        let m = MediaRef::new("k".into(), "png", "🙂".into(), 42u32);
        assert_eq!(m.key(), "k");
        assert_eq!(m.ext(), "png");
        assert_eq!(m.emoji(), "🙂");
        assert_eq!(m.get::<u32>(), Some(&42));
        assert!(m.get::<String>().is_none());
    }
}
