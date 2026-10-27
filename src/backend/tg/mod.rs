//! Telegram backend: grammers 0.11 (MTProto client in pure Rust, no TDLib).
//!
//! Tutto quello che tocca grammers sta qui sotto: l'accesso, le chiamate di
//! rete (`worker`) e gli update live (`live`). Fuori escono solo i tipi
//! neutri di `crate::model`.

pub mod live;
pub mod worker;

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use grammers_client::media::Media;
use grammers_client::message::InputMessage;
use grammers_client::message::InputReactions;
use grammers_client::sender::SenderPool;
use grammers_client::session::storages::SqliteSession;
use grammers_client::session::types::{PeerAuth, PeerId, PeerRef};
use grammers_client::session::updates::UpdatesLike;
use grammers_client::tl;
use grammers_client::{Client, InvocationError, SignInError};

use crate::i18n::tr;
use crate::links::TgLink;
use crate::model::{
    caption, ChatAction, ChatId, ChatRef, DialogInfo, HistoryRow, LastMsg, LinkHit, MediaRef, MyProfile, PeerInfo,
    ReactionItem, RowKind, SearchHit, Seen, Session, SharedItem, SharedTab, StickerSetInfo, FAVED_SET,
};
use crate::paths::{avatar_path, config_dir, data_dir, ensure_private_dir, DB_FILE};

// ---------------- tipi neutri <-> grammers ------------------------------

/// `PeerId::self_user()`: un id senza numero, tenuto con lo stesso valore
/// sentinella di grammers.
const SELF_USER: i64 = 1 << 40;

pub fn to_chat_id(p: PeerId) -> ChatId {
    ChatId(p.bot_api_dialog_id().unwrap_or(SELF_USER))
}

pub fn to_peer_id(c: ChatId) -> PeerId {
    PeerId::from_bot_api_dialog_id(c.0).unwrap_or_else(PeerId::self_user)
}

pub fn to_chat_ref(r: PeerRef) -> ChatRef {
    ChatRef { id: to_chat_id(r.id), auth: r.auth.hash() }
}

pub fn to_peer_ref(r: ChatRef) -> PeerRef {
    PeerRef { id: to_peer_id(r.id), auth: PeerAuth::from_hash(r.auth) }
}

/// Il media grammers dentro un `MediaRef` (con chiave di cache, estensione
/// dell'originale ed emoji dello sticker).
pub fn to_media_ref(media: Media) -> MediaRef {
    let key = media_key(&media);
    let ext = raw_ext(&media);
    let emoji = sticker_emoji(&media);
    MediaRef::new(key, ext, emoji, media)
}

/// Il media grammers di un `MediaRef` creato da questo backend.
pub fn media_of(m: &MediaRef) -> Result<&Media, String> {
    m.get::<Media>().ok_or_else(|| "media non Telegram".to_string())
}

/// Estensione dell'originale in cache (foto, immagini, gif).
fn raw_ext(media: &Media) -> &'static str {
    match media {
        Media::Photo(_) => "jpg",
        Media::Document(doc) => match doc.mime_type().unwrap_or("") {
            "image/gif" => "gif",
            "image/png" | "image/apng" => "png",
            "image/webp" => "webp",
            m if m.starts_with("image/jpeg") => "jpg",
            _ => "bin",
        },
        _ => "bin",
    }
}

// ---------------- dalla rete ai tipi neutri ----------------------------

pub fn seen_from_raw(s: &tl::enums::UserStatus) -> Seen {
    match s {
        tl::enums::UserStatus::Online(o) => Seen::Online(o.expires as i64),
        tl::enums::UserStatus::Offline(o) => Seen::Offline(o.was_online as i64),
        tl::enums::UserStatus::Recently(_) => Seen::Recently,
        tl::enums::UserStatus::LastWeek(_) => Seen::LastWeek,
        tl::enums::UserStatus::LastMonth(_) => Seen::LastMonth,
        tl::enums::UserStatus::Empty => Seen::Unknown,
    }
}

/// Foto e stato di un peer, per la lista chat.
pub fn peer_extras(peer: &grammers_client::peer::Peer) -> (Option<i64>, Seen) {
    use grammers_client::peer::Peer;
    match peer {
        Peer::User(u) => (u.photo().map(|p| p.photo_id), seen_from_raw(u.status())),
        Peer::Group(g) => (g.photo().map(|p| p.photo_id), Seen::Unknown),
        Peer::Channel(c) => (c.photo().map(|p| p.photo_id), Seen::Unknown),
        Peer::Community(c) => (c.photo().map(|p| p.photo_id), Seen::Unknown),
    }
}

pub fn is_broadcast(peer: &grammers_client::peer::Peer) -> bool {
    matches!(peer, grammers_client::peer::Peer::Channel(c) if c.raw.broadcast)
}

/// Scarica la foto profilo piccola (160px) se non è già in cache.
pub async fn download_avatar(client: &Client, peer: ChatRef, photo_id: i64) -> Result<PathBuf, String> {
    let dest = avatar_path(photo_id);
    if dest.exists() {
        return Ok(dest);
    }
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let photo = grammers_client::media::ChatPhoto {
        raw: tl::enums::InputFileLocation::InputPeerPhotoFileLocation(
            tl::types::InputPeerPhotoFileLocation {
                big: false,
                peer: to_peer_ref(peer).into(),
                photo_id,
            },
        ),
    };
    // scarica in un temporaneo: un file a metà non deve sembrare in cache
    let part = dest.with_extension("part");
    client
        .download_media(&photo, &part)
        .await
        .map_err(|e| e.to_string())?;
    std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Cosa sta facendo l'altro («sta scrivendo…»); `None` = ha smesso.
pub fn typing_label(a: &tl::enums::SendMessageAction) -> Option<&'static str> {
    use tl::enums::SendMessageAction as A;
    match a {
        A::SendMessageCancelAction => None,
        A::SendMessageRecordAudioAction | A::SendMessageUploadAudioAction(_) => {
            Some(tr("registra un vocale"))
        }
        A::SendMessageRecordVideoAction
        | A::SendMessageUploadVideoAction(_)
        | A::SendMessageRecordRoundAction
        | A::SendMessageUploadRoundAction(_) => Some(tr("invia un video")),
        A::SendMessageUploadPhotoAction(_) => Some(tr("invia una foto")),
        A::SendMessageUploadDocumentAction(_) => Some(tr("invia un file")),
        A::SendMessageChooseStickerAction => Some(tr("sceglie uno sticker")),
        _ => Some(tr("sta scrivendo")),
    }
}

pub fn last_from_message(m: &grammers_client::message::Message) -> LastMsg {
    LastMsg {
        id: m.id(),
        text: row_kind(m.media(), m.text().to_string()).summary(),
        outgoing: m.outgoing(),
        date: m.date().as_second(),
    }
}

/// Riga da un messaggio grammers; `from` è già risolto dal chiamante
/// (l'update live preferisce il nome nella lista dialoghi).
pub fn row_from_message(m: &grammers_client::message::Message, from: String) -> HistoryRow {
    HistoryRow {
        id: m.id(),
        outgoing: m.outgoing(),
        from,
        kind: row_kind(m.media(), m.text().to_string()),
        reactions: collect_reactions(&m.raw),
        date: m.date().as_second(),
        reply_to: m.reply_to_message_id(),
        reply_quote: match &m.raw {
            tl::enums::Message::Message(tl::types::Message {
                reply_to: Some(tl::enums::MessageReplyHeader::Header(h)),
                ..
            }) => h.quote_text.clone().filter(|q| !q.trim().is_empty()),
            _ => None,
        },
        fwd: m.forward_header().map(|h| {
            let tl::enums::MessageFwdHeader::Header(h) = h;
            h.from_name.unwrap_or_default()
        }),
        edited: m.edit_date().is_some() && !m.edit_hide(),
    }
}

