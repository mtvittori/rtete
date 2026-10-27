//! WhatsApp backend: whatsapp-rust 0.7 (protocollo multi-device, come
//! WhatsApp Web; non ufficiale). Rondine si collega come «dispositivo
//! collegato»: abbinamento con QR (o codice), poi la sessione resta in
//! `whatsapp.db`.
//!
//! Il server non conserva la cronologia: i messaggi arrivano una volta
//! (history sync all'abbinamento, poi quelli nuovi) e li tiene `store`.
//! Come per Telegram, fuori escono solo i tipi neutri di `crate::model`.

pub mod live;
pub mod store;
pub mod worker;

use std::io::Write;
use std::sync::{Arc, Mutex};

use base64::Engine;
use tokio::sync::mpsc;
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore::download::MediaType;
use whatsapp_rust::waproto::buffa::Message as _;

use crate::i18n::tr;
use crate::model::{DialogInfo, HistoryRow, MediaRef, ReactionItem, RowKind};
use crate::send::{Request, Response};
use store::{chat_id_of, Store, StoredMsg};

/// Lo stato condiviso fra eventi e richieste: il client e l'archivio.
#[derive(Clone)]
pub struct Wa {
    pub client: Arc<Client>,
    pub store: Arc<Mutex<Store>>,
}

impl Wa {
    pub fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        // un panic con il lucchetto preso non deve fermare tutto il resto
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Il media di un messaggio WhatsApp dentro un `MediaRef`: il protobuf
/// col link al CDN e le chiavi per decifrarlo.
pub struct WaMedia {
    pub msg: wa::Message,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------- dai protobuf all'archivio ----------------------------

/// Il contesto (risposta, inoltro) del messaggio, qualunque sia il tipo.
fn context_of(m: &wa::Message) -> Option<&wa::ContextInfo> {
    [
        m.extended_text_message.as_option().and_then(|x| x.context_info.as_option()),
        m.image_message.as_option().and_then(|x| x.context_info.as_option()),
        m.video_message.as_option().and_then(|x| x.context_info.as_option()),
        m.audio_message.as_option().and_then(|x| x.context_info.as_option()),
        m.document_message.as_option().and_then(|x| x.context_info.as_option()),
        m.sticker_message.as_option().and_then(|x| x.context_info.as_option()),
    ]
    .into_iter()
    .flatten()
    .next()
}

/// Un messaggio WhatsApp come riga dell'archivio (senza id, lo assegna lo
/// store). `None` per quelli che non si mostrano (reazioni, protocollo…).
pub fn stored_from(msg: &wa::Message, wa_id: &str, me: bool, sender: &str, name: &str, ts: i64) -> Option<StoredMsg> {
    let m = msg.get_base_message();
    let mut out = StoredMsg {
        wa: wa_id.to_string(),
        me,
        sender: sender.to_string(),
        name: name.to_string(),
        ts,
        ..Default::default()
    };
    let media = || Some(base64::engine::general_purpose::STANDARD.encode(m.encode_to_vec()));
    if let Some(img) = m.image_message.as_option() {
        out.kind = "image".into();
        out.mime = img.mimetype.clone().unwrap_or_default();
        out.size = img.file_length.unwrap_or(0);
        out.media = media();
    } else if let Some(st) = m.sticker_message.as_option() {
        out.kind = "sticker".into();
        out.mime = st.mimetype.clone().unwrap_or_default();
        out.flag = st.is_animated.unwrap_or(false);
        out.media = media();
    } else if let Some(v) = m.video_message.as_option() {
        out.kind = "video".into();
        out.secs = v.seconds.unwrap_or(0);
        out.flag = v.gif_playback.unwrap_or(false);
        out.size = v.file_length.unwrap_or(0);
        out.mime = v.mimetype.clone().unwrap_or_default();
        out.media = media();
    } else if let Some(a) = m.audio_message.as_option() {
        out.kind = if a.ptt.unwrap_or(false) { "voice" } else { "audio" }.into();
        out.secs = a.seconds.unwrap_or(0);
        out.mime = a.mimetype.clone().unwrap_or_default();
        out.media = media();
    } else if let Some(d) = m.document_message.as_option() {
        out.kind = "doc".into();
        out.file = d.file_name.clone().or_else(|| d.title.clone()).unwrap_or_else(|| "file".into());
        out.size = d.file_length.unwrap_or(0);
        out.mime = d.mimetype.clone().unwrap_or_default();
        out.media = media();
    } else if let Some(t) = m.text_content() {
        out.kind = "text".into();
        out.text = t.to_string();
    } else if m.location_message.is_set() || m.live_location_message.is_set() {
        out.kind = "note".into();
        out.text = tr("📍 posizione").into();
    } else {
        // contatto condiviso, altrimenti niente da mostrare
        let c = m.contact_message.as_option()?;
        out.kind = "note".into();
        out.text = crate::trf!("📇 contatto: {} {}", c.display_name.clone().unwrap_or_default(), "");
    }
    if out.kind != "text" && out.kind != "note" {
        out.text = m.get_caption().unwrap_or_default().to_string();
    }
    if let Some(ctx) = context_of(m) {
        out.reply = ctx.stanza_id.clone().filter(|s| !s.is_empty());
        out.fwd = ctx.is_forwarded.unwrap_or(false);
    }
    Some(out)
}

// ---------------- dall'archivio ai tipi neutri -------------------------

/// Estensione dell'originale in cache.
fn media_ext(m: &StoredMsg) -> &'static str {
    match (m.kind.as_str(), m.mime.as_str()) {
        ("sticker", _) => "webp",
        // del video si mostra la miniatura (jpeg)
        ("video", _) => "jpg",
        ("image", "image/png") => "png",
        ("image", "image/webp") => "webp",
        ("image", "image/gif") => "gif",
        ("image", _) => "jpg",
        _ => "bin",
    }
}

pub(crate) fn media_ref(jid: &str, m: &StoredMsg) -> Option<MediaRef> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(m.media.as_deref()?).ok()?;
    let msg = wa::Message::decode_from_slice(&bytes).ok()?;
    let key = format!("{:x}", md5::compute(format!("wa:{jid}:{}", m.wa)));
    let emoji = if m.kind == "sticker" { "🙂".to_string() } else { String::new() };
    Some(MediaRef::new(key, media_ext(m), emoji, WaMedia { msg }))
}

