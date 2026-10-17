//! Telegram backend: grammers 0.11 (MTProto client in pure Rust, no TDLib).

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;

use grammers_client::media::Media;
use grammers_client::message::InputMessage;
use grammers_client::sender::SenderPool;
use grammers_client::session::storages::SqliteSession;
use grammers_client::session::types::PeerId;
use grammers_client::session::updates::UpdatesLike;
use grammers_client::tl;
use grammers_client::{Client, InvocationError, SignInError};

pub type ChatRef = grammers_client::session::types::PeerRef;

/// One conversation row in the sidebar.
pub struct DialogInfo {
    pub name: String,
    pub username: Option<String>,
    pub peer: grammers_client::peer::Peer,
    pub peer_ref: ChatRef,
    pub peer_id: PeerId,
    pub is_group: bool,
    pub unread: usize,
}

/// One message rendered in the history pane.
pub struct HistoryRow {
    pub id: i32,
    pub outgoing: bool,
    /// nome del mittente (vuoto per i messaggi propri: si usa quello dell'utente).
    pub from: String,
    pub kind: RowKind,
}

pub enum RowKind {
    Text(String),
    Sticker {
        media: Media,
        emoji: String,
        animated: bool,
    },
    /// foto Telegram, allegato immagine o gif: renderizzabili in kitty.
    Photo {
        media: Media,
        caption: String,
        gif: bool,
    },
    /// documento scaricabile (Invio = salva in ~/Downloads).
    Doc {
        media: Media,
        name: String,
        size: Option<usize>,
        caption: String,
    },
    /// etichetta informativa: video, vocale, contatti, sondaggi…
    Note(String),
    Service(String),
}

/// Caption ripulita: righe collassate, tagliata a lunghezza ragionevole.
fn caption(text: &str) -> String {
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
        format!("{bytes:.1} {}", UNITS[unit])
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
        "messaggio vocale"
    } else if mime.starts_with("audio/") {
        "audio"
    } else {
        "media non supportato"
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

/// Connection credentials for my.telegram.org apps.
pub struct Credentials {
    pub api_id: i32,
    pub api_hash: String,
}

fn config_dir() -> PathBuf {
    // specifica XDG: la dir specifica dell'app è base + "/rtete".
    // Le var XDG contano solo se assolute, altrimenti si riparte da HOME.
    let base = match std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|p| std::path::Path::new(p).is_absolute())
    {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => std::env::var("HOME")
            .ok()
            .map(|h| PathBuf::from(h).join(".config"))
            .unwrap_or_else(|| PathBuf::from(".")),
    };
    base.join("rtete")
}

fn data_dir() -> PathBuf {
    let base = match std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|p| std::path::Path::new(p).is_absolute())
    {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => std::env::var("HOME")
            .ok()
            .map(|h| PathBuf::from(h).join(".local/share"))
            .unwrap_or_else(|| PathBuf::from(".")),
    };
    base.join("rtete")
}