///(conteggi) da una pagina delle reazioni (update server o raw del messaggio).
pub fn reactions_from(container: &tl::enums::MessageReactions) -> Vec<ReactionItem> {
    match container {
        tl::enums::MessageReactions::Reactions(r) => r
            .results
            .iter()
            .filter_map(|rc| {
                let tl::enums::ReactionCount::Count(rc) = rc;
                let tl::enums::Reaction::Emoji(e) = &rc.reaction else {
                    return None; // reazioni custom senza entità: oggi niente
                };
                Some(ReactionItem {
                    emoticon: e.emoticon.clone(),
                    count: rc.count,
                    mine: rc.chosen_order.is_some(),
                })
            })
            .collect(),
    }
}

/// le reazioni di un messaggio grezzo (storia: raw durante l'iter).
pub fn collect_reactions(raw: &tl::enums::Message) -> Vec<ReactionItem> {
    let tl::enums::Message::Message(inner) = raw else {
        return Vec::new();
    };
    match inner.reactions.as_ref() {
        Some(r) => reactions_from(r),
        None => Vec::new(),
    }
}

/// Etichetta breve per i media che non renderizziamo (video, vocale…).
fn media_note(doc: &grammers_client::media::Document, text: &str) -> String {
    let mime = doc.mime_type().unwrap_or("");
    let secs = doc.duration().map(|d| d as i64).unwrap_or(0);
    let caption = caption(text);
    let base = if mime.starts_with("video/") {
        "video"
    } else if secs > 0 && doc.name().is_none() && mime.contains("ogg") {
        tr("messaggio vocale")
    } else if mime.starts_with("audio/") {
        "audio"
    } else {
        tr("media non supportato")
    };
    let time = if secs > 0 {
        format!(" {secs}s")
    } else {
        String::new()
    };
    let extra = if caption.is_empty() {
        String::new()
    } else {
        format!(" — “{caption}”")
    };
    format!("{base}{time}{extra}")
}

// ---------------- accesso ----------------------------------------------

/// Connection credentials for my.telegram.org apps.
pub struct Credentials {
    pub api_id: i32,
    pub api_hash: String,
}

const CONFIG_FILE: &str = "credentials";

/// Read credentials from disk; `None` the first time around.
pub fn load_credentials() -> Option<Credentials> {
    let raw = std::fs::read_to_string(config_dir().join(CONFIG_FILE)).ok()?;
    parse_creds(&raw)
}

fn parse_creds(raw: &str) -> Option<Credentials> {
    let mut it = raw.split_whitespace();
    let api_id = it.next()?.parse().ok()?;
    let api_hash = it.next()?.to_string();
    Some(Credentials { api_id, api_hash })
}

/// Interactive first-run prompt; stores the answer in the credentials file.
pub async fn prompt_credentials() -> Result<Credentials, Box<dyn std::error::Error>> {
    println!("{}", tr("Prima configurazione di Rondine."));
    let _ = write!(std::io::stdout(), "API ID: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let api_id: i32 = line.trim().parse()?;
    let _ = write!(std::io::stdout(), "API hash: ");
    let _ = std::io::stdout().flush();
    let mut hash = String::new();
    std::io::stdin().read_line(&mut hash)?;
    let creds = Credentials {
        api_id,
        api_hash: hash.trim().to_string(),
    };
    save_credentials(&creds)?;
    Ok(creds)
}

/// Salva le credenziali dell'app (my.telegram.org) in un file 0600.
pub fn save_credentials(creds: &Credentials) -> std::io::Result<()> {
    ensure_private_dir(&config_dir())?;
    let cred_path = config_dir().join(CONFIG_FILE);
    // il file contiene l'api_hash: nasce 0600 dall'open, senza la finestra
    // "scritto con la umask, poi chmod" in cui era leggibile da altri
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&cred_path)?;
    file.write_all(format!("{} {}\n", creds.api_id, creds.api_hash).as_bytes())
}

/// Cosa chiedere a chi accede da una finestra (GUI), con l'errore del
/// tentativo prima se c'è.
#[derive(Clone, Debug, PartialEq)]
pub enum LoginAsk {
    Phone(Option<String>),
    Code(Option<String>),
    Password { hint: String, error: Option<String> },
}