/// Una riga dell'archivio come la vede l'app.
pub fn row_of(store: &mut Store, jid: &str, m: &StoredMsg) -> HistoryRow {
    let group = store.chat(jid).is_some_and(|c| c.group);
    let from = if m.me {
        String::new()
    } else if group {
        match store.display_name(&m.sender) {
            n if n.starts_with('+') && !m.name.is_empty() => m.name.clone(),
            n => n,
        }
    } else {
        store.display_name(jid)
    };
    let reply_to = m.reply.as_deref().and_then(|wa| store.by_wa(jid, wa)).map(|r| r.id);
    let mut reactions: Vec<ReactionItem> = Vec::new();
    for (who, emoji) in &m.reactions {
        match reactions.iter_mut().find(|r| &r.emoticon == emoji) {
            Some(r) => {
                r.count += 1;
                r.mine |= who == "me";
            }
            None => reactions.push(ReactionItem { emoticon: emoji.clone(), count: 1, mine: who == "me" }),
        }
    }
    HistoryRow {
        id: m.id,
        outgoing: m.me,
        from,
        kind: row_kind(jid, m),
        reactions,
        date: m.ts,
        reply_to,
        reply_quote: None,
        fwd: m.fwd.then(String::new),
        edited: m.edited,
    }
}

fn row_kind(jid: &str, m: &StoredMsg) -> RowKind {
    if m.deleted {
        return RowKind::Note(tr("🚫 messaggio eliminato").into());
    }
    let caption = crate::model::caption(&m.text);
    let media = media_ref(jid, m);
    match (m.kind.as_str(), media) {
        ("image", Some(media)) => RowKind::Photo { media, caption, gif: m.mime == "image/gif" },
        ("sticker", Some(media)) => RowKind::Sticker { media, emoji: "🙂".into(), animated: m.flag },
        ("video", Some(media)) => RowKind::Video {
            media,
            seconds: m.secs,
            caption,
            round: false,
            note: format!("video {}s", m.secs),
        },
        ("voice" | "audio", Some(media)) => RowKind::Audio {
            media,
            name: if m.kind == "voice" { String::new() } else { "audio".into() },
            seconds: m.secs,
            caption,
            voice: m.kind == "voice",
        },
        ("doc", Some(media)) => RowKind::Doc {
            media,
            name: m.file.clone(),
            size: Some(m.size as usize),
            caption,
        },
        ("text", _) => RowKind::Text(m.text.clone()),
        ("note", _) => RowKind::Note(m.text.clone()),
        _ if m.text.is_empty() => RowKind::Service(String::new()),
        _ => RowKind::Text(m.text.clone()),
    }
}