pub fn media_dir() -> PathBuf {
    data_dir().join("assets")
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
    println!("Prima configurazione di RTete.");
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
    std::fs::create_dir_all(config_dir())?;
    let cred_path = config_dir().join(CONFIG_FILE);
    std::fs::write(&cred_path, format!("{} {}\n", creds.api_id, creds.api_hash))?;
    // il file contiene l'api_hash: visibile solo all'utente
    if let Ok(mut perms) = std::fs::metadata(&cred_path).map(|m| m.permissions()) {
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(&cred_path, perms);
    }
    Ok(creds)
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
    std::fs::create_dir_all(data_dir())?;
    let session = Arc::new(SqliteSession::open(data_dir().join("rtete.db")).await?);
    // la sessione contiene le chiavi di autorizzazione: solo l'utente
    if let Ok(mut perms) = std::fs::metadata(data_dir().join("rtete.db")).map(|m| m.permissions()) {
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(data_dir().join("rtete.db"), perms);
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

/// Normalizza un numero di telefono all'unico formato che Telegram accetta:
/// solo cifre con `+` davanti. Accetta spazi/trattini/parentesi, `00…` al
/// posto di `+`, o il numero nudo da mobile italiano (prepend `+39`).
fn normalize_phone(raw: &str) -> String {
    let t = raw.trim();
    let has_plus = t.starts_with('+');
    let mut digits: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    if !has_plus && digits.starts_with("00") {
        digits = digits[2..].to_string();
    }
    if has_plus || (digits.starts_with("39") && digits.len() >= 11) {
        format!("+{digits}")
    } else {
        format!("+39{digits}")
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
    println!("⚠️  Digita ogni dato SOLO quando compare la domanda, poi conferma con Invio.");
    println!("    (Se scrivi prima che la richiesta appaia, l'input va perso.)");
    let phone = loop {
        let _ = write!(std::io::stdout(), "Numero di telefono (+39…): ");
        let _ = std::io::stdout().flush();
        let mut phone = String::new();
        if std::io::stdin().read_line(&mut phone)? == 0 {
            return Err("input chiuso".into());
        }
        let phone = normalize_phone(&phone);
        // log mascherato per diagnosi: niente numero completo su disco
        let shown = &phone[..phone.len().min(4)];
        eprintln!(
            "[rtete] tel normalizzato: {shown}… ({} cifre)",
            phone.len() - 1
        );
        // "+39" da solo o righe spurie: richiedi invece di chiamare Telegram
        if phone.len() >= 7 {
            break phone;
        }
        println!(
            "Numero troppo corto. Usa il solo mobile (es. 3391234567) o il formato +39 completo."
        );
    };
    let token = client.request_login_code(&phone, &creds.api_hash).await?;
    let _ = write!(std::io::stdout(), "Codice ricevuto: ");
    let _ = std::io::stdout().flush();
    let mut code = String::new();
    std::io::stdin().read_line(&mut code)?;
    let user = match client.sign_in(&token, code.trim()).await {
        Ok(user) => user,
        Err(SignInError::PasswordRequired(token)) => {
            let hint = token.hint().map(str::to_string).unwrap_or_default();
            let _ = write!(std::io::stdout(), "Password 2FA ({hint}): ");
            let _ = std::io::stdout().flush();
            let mut pass = String::new();
            std::io::stdin().read_line(&mut pass)?;
            client.check_password(token, pass.trim()).await?
        }
        Err(SignInError::SignUpRequired) => {
            return Err("Account senza Telegram: crea prima un account con l'app ufficiale.".into())
        }
        Err(SignInError::InvalidCode) => return Err("Codice errato, riavvia e riprova.".into()),
        Err(SignInError::InvalidPassword(_)) => return Err("2FA errata, riavvia e riprova.".into()),
        Err(e) => return Err(e.into()),
    };
    println!("Connesso come {}", user.full_name());
    Ok(())
}

/// List the dialogs (chats) of the account.
pub async fn load_dialogs(client: &Client) -> Result<Vec<DialogInfo>, InvocationError> {
    let mut out = Vec::new();
    let mut iter = client.iter_dialogs();
    while let Some(dialog) = iter.next().await? {
        let peer = dialog.peer().clone();
        let name = peer.name().unwrap_or("senza nome").to_string();
        let is_group = !matches!(peer, grammers_client::peer::Peer::User(_));
        let read_unread = match &dialog.raw {
            tl::enums::Dialog::Dialog(raw) => raw.unread_count.max(0) as usize,
            _ => 0,
        };
        out.push(DialogInfo {
            name,
            username: peer.username().map(str::to_string),
            peer,
            peer_ref: dialog.peer_ref(),
            peer_id: dialog.peer_id(),
            is_group,
            unread: read_unread,
        });
    }
    Ok(out)
}

/// Parse into a projected row: sticker / foto / gif / file / nota / testo.
pub fn row_kind(media: Option<Media>, text: String) -> RowKind {
    let cap = caption(&text);
    match media.clone() {
        Some(Media::Sticker(sticker)) => {
            let emoji = sticker.emoji().to_string();
            let animated = sticker.is_animated();
            RowKind::Sticker {
                media: media.unwrap(),
                emoji,
                animated,
            }
        }
        Some(Media::Photo(_)) => RowKind::Photo {
            media: media.unwrap(),
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
                    media: media.unwrap(),
                    caption: cap,
                    gif: true,
                };
            }
            if matches!(mime.as_str(), "image/webp" | "image/png" | "image/apng")
                || mime.starts_with("image/jpeg")
            {
                return RowKind::Photo {
                    media: media.unwrap(),
                    caption: cap,
                    gif: false,
                };
            }
            if mime.starts_with("video/") || mime.starts_with("audio/") {
                return RowKind::Note(media_note(&doc, &text));
            }
            RowKind::Doc {
                media: media.unwrap(),
                name,
                size,
                caption: cap,
            }
        }
        // anteprime dei link: conta solo il testo del messaggio
        Some(Media::WebPage(_)) => RowKind::Text(text),
        Some(other) => RowKind::Note(match other {
            Media::Contact(c) => format!("📇 contatto: {} {}", c.first_name(), c.last_name()),
            Media::Poll(p) => match p.question() {
                tl::enums::TextWithEntities::Entities(t) => {
                    format!("📊 sondaggio: {}", t.text)
                }
            },
            Media::Geo(_) | Media::GeoLive(_) => "📍 posizione".to_string(),
            Media::Venue(_) => "🗺 luogo".to_string(),
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
    let mut iter = client.iter_messages(peer_ref).limit(limit);
    while let Some(m) = iter.next().await? {
        let id = m.id();
        let outgoing = m.outgoing();
        let from = sender_name(&m);
        let kind = row_kind(m.media(), m.text().to_string());
        rows.push(HistoryRow {
            id,
            outgoing,
            from,
            kind,
        });
    }
    rows.reverse();
    Ok(rows)
}

/// One installed sticker set.
pub struct StickerSetInfo {
    pub set_id: i64,
    pub access_hash: i64,
    pub title: String,
    pub short_name: String,
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
                access_hash: s.access_hash,
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
) -> Result<grammers_client::message::Message, InvocationError> {
    client.send_message(peer_ref, text.to_string()).await
}

/// Send a sticker reusing its server-side file reference (`copy_media`
/// passa lo stesso documento — niente ri-upload).
pub async fn send_sticker(
    client: &Client,
    peer_ref: ChatRef,
    media: &Media,
) -> Result<grammers_client::message::Message, InvocationError> {
    client
        .send_message(peer_ref, InputMessage::new().copy_media(media))
        .await
}

/// Modifica in place un testo già inviato (restituito id invariato).
pub async fn edit_text(
    client: &Client,
    peer_ref: ChatRef,
    message_id: i32,
    new_text: &str,
) -> Result<(), InvocationError> {
    client
        .edit_message(peer_ref, message_id, InputMessage::new().text(new_text))
        .await
}

/// Elimina definitivamente (revoke) gli id passati della chat.
pub async fn delete_messages(
    client: &Client,
    peer_ref: ChatRef,
    ids: &[i32],
) -> Result<(), InvocationError> {
    client.delete_messages(peer_ref, ids).await?;
    Ok(())
}

/// Un colpo da ricerca: id, data, autore e testo potentemente tagsliato.
pub struct SearchHit {
    pub id: i32,
    pub outgoing: bool,
    pub from: String,
    pub date: String,
    pub snippet: String,
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
    let mut iter = client.search_messages(peer_ref).query(query);
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
                    id: m.id(),
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
    media: &Media,
    dest: PathBuf,
) -> Result<(), String> {
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

/// Confronto peer (PeerId value equality).
pub fn same_peer(a: PeerId, b: PeerId) -> bool {
    a == b
}
