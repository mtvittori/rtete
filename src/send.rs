//! Protocollo fra UI e worker: `Request` in invio, `Response` in ritorno, i job di invio.

use std::path::PathBuf;

use crate::clipboard::ClipboardHit;
use crate::links::TgLink;
use crate::model::{
    ChatId, ChatRef, HistoryRow, Live, LinkHit, MediaRef, MyProfile, SearchHit, SharedItem, SharedTab, StickerSetInfo,
    DialogInfo, PeerInfo, Session,
};

pub struct SendJob {
    pub peer: ChatRef,
    pub peer_id: ChatId,
    pub kind: SendKind,
    /// risposta a questo messaggio (Ctrl-R); ignorato dalle modifiche
    pub reply_to: Option<i32>,
}

/// esito: nuovo messaggio o modifica confermata
pub enum SendDone {
    New(ChatId, HistoryRow),
    Edit(ChatId, HistoryRow),
}

// I payload servono interi sui canali mpsc: il boxing aggiungerebbe solo
// allocazioni, la dimensione dell'enum non è un collo di bottiglia.
#[allow(clippy::large_enum_variant)]
pub enum SendKind {
    Text(String),
    Sticker(MediaRef),
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
        peer_ref: ChatRef,
        peer: ChatId,
    },
    /// pagina precedente: messaggi prima di `before`
    HistOlder {
        peer_ref: ChatRef,
        peer: ChatId,
        before: i32,
    },
    /// messaggi citati da risposte ma assenti dalla cronologia caricata
    Quoted {
        peer_ref: ChatRef,
        peer: ChatId,
        ids: Vec<i32>,
    },
    /// la chat aperta è stata letta (fuori dal flusso eventi)
    MarkRead(ChatRef),
    Sets,
    SetDocs {
        set_id: i64,
        short: String,
    },
    /// conversione png: il dispatcher la limita con un semaforo a 2 posti
    Conv(MediaRef),
    Send(SendJob),
    Del {
        peer_ref: ChatRef,
        id: i32,
    },
    Search {
        peer_ref: ChatRef,
        query: String,
    },
    Save {
        media: MediaRef,
        name: String,
    },
    Play {
        media: MediaRef,
        fname: String,
        label: String,
        /// true: apre la finestra video (mpv), false: solo audio
        video: bool,
    },
    /// inoltra un messaggio da una chat a un'altra
    Forward {
        from: ChatRef,
        id: i32,
        to: ChatRef,
        to_name: String,
    },
    React {
        peer_ref: ChatRef,
        msg_id: i32,
        emoticon: Option<String>,
    },
    /// link Telegram da risolvere (chat pubblica o invito)
    Link(TgLink),
    /// accetta l'invito confermato
    Join(String),
    /// rilegge la lista dei dialoghi (dopo un ingresso in una chat)
    Dialogs,
    /// foto profilo piccola di una chat
    Avatar { peer_ref: ChatRef, photo_id: i64 },
    /// scheda della chat (bio, membri, bloccato)
    Info { peer_ref: ChatRef, peer: ChatId },
    /// contenuti condivisi di un tipo (media, file, link…)
    Shared {
        peer_ref: ChatRef,
        peer: ChatId,
        tab: SharedTab,
    },
    /// blocca (true) o sblocca un utente
    Block {
        peer_ref: ChatRef,
        peer: ChatId,
        blocked: bool,
    },
    /// elimina la chat (o esce dal gruppo); `revoke`: anche per l'altro
    DeleteChat {
        peer_ref: ChatRef,
        peer: ChatId,
        revoke: bool,
    },
    /// il mio profilo (impostazioni)
    Profile,
    /// salva nome, bio e username (`old_username`: quello di prima)
    SaveProfile {
        profile: MyProfile,
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
        peer: ChatId,
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
        peer: ChatId,
        rows: Result<Vec<HistoryRow>, String>,
    },
    HistOlder {
        peer: ChatId,
        rows: Result<Vec<HistoryRow>, String>,
    },
    /// per id: la riga, o `None` se il messaggio non esiste più
    Quoted {
        peer: ChatId,
        rows: Result<Vec<(i32, Option<HistoryRow>)>, String>,
    },
    Sets(Result<Vec<StickerSetInfo>, String>),
    Docs {
        set_id: i64,
        docs: Result<Vec<MediaRef>, String>,
    },
    Converted((String, Result<PathBuf, String>)),
    Sent(Result<SendDone, String>),
    Deleted(Result<(), String>),
    Searched(Result<Vec<SearchHit>, String>),
    Saved(Result<String, String>),
    Played(Result<String, String>),
    Reacted(Result<(), String>),
    Link(Result<LinkHit, String>),
    Forwarded(Result<String, String>),
    Dialogs(Result<Vec<DialogInfo>, String>),
    Avatar { photo_id: i64, path: Result<PathBuf, String> },
    Info {
        peer: ChatId,
        info: Result<PeerInfo, String>,
    },
    Shared {
        peer: ChatId,
        tab: SharedTab,
        items: Result<Vec<SharedItem>, String>,
    },
    Blocked {
        peer: ChatId,
        res: Result<bool, String>,
    },
    ChatDeleted {
        peer: ChatId,
        res: Result<(), String>,
    },
    Profile(Result<MyProfile, String>),
    /// profilo o foto salvati
    ProfileSaved(Result<(), String>),
    Sessions(Result<Vec<Session>, String>),
    SessionEnded(Result<Option<i64>, String>),
    Translated {
        peer: ChatId,
        id: i32,
        auto: bool,
        target: String,
        res: Result<crate::translate::Translated, String>,
    },
    TranslateTest(Result<crate::translate::Translated, String>),
    /// il probe wl-clipboard ha girato a thread fuori dal ciclo eventi
    Clipboard(ClipboardHit),
    /// un evento live dal server (messaggi nuovi, letture…)
    Live(Live),
    /// lo stream degli eventi live ha avuto un errore
    LiveErr(String),
}