/// La voce di lista di una chat.
pub fn dialog_of(store: &Store, jid: &str) -> Option<DialogInfo> {
    let id = chat_id_of(jid);
    store.dialogs().into_iter().find(|d| d.peer_id == id)
}

// ---------------- media ------------------------------------------------

/// Scarica e decifra il media dal CDN.
pub async fn download(client: &Client, media: &MediaRef) -> Result<Vec<u8>, String> {
    let m = &media.get::<WaMedia>().ok_or("media non WhatsApp")?.msg;
    let res = if let Some(x) = m.image_message.as_option() {
        client.download(x).await
    } else if let Some(x) = m.sticker_message.as_option() {
        client.download(x).await
    } else if let Some(x) = m.video_message.as_option() {
        client.download(x).await
    } else if let Some(x) = m.audio_message.as_option() {
        client.download(x).await
    } else if let Some(x) = m.document_message.as_option() {
        client.download(x).await
    } else {
        return Err(tr("media non supportato").into());
    };
    res.map_err(|e| e.to_string())
}

/// Scarica in un file (salvataggi, player).
pub async fn download_to(client: &Client, media: &MediaRef, dest: std::path::PathBuf) -> Result<(), String> {
    let bytes = download(client, media).await?;
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    tokio::fs::write(&dest, bytes).await.map_err(|e| e.to_string())
}

/// L'originale per l'anteprima, in cache: dei video la miniatura che
/// viaggia già dentro il messaggio, degli altri il file intero.
pub async fn fetch_raw(client: &Client, media: &MediaRef) -> Result<std::path::PathBuf, String> {
    let path = crate::loader::cache_raw(media.key(), media.ext());
    if path.exists() {
        return Ok(path);
    }
    crate::paths::ensure_private_dir(&crate::paths::media_dir()).map_err(|e| format!("cache dir: {e}"))?;
    let m = &media.get::<WaMedia>().ok_or("media non WhatsApp")?.msg;
    let bytes = match m.video_message.as_option() {
        Some(v) => v.jpeg_thumbnail.clone().ok_or("video senza miniatura")?,
        None => download(client, media).await?,
    };
    tokio::fs::write(&path, bytes).await.map_err(|e| format!("cache: {e}"))?;
    Ok(path)
}

/// Carica un file e prepara il messaggio: foto (png/jpg) o documento.
pub async fn upload_message(client: &Client, path: &std::path::Path, caption: &str) -> Result<wa::Message, String> {
    let data = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
    let photo = crate::model::is_photo_file(path);
    let mime = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        _ => "application/octet-stream",
    };
    let kind = if photo { MediaType::Image } else { MediaType::Document };
    let up = client
        .upload(data, kind, Default::default())
        .await
        .map_err(|e| format!("upload: {e}"))?;
    let caption = (!caption.trim().is_empty()).then(|| caption.to_string());
    let mut msg = wa::Message::default();
    if photo {
        msg.image_message = MessageField::some(wa::message::ImageMessage {
            url: Some(up.url),
            direct_path: Some(up.direct_path),
            media_key: Some(up.media_key.to_vec()),
            file_enc_sha256: Some(up.file_enc_sha256.to_vec()),
            file_sha256: Some(up.file_sha256.to_vec()),
            file_length: Some(up.file_length),
            media_key_timestamp: Some(up.media_key_timestamp),
            mimetype: Some(mime.into()),
            caption,
            ..Default::default()
        });
    } else {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
        msg.document_message = MessageField::some(wa::message::DocumentMessage {
            url: Some(up.url),
            direct_path: Some(up.direct_path),
            media_key: Some(up.media_key.to_vec()),
            file_enc_sha256: Some(up.file_enc_sha256.to_vec()),
            file_sha256: Some(up.file_sha256.to_vec()),
            file_length: Some(up.file_length),
            media_key_timestamp: Some(up.media_key_timestamp),
            mimetype: Some(mime.into()),
            file_name: Some(name.clone()),
            title: Some(name),
            caption,
            ..Default::default()
        });
    }
    Ok(msg)
}