/// Accesso passo passo senza terminale: le domande escono su `ask`, le
/// risposte arrivano da `answers` (numero, codice, password 2FA).
pub async fn sign_in_steps(
    client: &Client,
    api_hash: &str,
    ask: &tokio::sync::mpsc::UnboundedSender<LoginAsk>,
    answers: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
) -> Result<(), String> {
    let closed = || tr("accesso interrotto").to_string();
    let mut error = None;
    let token = loop {
        let _ = ask.send(LoginAsk::Phone(error.take()));
        let phone = normalize_phone(&answers.recv().await.ok_or_else(closed)?);
        if phone.len() < 7 {
            error = Some(tr("numero troppo corto: scrivilo col prefisso, es. +39 333 1234567").into());
            continue;
        }
        match client.request_login_code(&phone, api_hash).await {
            Ok(t) => break t,
            Err(e) if e.is("PHONE_NUMBER_INVALID") => error = Some(tr("numero non valido").into()),
            Err(e) => error = Some(e.to_string()),
        }
    };
    let mut error = None;
    loop {
        let _ = ask.send(LoginAsk::Code(error.take()));
        let code = answers.recv().await.ok_or_else(closed)?;
        match client.sign_in(&token, code.trim()).await {
            Ok(_) => return Ok(()),
            Err(SignInError::InvalidCode) => error = Some(tr("codice errato, riprova").into()),
            Err(SignInError::SignUpRequired) => {
                return Err(tr("Account senza Telegram: crea prima un account con l'app ufficiale.").into())
            }
            Err(SignInError::PasswordRequired(mut pt)) => {
                let mut error = None;
                loop {
                    let hint = pt.hint().unwrap_or_default().to_string();
                    let _ = ask.send(LoginAsk::Password { hint, error: error.take() });
                    let pass = answers.recv().await.ok_or_else(closed)?;
                    match client.check_password(pt, pass.trim()).await {
                        Ok(_) => return Ok(()),
                        Err(SignInError::InvalidPassword(next)) => {
                            pt = next;
                            error = Some(tr("password errata, riprova").into());
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                }
            }
            Err(e) => error = Some(e.to_string()),
        }
    }
}

/// Connect the backend: sqlite session + runner task + client handle.
/// Returns the updates receiver (to give to `client.stream_updates`).
pub async fn connect(
    creds: &Credentials,
) -> Result<
    (
        Arc<SqliteSession>,
        Client,
        tokio::sync::mpsc::Receiver<UpdatesLike>,
    ),
    Box<dyn std::error::Error>,
> {
    ensure_private_dir(&data_dir())?;
    // La sessione contiene le chiavi di autorizzazione: solo l'utente.
    // Il file nasce 0600 (pre-creato: sqlite apre quello esistente e non lo
    // ricrea con la umask); i -wal/-journal che sqlite crea coi permessi
    // standard restano protetti perché la cartella è 0700.
    let db_path = data_dir().join(DB_FILE);
    if !db_path.exists() {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&db_path)?;
    }
    let session = Arc::new(SqliteSession::open(&db_path).await?);
    // se esisteva già con permessi larghi (install vecchie): ripara
    if let Ok(mut perms) = std::fs::metadata(&db_path).map(|m| m.permissions()) {
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(&db_path, perms);
    }
    let pool = SenderPool::new(Arc::clone(&session), creds.api_id);
    let SenderPool {
        runner,
        handle,
        updates,
    } = pool;
    tokio::spawn(runner.run());
    let client = Client::new(handle);
    Ok((session, client, updates))
}

/// La rete non è sempre pronta al primo colpo (Wi-Fi che rientra, laptop
/// che riprende dal sospendo): chi lanciava rondine vedeva solo un errore
/// e una chiusura. Qui si ritenta pochi colpi con crescente attesa, e
/// solo per errori di cavallo di rete: parola? numero sbagliato?
pub async fn connect_and_sign(
    creds: &Credentials,
) -> Result<
    (
        Arc<SqliteSession>,
        Client,
        tokio::sync::mpsc::Receiver<UpdatesLike>,
    ),
    Box<dyn std::error::Error>,
> {
    const ATTEMPTS: u32 = 5;
    let mut wait = 2u64;
    println!("{}", tr("Verifico l'accesso al tuo account…"));
    let mut wobble: Option<Box<dyn std::error::Error>> = None;
    for attempt in 1..=ATTEMPTS {
        // c'era un calo di rete al colpo prima: aspetta e dicilo
        if let Some(e) = wobble.take() {
            eprintln!(
                "{}",
                crate::trf!("Rete non pronta ({}): ritento tra {}s ({}/{})…", e, wait, attempt, ATTEMPTS - 1)
            );
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            wait = (wait * 2).min(16);
        }
        let (session, client, updates) = match connect(creds).await {
            Ok(x) => x,
            Err(e) if attempt < ATTEMPTS && net_wobble(&e.to_string()) => {
                wobble = Some(e);
                continue;
            }
            Err(e) => return Err(e),
        };
        // sign_in tocca la rete (is_authorized): un calo qui è lo stesso
        // tipo di problema, quindi rientra negli stessi tentativi
        match sign_in(&client, creds).await {
            Ok(()) => return Ok((session, client, updates)),
            Err(e) if attempt < ATTEMPTS && net_wobble(&e.to_string()) => {
                wobble = Some(e);
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("dell'ultimo tentativo un errore non-netto era già uscito")
}

/// Errori per cui ha senso il ritento: solo la rete da prepararsi, non i
/// valve/numeri/codici sbagliati (quelli vanno corretti da persona).
fn net_wobble(e: &str) -> bool {
    [
        "NetworkUnreachable",
        "NetworkDown",
        "NetworkChanged",
        "TimedOut",
        "ConnectionRefused",
        "ConnectionReset",
        "AddrNotAvailable",
        "HostUnreachable",
        "UnexpectedEof",
    ]
    .iter()
    .any(|s| e.contains(s))
}

/// Normalizza un numero di telefono all'unico formato che Telegram accetta:
/// solo cifre con `+` davanti. Accetta spazi/trattini/parentesi e `00…` al
/// posto di `+`. Il numero nudo si legge come mobile italiano (3XXXXXXXXX)
/// solo se ha quella forma: prima QUALSIASI numero nudo diventava +39… e
/// un numero di un altro paese (es. "491512345678" tedesco) finiva
/// sbagliato. Per gli altri paesi serve il prefisso internazionale.
fn normalize_phone(raw: &str) -> String {
    let t = raw.trim();
    let has_plus = t.starts_with('+');
    let mut digits: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    if !has_plus && digits.starts_with("00") {
        digits = digits[2..].to_string();
    }
    if has_plus {
        format!("+{digits}")
    } else if digits.starts_with('3') && digits.len() == 10 {
        format!("+39{digits}")
    } else {
        format!("+{digits}")
    }
}

/// Interactive sign-in; runs before the TUI starts.
pub async fn sign_in(
    client: &Client,
    creds: &Credentials,
) -> Result<(), Box<dyn std::error::Error>> {
    if client.is_authorized().await? {
        return Ok(());
    }
    println!();
    println!("{}", tr("⚠️  Digita ogni dato SOLO quando compare la domanda, poi conferma con Invio."));
    println!("{}", tr("    (Se scrivi prima che la richiesta appaia, l'input va perso.)"));
    let phone = loop {
        let _ = write!(std::io::stdout(), "{}", tr("Numero di telefono (+39…): "));
        let _ = std::io::stdout().flush();
        let mut phone = String::new();
        if std::io::stdin().read_line(&mut phone)? == 0 {
            return Err("input chiuso".into());
        }
        let phone = normalize_phone(&phone);
        // log mascherato per diagnosi: niente numero completo su disco
        let shown = &phone[..phone.len().min(4)];
        eprintln!(
            "[rondine] tel normalizzato: {shown}… ({} cifre)",
            phone.len() - 1
        );
        // "+39" da solo o righe spurie: richiedi invece di chiamare Telegram
        if phone.len() >= 7 {
            break phone;
        }
        println!(
            "{}",
            tr("Numero troppo corto. Usa il solo mobile (es. 3391234567) o il formato +39 completo.")
        );
    };
    let token = client.request_login_code(&phone, &creds.api_hash).await?;
    let _ = write!(std::io::stdout(), "{}", tr("Codice ricevuto: "));
    let _ = std::io::stdout().flush();
    let mut code = String::new();
    std::io::stdin().read_line(&mut code)?;
    let user = match client.sign_in(&token, code.trim()).await {
        Ok(user) => user,
        Err(SignInError::PasswordRequired(token)) => {
            let hint = token.hint().map(str::to_string).unwrap_or_default();
            // rpassword legge senza eco: la password non resta visibile
            // nella cronologia del terminale come con read_line
            let prompt = if hint.is_empty() {
                tr("Password 2FA: ").to_string()
            } else {
                crate::trf!("Password 2FA ({}): ", hint)
            };
            let pass = rpassword::prompt_password(prompt)?;
            client.check_password(token, pass.trim()).await?
        }
        Err(SignInError::SignUpRequired) => {
            return Err(tr("Account senza Telegram: crea prima un account con l'app ufficiale.").into())
        }
        Err(SignInError::InvalidCode) => return Err(tr("Codice errato, riavvia e riprova.").into()),
        Err(SignInError::InvalidPassword(_)) => return Err(tr("2FA errata, riavvia e riprova.").into()),
        Err(e) => return Err(e.into()),
    };
    println!("{}", crate::trf!("Connesso come {}", user.full_name()));
    Ok(())
}

// ---------------- chiamate di rete -------------------------------------

/// List the dialogs (chats) of the account.
pub async fn load_dialogs(client: &Client) -> Result<Vec<DialogInfo>, InvocationError> {
    let mut out = Vec::new();
    let mut iter = client.iter_dialogs();
    while let Some(dialog) = iter.next().await? {
        let peer = dialog.peer().clone();
        let name = peer.name().unwrap_or(tr("senza nome")).to_string();
        let is_group = !matches!(peer, grammers_client::peer::Peer::User(_));
        let (photo_id, seen) = peer_extras(&peer);
        let read_unread = match &dialog.raw {
            tl::enums::Dialog::Dialog(raw) => raw.unread_count.max(0) as usize,
            _ => 0,
        };
        let (pinned, read_out, archived, unread_mark) = match &dialog.raw {
            tl::enums::Dialog::Dialog(raw) => {
                (raw.pinned, raw.read_outbox_max_id, raw.folder_id == Some(1), raw.unread_mark)
            }
            _ => (false, 0, false, false),
        };
        let muted = match &dialog.raw {
            tl::enums::Dialog::Dialog(raw) => {
                let tl::enums::PeerNotifySettings::Settings(n) = &raw.notify_settings;
                n.mute_until.is_some_and(|t| t as i64 > jiff::Timestamp::now().as_second())
            }
            _ => false,
        };
        out.push(DialogInfo {
            name,
            peer_ref: to_chat_ref(dialog.peer_ref()),
            peer_id: to_chat_id(dialog.peer_id()),
            is_group,
            unread: read_unread,
            pinned,
            last: dialog.last_message.as_ref().map(last_from_message),
            read_out,
            photo_id,
            seen,
            broadcast: is_broadcast(&peer),
            muted,
            archived,
            unread_mark,
        });
    }
    Ok(out)
}

/// Voce di lista per una chat che non è ancora tra i dialoghi (primo
/// messaggio da uno sconosciuto, gruppo appena entrato): il peer arriva
/// dalla cache dell'update o, in mancanza, da resolve_peer.
pub async fn dialog_from_message(
    client: &Client,
    m: &grammers_client::message::Message,
) -> Option<DialogInfo> {
    let peer_ref = m.peer_ref().await.ok().flatten()?;
    let peer = match m.peer() {
        Some(p) => p.clone(),
        None => client.resolve_peer(peer_ref).await.ok()?,
    };
    let (photo_id, seen) = peer_extras(&peer);
    Some(DialogInfo {
        muted: false,
        archived: false,
        unread_mark: false,
        broadcast: is_broadcast(&peer),
        photo_id,
        seen,
        name: peer.name().unwrap_or(tr("senza nome")).to_string(),
        is_group: !matches!(peer, grammers_client::peer::Peer::User(_)),
        peer_id: to_chat_id(m.peer_id()),
        peer_ref: to_chat_ref(peer_ref),
        unread: 0,
        pinned: false,
        last: Some(last_from_message(m)),
        read_out: 0,
    })
}

/// Parse into a projected row: sticker / foto / gif / file / nota / testo.
pub fn row_kind(media: Option<Media>, text: String) -> RowKind {
    let cap = caption(&text);
    match media.clone() {
        Some(Media::Sticker(sticker)) => {
            let emoji = sticker.emoji().to_string();
            let animated = sticker.is_animated();
            RowKind::Sticker {
                media: to_media_ref(media.unwrap()),
                emoji,
                animated,
            }
        }
        Some(Media::Photo(_)) => RowKind::Photo {
            media: to_media_ref(media.unwrap()),
            caption: cap,
            gif: false,
        },
        Some(Media::Document(doc)) => {
            let mime = doc.mime_type().unwrap_or("").to_string();
            let name = doc
                .name()
                .map(str::to_string)
                .unwrap_or_else(|| "file".into());
            let size = doc.size();
            if mime == "image/gif" {
                return RowKind::Photo {
                    media: to_media_ref(media.unwrap()),
                    caption: cap,
                    gif: true,
                };
            }
            if matches!(mime.as_str(), "image/webp" | "image/png" | "image/apng")
                || mime.starts_with("image/jpeg")
            {
                return RowKind::Photo {
                    media: to_media_ref(media.unwrap()),
                    caption: cap,
                    gif: false,
                };
            }
            if mime.starts_with("video/") {
                let round = matches!(&doc.raw, tl::types::MessageMediaDocument { round: true, .. });
                return RowKind::Video {
                    seconds: doc.duration().unwrap_or(0.0) as u32,
                    caption: cap,
                    round,
                    note: media_note(&doc, &text),
                    media: to_media_ref(media.unwrap()),
                };
            }
            if mime.starts_with("audio/") {
                return RowKind::Audio {
                    media: to_media_ref(media.unwrap()),
                    name,
                    seconds: doc.duration().unwrap_or(0.0) as u32,
                    caption: cap,
                    voice: doc.name().is_none(),
                };
            }
            RowKind::Doc {
                media: to_media_ref(media.unwrap()),
                name,
                size,
                caption: cap,
            }
        }
        // anteprime dei link: conta solo il testo del messaggio
        Some(Media::WebPage(_)) => RowKind::Text(text),
        Some(other) => RowKind::Note(match other {
            Media::Contact(c) => crate::trf!("📇 contatto: {} {}", c.first_name(), c.last_name()),
            Media::Poll(p) => match p.question() {
                tl::enums::TextWithEntities::Entities(t) => {
                    crate::trf!("📊 sondaggio: {}", t.text)
                }
            },
            Media::Geo(_) | Media::GeoLive(_) => tr("📍 posizione").to_string(),
            Media::Venue(_) => tr("🗺 luogo").to_string(),
            Media::Dice(d) => format!("🎲 {} ({})", d.emoji(), d.value()),
            _ => format!("{other:?}"),
        }),
        None if text.is_empty() => RowKind::Service(String::new()),
        None => RowKind::Text(text),
    }
}

/// Nome del mittente se risolvibile dalla cache della sessione;
/// vuoto quando Telegram non lo fornisce nel pacchetto (fallback in vista).
pub fn sender_name(m: &grammers_client::message::Message) -> String {
    m.sender()
        .and_then(|p| p.name())
        .unwrap_or_default()
        .to_string()
}

/// Il nome completo dell'utente connesso (per le etichette "nome │ testo").
pub async fn own_name(client: &Client) -> String {
    client
        .get_me()
        .await
        .map(|u| u.full_name())
        .unwrap_or_default()
}

/// Fetch the last `limit` messages of a dialog, oldest first.
pub async fn load_history(
    client: &Client,
    peer_ref: ChatRef,
    limit: usize,
) -> Result<Vec<HistoryRow>, InvocationError> {
    let mut rows = Vec::new();
    let mut iter = client.iter_messages(to_peer_ref(peer_ref)).limit(limit);
    while let Some(m) = iter.next().await? {
        let from = sender_name(&m);
        rows.push(row_from_message(&m, from));
    }
    rows.reverse();
    Ok(rows)
}

/// `limit` messaggi precedenti a `before` (id escluso), dal più vecchio.
pub async fn load_history_before(
    client: &Client,
    peer_ref: ChatRef,
    before: i32,
    limit: usize,
) -> Result<Vec<HistoryRow>, InvocationError> {
    let mut rows = Vec::new();
    let mut iter = client
        .iter_messages(to_peer_ref(peer_ref))
        .offset_id(before)
        .limit(limit);
    while let Some(m) = iter.next().await? {
        let from = sender_name(&m);
        rows.push(row_from_message(&m, from));
    }
    rows.reverse();
    Ok(rows)
}

/// Messaggi per id, nell'ordine di `ids`; `None` per quelli eliminati.
pub async fn load_by_id(
    client: &Client,
    peer_ref: ChatRef,
    ids: &[i32],
) -> Result<Vec<Option<HistoryRow>>, InvocationError> {
    let found = client.get_messages_by_id(to_peer_ref(peer_ref), ids).await?;
    Ok(found
        .into_iter()
        .map(|m| m.map(|m| row_from_message(&m, sender_name(&m))))
        .collect())
}

/// All the sticker sets installed on the account.
pub async fn fetch_installed_sets(client: &Client) -> Result<Vec<StickerSetInfo>, InvocationError> {
    let all = client
        .invoke(&tl::functions::messages::GetAllStickers { hash: 0 })
        .await?;
    let sets = match all {
        tl::enums::messages::AllStickers::Stickers(all) => all.sets,
        tl::enums::messages::AllStickers::NotModified => Vec::new(),
    };
    Ok(sets
        .into_iter()
        .map(|st| {
            let tl::enums::StickerSet::Set(s) = &st;
            StickerSetInfo {
                set_id: s.id,
                title: s.title.clone(),
                short_name: s.short_name.clone(),
            }
        })
        .collect())
}

/// Turn a raw TL document into the client `Media` we can send/copy.
pub fn media_from_doc(doc: tl::enums::Document) -> Option<Media> {
    let inner = tl::types::MessageMediaDocument {
        nopremium: false,
        spoiler: false,
        video: false,
        round: false,
        voice: false,
        document: Some(doc),
        alt_documents: None,
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
    };
    Media::from_raw(tl::enums::MessageMedia::Document(inner))
}

/// Gli sticker preferiti (FAVED_SET) o recenti (RECENT_SET) dell'account.
pub async fn fetch_special_docs(
    client: &Client,
    set_id: i64,
) -> Result<Vec<tl::enums::Document>, InvocationError> {
    if set_id == FAVED_SET {
        match client.invoke(&tl::functions::messages::GetFavedStickers { hash: 0 }).await? {
            tl::enums::messages::FavedStickers::Stickers(s) => Ok(s.stickers),
            tl::enums::messages::FavedStickers::NotModified => Ok(Vec::new()),
        }
    } else {
        match client
            .invoke(&tl::functions::messages::GetRecentStickers { attached: false, hash: 0 })
            .await?
        {
            tl::enums::messages::RecentStickers::Stickers(s) => Ok(s.stickers),
            tl::enums::messages::RecentStickers::NotModified => Ok(Vec::new()),
        }
    }
}

/// Documenti di un set richiesti per short_name (on-demand nel pannello).
pub async fn fetch_set_docs_by_name(
    client: &Client,
    short_name: &str,
) -> Result<Vec<tl::enums::Document>, InvocationError> {
    let resp = client
        .invoke(&tl::functions::messages::GetStickerSet {
            stickerset: tl::enums::InputStickerSet::ShortName(
                tl::types::InputStickerSetShortName {
                    short_name: short_name.to_string(),
                },
            ),
            hash: 0,
        })
        .await?;
    match resp {
        tl::enums::messages::StickerSet::Set(full) => Ok(full.documents),
        tl::enums::messages::StickerSet::NotModified => Ok(Vec::new()),
    }
}

/// Send plain text, returning the sent message.
pub async fn send_text(
    client: &Client,
    peer_ref: ChatRef,
    text: &str,
    reply_to: Option<i32>,
) -> Result<grammers_client::message::Message, InvocationError> {
    client
        .send_message(to_peer_ref(peer_ref), InputMessage::new().text(text).reply_to(reply_to))
        .await
}

/// Send a sticker reusing its server-side file reference (`copy_media`
/// passa lo stesso documento — niente ri-upload).
pub async fn send_sticker(
    client: &Client,
    peer_ref: ChatRef,
    media: &MediaRef,
    reply_to: Option<i32>,
) -> Result<grammers_client::message::Message, String> {
    let media = media_of(media)?;
    client
        .send_message(
            to_peer_ref(peer_ref),
            InputMessage::new().copy_media(media).reply_to(reply_to),
        )
        .await
        .map_err(|e| e.to_string())
}

/// aggiunge o toglie la reazione standard a un messaggio (None = le mie via).
pub async fn send_reaction(
    client: &Client,
    peer_ref: ChatRef,
    msg_id: i32,
    emoticon: Option<&str>,
) -> Result<(), InvocationError> {
    let peer = to_peer_ref(peer_ref);
    match emoticon {
        Some(e) => client
            .send_reactions(peer, msg_id, InputReactions::emoticon(e))
            .await,
        None => client
            .send_reactions(peer, msg_id, InputReactions::remove())
            .await,
    }
}

/// Invia un file locale (PNG/JPEG) come foto del messaggio: upload piano,
/// poi l'invio vero e proprio; il testo è la didascalia.
pub async fn send_photo(
    client: &Client,
    peer_ref: ChatRef,
    path: &std::path::Path,
    caption: &str,
    reply_to: Option<i32>,
) -> Result<grammers_client::message::Message, String> {
    let uploaded = client
        .upload_file(path)
        .await
        .map_err(|e| format!("upload: {e}"))?;
    let mut msg = InputMessage::new().photo(uploaded).reply_to(reply_to);
    if !caption.trim().is_empty() {
        msg = msg.text(caption);
    }
    client
        .send_message(to_peer_ref(peer_ref), msg)
        .await
        .map_err(|e| e.to_string())
}

/// Invia un file locale come documento (nome e mime dal file), col testo
/// come didascalia.
pub async fn send_document(
    client: &Client,
    peer_ref: ChatRef,
    path: &std::path::Path,
    caption: &str,
    reply_to: Option<i32>,
) -> Result<grammers_client::message::Message, String> {
    let uploaded = client
        .upload_file(path)
        .await
        .map_err(|e| format!("upload: {e}"))?;
    let mut msg = InputMessage::new().document(uploaded).reply_to(reply_to);
    if !caption.trim().is_empty() {
        msg = msg.text(caption);
    }
    client
        .send_message(to_peer_ref(peer_ref), msg)
        .await
        .map_err(|e| e.to_string())
}

/// Modifica in place un testo già inviato (restituito id invariato).
pub async fn edit_text(
    client: &Client,
    peer_ref: ChatRef,
    message_id: i32,
    new_text: &str,
) -> Result<(), InvocationError> {
    client
        .edit_message(to_peer_ref(peer_ref), message_id, InputMessage::new().text(new_text))
        .await
}

/// Elimina definitivamente (revoke) gli id passati della chat.
pub async fn delete_messages(
    client: &Client,
    peer_ref: ChatRef,
    ids: &[i32],
) -> Result<(), InvocationError> {
    client.delete_messages(to_peer_ref(peer_ref), ids).await?;
    Ok(())
}

/// Legge la scheda completa: GetFullUser / GetFullChat / GetFullChannel.
pub async fn peer_info(client: &Client, peer_ref: ChatRef) -> Result<PeerInfo, String> {
    use grammers_client::peer::Peer;
    use grammers_client::session::types::PeerKind;
    let peer_ref = to_peer_ref(peer_ref);
    let err = |e: InvocationError| e.to_string();
    let mut info = PeerInfo::default();
    match client.resolve_peer(peer_ref).await.map_err(err)? {
        Peer::User(u) => {
            info.username = u.username().map(str::to_string);
            info.phone = u.phone().filter(|p| !p.is_empty()).map(|p| format!("+{p}"));
            info.bot = u.is_bot();
            info.me = u.is_self();
        }
        Peer::Channel(c) => info.username = c.raw.username.clone(),
        _ => {}
    }
    match peer_ref.id.kind() {
        PeerKind::User => {
            let tl::enums::users::UserFull::Full(full) = client
                .invoke(&tl::functions::users::GetFullUser { id: peer_ref.into() })
                .await
                .map_err(err)?;
            let tl::enums::UserFull::Full(u) = full.full_user;
            info.about = u.about.unwrap_or_default();
            info.blocked = u.blocked;
            info.ttl = u.ttl_period.unwrap_or(0);
        }
        PeerKind::Chat => {
            let tl::enums::messages::ChatFull::Full(full) = client
                .invoke(&tl::functions::messages::GetFullChat { chat_id: peer_ref.into() })
                .await
                .map_err(err)?;
            fill_chat_full(&mut info, full.full_chat);
        }
        PeerKind::Channel => {
            let tl::enums::messages::ChatFull::Full(full) = client
                .invoke(&tl::functions::channels::GetFullChannel { channel: peer_ref.into() })
                .await
                .map_err(err)?;
            fill_chat_full(&mut info, full.full_chat);
        }
    }
    Ok(info)
}

fn fill_chat_full(info: &mut PeerInfo, full: tl::enums::ChatFull) {
    match full {
        tl::enums::ChatFull::Full(c) => {
            info.about = c.about;
            info.ttl = c.ttl_period.unwrap_or(0);
            info.members = match c.participants {
                tl::enums::ChatParticipants::Participants(p) => Some(p.participants.len() as i32),
                tl::enums::ChatParticipants::Forbidden(_) => None,
            };
        }
        tl::enums::ChatFull::ChannelFull(c) => {
            info.about = c.about;
            info.ttl = c.ttl_period.unwrap_or(0);
            info.members = c.participants_count;
        }
        tl::enums::ChatFull::CommunityFull(c) => info.about = c.about,
    }
}

/// Il filtro di ricerca di Telegram per una scheda dei contenuti condivisi.
fn shared_filter(tab: SharedTab) -> tl::enums::MessagesFilter {
    use tl::enums::MessagesFilter as F;
    match tab {
        SharedTab::Media => F::InputMessagesFilterPhotoVideo,
        SharedTab::Files => F::InputMessagesFilterDocument,
        SharedTab::Links => F::InputMessagesFilterUrl,
        SharedTab::Audio => F::InputMessagesFilterMusic,
        SharedTab::Voice => F::InputMessagesFilterRoundVoice,
    }
}

/// Gli URL di un messaggio: quelli scritti nel testo e quelli nascosti
/// dietro una parola (entità TextUrl).
fn message_links(m: &grammers_client::message::Message) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in m.text().split_whitespace() {
        let w = word.trim_matches(|c: char| matches!(c, '(' | ')' | '<' | '>' | '"' | '«' | '»' | ',' | ';'));
        let w = w.trim_end_matches(['.', ':', '!', '?']);
        let url = if w.starts_with("https://") || w.starts_with("http://") {
            w.to_string()
        } else if w.starts_with("www.") || w.starts_with("t.me/") {
            format!("https://{w}")
        } else {
            continue;
        };
        if !out.contains(&url) {
            out.push(url);
        }
    }
    if let tl::enums::Message::Message(raw) = &m.raw {
        // gli offset delle entità sono in unità UTF-16
        let utf16: Vec<u16> = raw.message.encode_utf16().collect();
        for e in raw.entities.iter().flatten() {
            let url = match e {
                tl::enums::MessageEntity::TextUrl(u) => u.url.clone(),
                tl::enums::MessageEntity::Url(u) => {
                    let (a, len) = (u.offset.max(0) as usize, u.length.max(0) as usize);
                    let Some(part) = utf16.get(a..a + len) else { continue };
                    let w = String::from_utf16_lossy(part);
                    if w.contains("://") {
                        w
                    } else {
                        format!("https://{w}")
                    }
                }
                _ => continue,
            };
            if !out.contains(&url) {
                out.push(url);
            }
        }
    }
    out
}

/// Gli ultimi `limit` contenuti di un tipo condivisi nella chat (dal più recente).
pub async fn shared_media(
    client: &Client,
    peer_ref: ChatRef,
    tab: SharedTab,
    limit: usize,
) -> Result<Vec<SharedItem>, InvocationError> {
    let mut out = Vec::new();
    let mut iter = client.search_messages(to_peer_ref(peer_ref)).filter(shared_filter(tab));
    while out.len() < limit {
        let Some(m) = iter.next().await? else { break };
        let links = if tab == SharedTab::Links { message_links(&m) } else { Vec::new() };
        let from = sender_name(&m);
        out.push(SharedItem { row: row_from_message(&m, from), links });
    }
    Ok(out)
}

/// Esegue un'azione rapida sulla chat.
pub async fn chat_action(client: &Client, peer_ref: ChatRef, action: ChatAction) -> Result<(), InvocationError> {
    let peer_ref = to_peer_ref(peer_ref);
    let dialog_peer = || tl::enums::InputDialogPeer::Peer(tl::types::InputDialogPeer { peer: peer_ref.into() });
    match action {
        ChatAction::Archive(on) => {
            client
                .invoke(&tl::functions::folders::EditPeerFolders {
                    folder_peers: vec![tl::enums::InputFolderPeer::Peer(tl::types::InputFolderPeer {
                        peer: peer_ref.into(),
                        folder_id: if on { 1 } else { 0 },
                    })],
                })
                .await?;
        }
        ChatAction::Pin(pinned) => {
            client
                .invoke(&tl::functions::messages::ToggleDialogPin { pinned, peer: dialog_peer() })
                .await?;
        }
        ChatAction::Mute(on) => {
            client
                .invoke(&tl::functions::account::UpdateNotifySettings {
                    peer: tl::enums::InputNotifyPeer::Peer(tl::types::InputNotifyPeer { peer: peer_ref.into() }),
                    settings: tl::enums::InputPeerNotifySettings::Settings(tl::types::InputPeerNotifySettings {
                        show_previews: None,
                        silent: None,
                        // «per sempre» per Telegram è il massimo di un i32
                        mute_until: Some(if on { i32::MAX } else { 0 }),
                        sound: None,
                        stories_muted: None,
                        stories_hide_sender: None,
                        stories_sound: None,
                    }),
                })
                .await?;
        }
        ChatAction::MarkUnread(unread) => {
            client
                .invoke(&tl::functions::messages::MarkDialogUnread {
                    unread,
                    parent_peer: None,
                    peer: dialog_peer(),
                })
                .await?;
        }
        ChatAction::Ttl(period) => {
            client
                .invoke(&tl::functions::messages::SetHistoryTtl { peer: peer_ref.into(), period })
                .await?;
        }
    }
    Ok(())
}

/// Blocca o sblocca un utente.
pub async fn set_blocked(client: &Client, peer_ref: ChatRef, blocked: bool) -> Result<(), InvocationError> {
    let peer = to_peer_ref(peer_ref);
    if blocked {
        client
            .invoke(&tl::functions::contacts::Block { my_stories_from: false, id: peer.into() })
            .await?;
    } else {
        client
            .invoke(&tl::functions::contacts::Unblock { my_stories_from: false, id: peer.into() })
            .await?;
    }
    Ok(())
}

/// Elimina la chat dalla lista: per le private e i gruppi piccoli cancella
/// la cronologia (`revoke`: anche per gli altri, solo private), dai gruppi
/// si esce, dai canali/supergruppi si esce e basta.
pub async fn delete_chat(client: &Client, peer_ref: ChatRef, revoke: bool) -> Result<(), InvocationError> {
    use grammers_client::session::types::PeerKind;
    let peer_ref = to_peer_ref(peer_ref);
    match peer_ref.id.kind() {
        PeerKind::Channel => {
            client
                .invoke(&tl::functions::channels::LeaveChannel { channel: peer_ref.into() })
                .await?;
            Ok(())
        }
        PeerKind::Chat => {
            client
                .invoke(&tl::functions::messages::DeleteChatUser {
                    revoke_history: false,
                    chat_id: peer_ref.into(),
                    user_id: tl::enums::InputUser::UserSelf,
                })
                .await?;
            // già fuori dal gruppo: se la pulizia non va, la chat sparisce
            // comunque dalla lista al prossimo avvio
            let _ = clear_history(client, peer_ref, false).await;
            Ok(())
        }
        PeerKind::User => clear_history(client, peer_ref, revoke).await,
    }
}

/// Cancella tutta la cronologia; Telegram lo fa a blocchi: `offset` > 0
/// vuol dire che ne resta.
async fn clear_history(client: &Client, peer_ref: PeerRef, revoke: bool) -> Result<(), InvocationError> {
    loop {
        let tl::enums::messages::AffectedHistory::History(done) = client
            .invoke(&tl::functions::messages::DeleteHistory {
                just_clear: false,
                revoke,
                peer: peer_ref.into(),
                max_id: 0,
                min_date: None,
                max_date: None,
            })
            .await?;
        if done.offset <= 0 {
            return Ok(());
        }
    }
}

/// Legge il mio profilo (nome, username, telefono, bio, foto).
pub async fn my_profile(client: &Client) -> Result<MyProfile, String> {
    let me = client.get_me().await.map_err(|e| e.to_string())?;
    let tl::enums::users::UserFull::Full(full) = client
        .invoke(&tl::functions::users::GetFullUser { id: tl::enums::InputUser::UserSelf })
        .await
        .map_err(|e| e.to_string())?;
    let tl::enums::UserFull::Full(f) = full.full_user;
    let photo = match (me.photo().map(|p| p.photo_id), me.to_ref().await.ok().flatten()) {
        (Some(id), Some(r)) => download_avatar(client, to_chat_ref(r), id).await.ok(),
        _ => None,
    };
    Ok(MyProfile {
        first: me.first_name().unwrap_or_default().to_string(),
        last: me.last_name().unwrap_or_default().to_string(),
        username: me.username().unwrap_or_default().to_string(),
        phone: me.phone().filter(|p| !p.is_empty()).map(|p| format!("+{p}")).unwrap_or_default(),
        about: f.about.unwrap_or_default(),
        photo,
    })
}

/// Salva nome, cognome e bio; lo username solo se è cambiato (`old_username`).
pub async fn save_profile(client: &Client, p: &MyProfile, old_username: &str) -> Result<(), String> {
    client
        .invoke(&tl::functions::account::UpdateProfile {
            first_name: Some(p.first.trim().to_string()),
            last_name: Some(p.last.trim().to_string()),
            about: Some(p.about.trim().to_string()),
        })
        .await
        .map_err(|e| e.to_string())?;
    let username = p.username.trim().trim_start_matches('@');
    if username != old_username {
        client
            .invoke(&tl::functions::account::UpdateUsername { username: username.to_string() })
            .await
            .map_err(|e| match e.to_string() {
                s if s.contains("USERNAME_OCCUPIED") => tr("username già in uso").to_string(),
                s if s.contains("USERNAME_INVALID") => {
                    tr("username non valido (5-32 caratteri: lettere, cifre, _)").to_string()
                }
                s => s,
            })?;
    }
    Ok(())
}

/// Carica un'immagine come nuova foto profilo.
pub async fn set_profile_photo(client: &Client, path: &Path) -> Result<(), String> {
    let uploaded = client.upload_file(path).await.map_err(|e| format!("upload: {e}"))?;
    client
        .invoke(&tl::functions::photos::UploadProfilePhoto {
            fallback: false,
            bot: None,
            file: Some(uploaded.raw),
            video: None,
            video_start_ts: None,
            video_emoji_markup: None,
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// I dispositivi collegati all'account: prima questo, poi i più recenti.
pub async fn sessions(client: &Client) -> Result<Vec<Session>, String> {
    let tl::enums::account::Authorizations::Authorizations(list) = client
        .invoke(&tl::functions::account::GetAuthorizations {})
        .await
        .map_err(|e| e.to_string())?;
    let mut out: Vec<Session> = list
        .authorizations
        .into_iter()
        .map(|a| {
            let tl::enums::Authorization::Authorization(a) = a;
            let device = [a.device_model.as_str(), a.platform.as_str(), a.system_version.as_str()]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" · ");
            let place = [a.region.as_str(), a.country.as_str()]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(", ");
            Session {
                hash: a.hash,
                current: a.current,
                device,
                app: format!("{} {}", a.app_name, a.app_version).trim().to_string(),
                place,
                ip: a.ip,
                active: a.date_active as i64,
            }
        })
        .collect();
    out.sort_by_key(|s| (!s.current, -s.active));
    Ok(out)
}

/// Chiude una sessione (`Some(hash)`) o tutte le altre (`None`).
pub async fn end_session(client: &Client, hash: Option<i64>) -> Result<(), String> {
    match hash {
        Some(hash) => client
            .invoke(&tl::functions::account::ResetAuthorization { hash })
            .await
            .map(drop),
        None => client
            .invoke(&tl::functions::auth::ResetAuthorizations {})
            .await
            .map(drop),
    }
    .map_err(|e| match e.to_string() {
        s if s.contains("FRESH_RESET_AUTHORISATION_FORBIDDEN") => {
            tr("una sessione appena aperta non può chiudere le altre: riprova fra 24 ore").to_string()
        }
        s => s,
    })
}

/// Server-side search inside a peer (text query, no filters).
pub async fn search_chat(
    client: &Client,
    peer_ref: ChatRef,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, InvocationError> {
    let mut out = Vec::new();
    if query.trim().is_empty() {
        return Ok(out);
    }
    let mut iter = client.search_messages(to_peer_ref(peer_ref)).query(query);
    while out.len() < limit {
        match iter.next().await? {
            Some(m) => {
                let text = m.text().to_string();
                let snippet = if text.is_empty() {
                    match m.media() {
                        Some(_) => "(media)".into(),
                        None => continue,
                    }
                } else {
                    caption(&text)
                };
                let date = m.date().to_string();
                let date = date.replace('T', " ").chars().take(16).collect::<String>();
                out.push(SearchHit {
                    outgoing: m.outgoing(),
                    from: sender_name(&m),
                    date,
                    snippet,
                });
            }
            None => break,
        }
    }
    Ok(out)
}

/// Scarica un documento (o una foto full) dentro `dest`.
pub async fn download_media_to(
    client: &Client,
    media: &MediaRef,
    dest: PathBuf,
) -> Result<(), String> {
    let media = media_of(media)?;
    std::fs::create_dir_all(
        dest.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".")),
    )
    .map_err(|e| e.to_string())?;
    client
        .download_media(media, dest.clone())
        .await
        .map_err(|e| e.to_string())
}

/// Scarica l'originale nella cache (se assente) e ne restituisce il
/// percorso con l'estensione reale; dei video basta la miniatura.
pub async fn fetch_raw(client: &Client, media: &MediaRef) -> Result<PathBuf, String> {
    let key = media.key();
    let inner = media_of(media)?;
    // video: basta la miniatura più grande, non il file intero
    if let Media::Document(doc) = inner {
        if doc.mime_type().unwrap_or("").starts_with("video/") {
            let raw_path = crate::loader::cache_raw(key, "jpg");
            if !raw_path.exists() {
                let thumb = doc
                    .thumbs()
                    .into_iter()
                    .max_by_key(|t| t.size())
                    .ok_or("video senza miniatura")?;
                ensure_private_dir(&crate::paths::media_dir())
                    .map_err(|e| format!("cache dir: {e}"))?;
                client
                    .download_media(&thumb, raw_path.clone())
                    .await
                    .map_err(|e| format!("download: {e}"))?;
            }
            return Ok(raw_path);
        }
    }
    let raw_path = crate::loader::cache_raw(key, media.ext());
    if !raw_path.exists() {
        ensure_private_dir(&crate::paths::media_dir())
            .map_err(|e| format!("cache dir: {e}"))?;
        client
            .download_media(inner, raw_path.clone())
            .await
            .map_err(|e| format!("download: {e}"))?;
    }
    Ok(raw_path)
}

/// Stable per-cache file name from media identity (foto e documenti).
pub fn media_key(media: &Media) -> String {
    let id = media_id(media);
    format!("{:x}", md5::compute(id.to_be_bytes()))
}

/// Unice id server-side dalla media (foto, documenti, sticker).
pub fn media_id(media: &Media) -> i64 {
    match media {
        Media::Sticker(st) => st.document.id(),
        Media::Document(doc) => doc.id(),
        Media::Photo(photo) => photo.id(),
        _ => -1,
    }
}

/// Convert a message Media into the (possibly) re-sent sticker set entry.
pub fn sticker_emoji(media: &Media) -> String {
    match media {
        Media::Sticker(sticker) => sticker.emoji().to_string(),
        _ => String::new(),
    }
}

fn chat_peer_id(chat: &tl::enums::Chat) -> Option<ChatId> {
    let pid = match chat {
        tl::enums::Chat::Chat(c) => PeerId::chat(c.id),
        tl::enums::Chat::Forbidden(c) => PeerId::chat(c.id),
        tl::enums::Chat::Channel(c) => PeerId::channel(c.id),
        tl::enums::Chat::ChannelForbidden(c) => PeerId::channel(c.id),
        tl::enums::Chat::Empty(_)
        | tl::enums::Chat::Community(_)
        | tl::enums::Chat::CommunityForbidden(_) => None,
    };
    pid.map(to_chat_id)
}

/// Chat pubblica per username o anteprima di un invito.
pub async fn resolve_link(client: &Client, link: TgLink) -> Result<LinkHit, String> {
    match link {
        TgLink::Public { name, post } => {
            let peer = client
                .resolve_username(&name)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| crate::trf!("@{} non esiste", name))?;
            let peer_ref = peer
                .to_ref()
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| crate::trf!("@{} non è raggiungibile", name))?;
            let (photo_id, seen) = peer_extras(&peer);
            Ok(LinkHit::Chat {
                dialog: DialogInfo {
                    muted: false,
                    archived: false,
                    unread_mark: false,
                    broadcast: is_broadcast(&peer),
                    photo_id,
                    seen,
                    name: peer.name().unwrap_or(&name).to_string(),
                    is_group: !matches!(peer, grammers_client::peer::Peer::User(_)),
                    peer_id: to_chat_id(peer.id()),
                    peer_ref: to_chat_ref(peer_ref),
                    unread: 0,
                    pinned: false,
                    last: None,
                    read_out: 0,
                },
                post,
            })
        }
        TgLink::Invite(hash) => {
            let res = client
                .invoke(&tl::functions::messages::CheckChatInvite { hash: hash.clone() })
                .await
                .map_err(|e| e.to_string())?;
            match res {
                tl::enums::ChatInvite::Already(a) => chat_peer_id(&a.chat)
                    .map(LinkHit::Already)
                    .ok_or_else(|| tr("chat dell'invito non valida").into()),
                tl::enums::ChatInvite::Peek(p) => chat_peer_id(&p.chat)
                    .map(LinkHit::Already)
                    .ok_or_else(|| tr("chat dell'invito non valida").into()),
                tl::enums::ChatInvite::Invite(i) => Ok(LinkHit::Invite {
                    hash,
                    title: i.title,
                    members: i.participants_count,
                }),
            }
        }
        TgLink::Private { .. } => Err(tr("chat privata: non è fra le tue").into()),
    }
}

/// Accetta un invito: la chat entra fra i dialoghi.
pub async fn join_invite(client: &Client, hash: String) -> Result<LinkHit, String> {
    let res = client
        .invoke(&tl::functions::messages::ImportChatInvite { hash })
        .await
        .map_err(|e| e.to_string())?;
    let tl::enums::messages::ChatInviteJoinResult::Ok(ok) = res else {
        return Err(tr("questo invito si apre solo nell'app ufficiale").into());
    };
    let chats = match ok.updates {
        tl::enums::Updates::Updates(u) => u.chats,
        tl::enums::Updates::Combined(u) => u.chats,
        _ => Vec::new(),
    };
    Ok(LinkHit::Joined(chats.iter().find_map(chat_peer_id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- tipi neutri ----------
    #[test]
    fn chat_id_andata_e_ritorno() {
        for pid in [
            PeerId::user_unchecked(777),
            PeerId::chat(4242).unwrap(),
            PeerId::channel(1234567890).unwrap(),
            PeerId::self_user(),
        ] {
            assert_eq!(to_peer_id(to_chat_id(pid)), pid);
        }
        // canali: gli stessi numeri dei link privati
        let ch = to_chat_id(PeerId::channel(1234567890).unwrap());
        assert_eq!(Some(ch), crate::links::channel_chat(1234567890));
        assert!(ch.own_msg_ids());
        assert!(!to_chat_id(PeerId::chat(4242).unwrap()).own_msg_ids());
    }

    #[test]
    fn chat_ref_porta_l_access_hash() {
        let r = PeerRef { id: PeerId::user_unchecked(5), auth: PeerAuth::from_hash(99) };
        assert_eq!(to_peer_ref(to_chat_ref(r)), r);
    }

    // ---------- normalize_phone ----------
    #[test]
    fn phone_with_plus() {
        assert_eq!(normalize_phone("+39 339 123 4567"), "+393391234567");
        assert_eq!(normalize_phone("+1 202 555 0100"), "+12025550100");
        assert_eq!(normalize_phone("+44 7700 900123"), "+447700900123");
    }

    #[test]
    fn phone_00_prefix() {
        assert_eq!(normalize_phone("0039 339 123 4567"), "+393391234567");
        assert_eq!(normalize_phone("0044 7700 900123"), "+447700900123");
    }

    #[test]
    fn phone_bare_italian_mobile() {
        assert_eq!(normalize_phone("339 123 4567"), "+393391234567");
        // col prefisso nazionale senza +: forma non nuda, ma le cifre
        // sono già le giuste → +39…
        assert_eq!(normalize_phone("39 339 123 4567"), "+393391234567");
    }

    #[test]
    fn phone_bare_foreign() {
        // tedesco: prima diventava +3949… (sbagliato)
        assert_eq!(normalize_phone("49 151 234 5678"), "+491512345678");
        // statunitense
        assert_eq!(normalize_phone("1 202 555 0100"), "+12025550100");
        // mobile italiano con prefisso completo senza +: le cifre sono
        // già internazionale-complete → +39…
        assert_eq!(normalize_phone("393391234567"), "+393391234567");
    }

    #[test]
    fn phone_garbage_chars_skipped() {
        assert_eq!(normalize_phone(" (339) 123-4567 "), "+393391234567");
    }
}
