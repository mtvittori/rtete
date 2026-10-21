//! Protocollo fra UI e worker: `Request` in invio, `Response` in ritorno, i job di invio.

use std::path::PathBuf;

use grammers_client::media::Media;
use grammers_client::session::types::PeerId;

use crate::clipboard::ClipboardHit;
use crate::tg;

pub(crate) type PeerRefT = grammers_client::session::types::PeerRef;

pub(crate) struct SendJob {
    pub(crate) peer: PeerRefT,
    pub(crate) peer_id: PeerId,
    pub(crate) kind: SendKind,
    /// risposta a questo messaggio (Ctrl-R); ignorato dalle modifiche
    pub(crate) reply_to: Option<i32>,
}

/// esito: nuovo messaggio o modifica confermata
pub(crate) enum SendDone {
    New(PeerId, tg::HistoryRow),
    Edit(PeerId, tg::HistoryRow),
}

// I payload servono interi sui canali mpsc: il boxing aggiungerebbe solo
// allocazioni, la dimensione dell'enum non è un collo di bottiglia.
#[allow(clippy::large_enum_variant)]
pub(crate) enum SendKind {
    Text(String),
    Sticker(Media),
    /// foto dalla clipboard (file già salvato in tmp) con eventuale caption.
    Photo { path: String, caption: String },
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
pub(crate) enum Request {
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
    },
    React {
        peer_ref: PeerRefT,
        msg_id: i32,
        emoticon: Option<String>,
    },
}

/// Risposte dei worker: un solo canale verso il ciclo; ogni variante ha
/// il suo aggiornamento di stato (le voci della vecchia LoopEvent restano).
// Come SendKind: i payload viaggiano interi, il boxing aggiungerebbe solo
// allocazioni.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Response {
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
    /// il probe wl-clipboard ha girato a thread fuori dal ciclo eventi
    Clipboard(ClipboardHit),
}