// ---------------- accesso e avvio --------------------------------------

/// Cosa mostrare all'utente mentre si collega WhatsApp: la TUI lo stampa
/// nel terminale, la GUI lo disegna nella finestra.
#[derive(Clone, Debug, PartialEq)]
pub enum Pairing {
    /// il QR da inquadrare col telefono (il testo da codificare)
    Qr(String),
    /// il codice di 8 caratteri (abbinamento col numero)
    Code(String),
    /// abbinato: arrivano le chat dal telefono
    Paired,
    /// una riga di stato («mi collego…»)
    Status(String),
}

pub type PairSink = Arc<dyn Fn(Pairing) + Send + Sync>;

/// Il terminale come destinazione (TUI).
pub fn terminal_sink() -> PairSink {
    Arc::new(|p| match p {
        Pairing::Qr(code) => print_qr(&code),
        Pairing::Code(code) => {
            println!("{}", crate::trf!("Codice di abbinamento: {} (WhatsApp → Dispositivi collegati → Collega con numero)", code))
        }
        Pairing::Paired => println!("{}", tr("Abbinato! Scarico le chat dal telefono…")),
        Pairing::Status(s) => println!("{s}"),
    })
}

/// Esiste già una sessione WhatsApp su questo PC (abbinata in passato)?
pub fn has_session() -> bool {
    crate::paths::data_dir().join("whatsapp").join("whatsapp.db").exists()
}

/// Disegna il QR dell'abbinamento nel terminale (mezzi blocchi Unicode).
fn print_qr(code: &str) {
    use qrcode::render::unicode::Dense1x2;
    match qrcode::QrCode::new(code.as_bytes()) {
        Ok(qr) => {
            let img = qr.render::<Dense1x2>().quiet_zone(true).build();
            println!("\n{img}");
        }
        Err(_) => println!("{code}"),
    }
    println!("{}", tr("WhatsApp sul telefono → Dispositivi collegati → Collega un dispositivo, poi inquadra il QR."));
    let _ = std::io::stdout().flush();
}

/// Si collega a WhatsApp (abbinamento la prima volta, nel terminale) e
/// avvia il backend: richieste da `req_rx`, risposte ed eventi su `res_tx`.
/// Restituisce le chat salvate e il mio nome.
pub async fn start(
    req_rx: mpsc::UnboundedReceiver<Request>,
    res_tx: mpsc::UnboundedSender<Response>,
) -> Result<(Vec<DialogInfo>, String), Box<dyn std::error::Error>> {
    start_with(req_rx, res_tx, terminal_sink(), crate::env_var("WA_PHONE")).await
}

