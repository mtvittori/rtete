//! Protocollo fra UI e worker: `Request` in invio, `Response` in ritorno, i job di invio.

use std::path::PathBuf;

use grammers_client::media::Media;
use grammers_client::session::types::PeerId;

use crate::clipboard::ClipboardHit;
use crate::tg;

pub type PeerRefT = grammers_client::session::types::PeerRef;

pub struct SendJob {
    pub peer: PeerRefT,
    pub peer_id: PeerId,
    pub kind: SendKind,
    /// risposta a questo messaggio (Ctrl-R); ignorato dalle modifiche
    pub reply_to: Option<i32>,
}

/// esito: nuovo messaggio o modifica confermata
pub enum SendDone {
    New(PeerId, tg::HistoryRow),
    Edit(PeerId, tg::HistoryRow),
}

// I payload servono interi sui canali mpsc: il boxing aggiungerebbe solo
// allocazioni, la dimensione dell'enum non è un collo di bottiglia.
#[allow(clippy::large_enum_variant)]
pub enum SendKind {
    Text(String),
    Sticker(Media),
    /// foto dalla clipboard (file già salvato in tmp) con eventuale caption.
    Photo { path: String, caption: String },
    /// file scelto dall'utente (selettore, appunti, trascinato): le
    /// immagini partono come foto, il resto come documento; il file resta.
    File { path: String, caption: String },
    /// modifica di un testo già inviato (id invariato).
    EditText {
        id: i32,
        text: String,
    },
}

#[allow(clippy::too_many_arguments)]
/// Richieste verso i worker: un solo canale in ingresso (prima dieci
/// coppie mpsc e un `waiting_pick` che girava a mano pescando dal primo
/// canale che rispondeva, con `.expect("servizio chiuso")` ovunque).
pub enum Request {
    Hist {
        peer_ref: PeerRefT,
        peer: PeerId,
    },
    /// pagina precedente: messaggi prima di `before`
    HistOlder {
        peer_ref: PeerRefT,
        peer: PeerId,
        before: i32,
    },
    /// messaggi citati da risposte ma assenti dalla cronologia caricata
    Quoted {
        peer_ref: PeerRefT,
        peer: PeerId,
        ids: Vec<i32>,
    },
    Sets,
    SetDocs {
        set_id: i64,
        short: String,
    },
    /// conversione png: il dispatcher la limita con un semaforo a 2 posti
    Conv(Media),
    Send(SendJob),
    Del {
        peer_ref: PeerRefT,
        id: i32,
    },
    Search {
        peer_ref: PeerRefT,
        query: String,
    },
    Save {
        media: Media,
        name: String,
    },
    Play {
        media: Media,
        fname: String,
        label: String,
        /// true: apre la finestra video (mpv), false: solo audio
        video: bool,
    },
    /// inoltra un messaggio da una chat a un'altra
    Forward {
        from: PeerRefT,
        id: i32,
        to: PeerRefT,
        to_name: String,
    },
    React {
        peer_ref: PeerRefT,
        msg_id: i32,
        emoticon: Option<String>,
    },
    /// link Telegram da risolvere (chat pubblica o invito)
    Link(tg::TgLink),
    /// accetta l'invito confermato
    Join(String),
    /// rilegge la lista dei dialoghi (dopo un ingresso in una chat)
    Dialogs,
    /// foto profilo piccola di una chat
    Avatar { peer_ref: PeerRefT, photo_id: i64 },
    /// scheda della chat (bio, membri, bloccato)
    Info { peer_ref: PeerRefT, peer: PeerId },
    /// contenuti condivisi di un tipo (media, file, link…)
    Shared {
        peer_ref: PeerRefT,
        peer: PeerId,
        tab: tg::SharedTab,
    },
    /// blocca (true) o sblocca un utente
    Block {
        peer_ref: PeerRefT,
        peer: PeerId,
        blocked: bool,
    },
    /// elimina la chat (o esce dal gruppo); `revoke`: anche per l'altro
    DeleteChat {
        peer_ref: PeerRefT,
        peer: PeerId,
        revoke: bool,
    },
    /// il mio profilo (impostazioni)
    Profile,
    /// salva nome, bio e username (`old_username`: quello di prima)
    SaveProfile {
        profile: tg::MyProfile,
        old_username: String,
    },
    /// nuova foto profilo da un file
    ProfilePhoto(PathBuf),
    /// i dispositivi collegati
    Sessions,
    /// chiude una sessione, o tutte le altre con `None`
    EndSession(Option<i64>),
    /// traduce il testo di un messaggio (`auto`: chat tradotta da sola)
    Translate {
        peer: PeerId,
        id: i32,
        text: String,
        cfg: Box<crate::translate::Config>,
        auto: bool,
    },
    /// traduce una frase di prova (Impostazioni → Traduzione)
    TranslateTest(Box<crate::translate::Config>),
}

/// Risposte dei worker: un solo canale verso il ciclo; ogni variante ha
/// il suo aggiornamento di stato (le voci della vecchia LoopEvent restano).
// Come SendKind: i payload viaggiano interi, il boxing aggiungerebbe solo
// allocazioni.
#[allow(clippy::large_enum_variant)]
pub enum Response {
    Hist {
        peer: PeerId,
        rows: Result<Vec<tg::HistoryRow>, String>,
    },
    HistOlder {
        peer: PeerId,
        rows: Result<Vec<tg::HistoryRow>, String>,
    },
    /// per id: la riga, o `None` se il messaggio non esiste più
    Quoted {
        peer: PeerId,
        rows: Result<Vec<(i32, Option<tg::HistoryRow>)>, String>,
    },
    Sets(Result<Vec<tg::StickerSetInfo>, String>),
    Docs {
        set_id: i64,
        docs: Result<Vec<Media>, String>,
    },
    Converted((String, Result<PathBuf, String>)),
    Sent(Result<SendDone, String>),
    Deleted(Result<(), String>),
    Searched(Result<Vec<tg::SearchHit>, String>),
    Saved(Result<String, String>),
    Played(Result<String, String>),
    Reacted(Result<(), String>),
    Link(Result<tg::LinkHit, String>),
    Forwarded(Result<String, String>),
    Dialogs(Result<Vec<tg::DialogInfo>, String>),
    Avatar { photo_id: i64, path: Result<PathBuf, String> },
    Info {
        peer: PeerId,
        info: Result<tg::PeerInfo, String>,
    },
    Shared {
        peer: PeerId,
        tab: tg::SharedTab,
        items: Result<Vec<tg::SharedItem>, String>,
    },
    Blocked {
        peer: PeerId,
        res: Result<bool, String>,
    },
    ChatDeleted {
        peer: PeerId,
        res: Result<(), String>,
    },
    Profile(Result<tg::MyProfile, String>),
    /// profilo o foto salvati
    ProfileSaved(Result<(), String>),
    Sessions(Result<Vec<tg::Session>, String>),
    SessionEnded(Result<Option<i64>, String>),
    Translated {
        peer: PeerId,
        id: i32,
        auto: bool,
        target: String,
        res: Result<crate::translate::Translated, String>,
    },
    TranslateTest(Result<crate::translate::Translated, String>),
    /// il probe wl-clipboard ha girato a thread fuori dal ciclo eventi
    Clipboard(ClipboardHit),
}