/// Come `start`, con QR e avvisi dell'abbinamento mandati a `pair`;
/// con `phone` l'abbinamento usa il codice di 8 caratteri invece del QR.
pub async fn start_with(
    req_rx: mpsc::UnboundedReceiver<Request>,
    res_tx: mpsc::UnboundedSender<Response>,
    pair: PairSink,
    phone: Option<String>,
) -> Result<(Vec<DialogInfo>, String), Box<dyn std::error::Error>> {
    let dir = crate::paths::data_dir().join("whatsapp");
    crate::paths::ensure_private_dir(&dir)?;
    let store = Arc::new(Mutex::new(Store::open(&dir)?));
    let db = dir.join("whatsapp.db");
    let backend = whatsapp_rust_sqlite_storage::SqliteStore::new(db.to_string_lossy().as_ref()).await?;

    let (ready_tx, mut ready_rx) = mpsc::unbounded_channel::<Result<(), String>>();
    let shared = live::Shared::new(Arc::clone(&store), res_tx.clone(), ready_tx, Arc::clone(&pair));
    let on_qr = Arc::clone(&pair);
    let mut builder = Bot::builder()
        .with_backend(backend)
        .on_qr_code(move |code, _timeout| {
            let on_qr = Arc::clone(&on_qr);
            async move { on_qr(Pairing::Qr(code)) }
        })
        .on_event({
            let shared = shared.clone();
            move |event, client| {
                let shared = shared.clone();
                async move { shared.handle(&event, &client) }
            }
        });
    // abbinamento col codice invece del QR
    if let Some(phone) = phone.filter(|p| p.chars().any(|c| c.is_ascii_digit())) {
        let phone: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
        builder = builder
            .with_pair_code(whatsapp_rust::wacore::pair_code::PairCodeOptions {
                phone_number: phone,
                ..Default::default()
            })
            .on_pair_code({
                let pair = Arc::clone(&pair);
                move |code, _timeout| {
                    let pair = Arc::clone(&pair);
                    async move { pair(Pairing::Code(code)) }
                }
            });
    }
    let bot = builder.build().await?;
    pair(Pairing::Status(tr("Mi collego a WhatsApp…").into()));
    let handle = bot.spawn();
    let client = handle.client();
    // il bot vive finché vive l'app (droppare l'handle lo fermerebbe)
    tokio::spawn(handle);
    match ready_rx.recv().await {
        Some(Ok(())) => {}
        Some(Err(e)) => return Err(e.into()),
        None => return Err(tr("connessione a WhatsApp chiusa").into()),
    }
    let wa = Wa { client: Arc::clone(&client), store };
    let contacts = wa.store().dialogs();
    let me = client.push_name();
    worker::run_worker(wa, req_rx, res_tx);
    Ok((contacts, me))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &str) -> wa::Message {
        wa::Message { conversation: Some(t.into()), ..Default::default() }
    }

    #[test]
    fn testo_e_risposta() {
        let m = stored_from(&text("ciao"), "ID1", false, "1@s.whatsapp.net", "Anna", 7).unwrap();
        assert_eq!((m.kind.as_str(), m.text.as_str(), m.ts), ("text", "ciao", 7));
        let reply = wa::Message {
            extended_text_message: MessageField::some(wa::message::ExtendedTextMessage {
                text: Some("sì".into()),
                context_info: MessageField::some(wa::ContextInfo {
                    stanza_id: Some("ID1".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let r = stored_from(&reply, "ID2", true, "", "", 8).unwrap();
        assert_eq!(r.text, "sì");
        assert_eq!(r.reply.as_deref(), Some("ID1"));
    }

    #[test]
    fn la_foto_porta_il_media_e_torna_indietro() {
        let m = wa::Message {
            image_message: MessageField::some(wa::message::ImageMessage {
                mimetype: Some("image/jpeg".into()),
                caption: Some("al mare".into()),
                direct_path: Some("/v/t62/x".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let s = stored_from(&m, "ID3", false, "", "", 9).unwrap();
        assert_eq!((s.kind.as_str(), s.text.as_str()), ("image", "al mare"));
        let media = media_ref("1@s.whatsapp.net", &s).unwrap();
        assert_eq!(media.ext(), "jpg");
        let back = &media.get::<WaMedia>().unwrap().msg;
        assert_eq!(back.image_message.as_option().unwrap().direct_path.as_deref(), Some("/v/t62/x"));
    }

    /// Lo storage di whatsapp-rust gira sulla SQLite già nel binario
    /// (quella di libsql, con Telegram): apre, migra e rilegge il db.
    #[tokio::test]
    async fn lo_storage_sqlite_si_apre_accanto_a_libsql() {
        let dir = std::env::temp_dir().join(format!("rondine-wa-db-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("whatsapp.db");
        let store = whatsapp_rust_sqlite_storage::SqliteStore::new(db.to_string_lossy().as_ref()).await;
        assert!(store.is_ok(), "{:?}", store.err());
        drop(store);
        assert!(whatsapp_rust_sqlite_storage::SqliteStore::new(db.to_string_lossy().as_ref()).await.is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reazioni_e_protocollo_non_sono_righe() {
        let m = wa::Message {
            reaction_message: MessageField::some(wa::message::ReactionMessage {
                text: Some("👍".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(stored_from(&m, "ID4", false, "", "", 1).is_none());
    }
}
