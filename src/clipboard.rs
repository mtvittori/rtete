//! Appunti (wl-clipboard su Linux, arboard altrove): probe a thread fuori dal ciclo eventi, incolla testo e foto.

use tokio::sync::mpsc;

use crate::app::App;
use crate::send::Response;
use crate::model;

/// Esito del probe degli appunti (il probe gira a thread, fuori dal
/// ciclo degli eventi): immagine con byte e mime, file copiati dal file
/// manager, testo, oppure vuoto.
pub enum ClipboardHit {
    Image(Vec<u8>, &'static str),
    Files(Vec<std::path::PathBuf>),
    Text(String),
    Empty,
}

/// Testo "copiabile" di una riga: testo, caption (foto/doc/audio), emoji
/// degli sticker, note di servizio. None se la riga non ha testo.
pub fn copy_text(r: &model::HistoryRow) -> Option<String> {
    match &r.kind {
        model::RowKind::Text(t) => Some(t.clone()),
        model::RowKind::Note(t) | model::RowKind::Service(t) | model::RowKind::Video { note: t, .. } => {
            let t = t.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        model::RowKind::Sticker { emoji, .. } => {
            let t = emoji.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        model::RowKind::Photo { caption, .. } => {
            let t = caption.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        model::RowKind::Doc { name, caption, .. } => {
            let t = pick_caption(caption, name);
            (!t.is_empty()).then(|| t.to_string())
        }
        model::RowKind::Audio { name, caption, .. } => {
            let t = pick_caption(caption, name);
            (!t.is_empty()).then(|| t.to_string())
        }
    }
}

/// preferisce la caption; in mancanza il nome (per gli allegati).
pub fn pick_caption<'a>(caption: &'a str, name: &'a str) -> &'a str {
    let c = caption.trim();
    if c.is_empty() {
        name.trim()
    } else {
        c
    }
}

/// Super/Ctrl-Y: copia negli appunti il messaggio selezionato (o senza
/// selezione, l'ultimo messaggio con testo della chat). Su Linux wl-copy si serve
/// da sé in background; altrove arboard.
pub fn ctrl_copy(app: &mut App) {
    app.status.clear();
    if app.show_search || app.show_sticker {
        return;
    }
    let Some(chat) = app.current_chat() else {
        app.status = crate::tr("apri prima una chat").into();
        return;
    };
    let ring = app.histories.get(&chat.peer_id);
    let picked = match (ring, app.msg_cursor) {
        (Some(ring), Some(id)) => ring.iter().rev().find(|r| r.id == id).and_then(copy_text),
        (Some(ring), None) => ring.iter().rev().find_map(copy_text),
        (None, _) => None,
    };
    let Some(text) = picked else {
        app.status = crate::tr("niente testo da copiare in questo messaggio").into();
        return;
    };
    let head: String = text.chars().take(24).collect();
    app.status = crate::trf!("copiato negli appunti: «{}…»", head);
    std::thread::spawn(move || copy_to_clipboard(text));
}

#[cfg(target_os = "linux")]
fn copy_to_clipboard(text: String) {
    // il testo va su stdin: come argomento, un messaggio che inizia
    // per "-" ("Adesso -n ciao") veniva preso per una opzione wl-copy
    use std::io::Write;
    use std::process::{Command, Stdio};
    let spawned = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(mut ch) => {
            if let Some(mut si) = ch.stdin.take() {
                let _ = si.write_all(text.as_bytes());
            } // drop → EOF: wl-copy legge il contenuto e resta a servire
              // i paste finché il clipboard non viene rimpiazzato
            let _ = ch.wait();
        }
        Err(_) => eprintln!("[rondine] wl-copy non trovato (installa wl-clipboard)"),
    }
}

#[cfg(not(target_os = "linux"))]
fn copy_to_clipboard(text: String) {
    // Windows/macOS: gli appunti li tiene il sistema anche dopo il drop
    if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
        eprintln!("[rondine] appunti: {e}");
    }
}

/// Pulisce il testo incollato: via i caratteri di controllo, le nuove
/// righe restano nel compose (messaggi multi-riga) e diventano spazi
/// nella ricerca. `cap` limita i caratteri (Telegram: 4096).
pub fn paste_text(raw: &str, keep_nl: bool, cap: usize) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if out.chars().count() >= cap {
            break;
        }
        match ch {
            '\r' => {}
            '\n' if keep_nl => out.push('\n'),
            '\n' => out.push(' '),
            '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    if keep_nl {
        out.trim_end().to_string()
    } else {
        while out.contains("  ") {
            out = out.replace("  ", " ");
        }
        out.trim().to_string()
    }
}

/// Consegna del probe degli appunti (girava a thread fuori dal ciclo):
/// lo stato della UI viene rileto qui, non al momento del Ctrl-V.
pub fn handle_clipboard(app: &mut App, hit: ClipboardHit) {
    match hit {
        ClipboardHit::Image(bytes, mime) => match clipboard_png_file(&bytes, mime) {
            Some((path, kb)) => {
                app.discard_pending_photo();
                app.disarm_delete();
                app.pending_photo = Some(path.display().to_string());
                app.status = crate::trf!(
                    "foto ({} KB) pronta — Invio invia, il testo del compose è la didascalia",
                    kb
                );
            }
            None => app.status = crate::tr("appunti: non leggo l'immagine").into(),
        },
        ClipboardHit::Files(paths) => {
            let n = paths.len();
            if let Some(path) = paths.into_iter().next() {
                attach_file(app, path);
                if n > 1 {
                    app.status.push_str(&crate::trf!(" (solo il primo di {} file)", n));
                }
            }
        }
        ClipboardHit::Text(text) => {
            if app.show_search {
                let clean = paste_text(&text, false, 200);
                app.search_q.push_str(&clean);
                app.status = crate::tr("appunti incollati nella ricerca").into();
            } else if app.open.is_some() && app.editing.is_none() {
                let cap = app.compose_cap().saturating_sub(app.compose.chars().count());
                let clean = paste_text(&text, true, cap);
                app.compose.push_str(&clean);
                app.disarm_delete();
                let n = clean.chars().count();
                app.status = crate::trf!("appunti: {} caratteri incollati", n);
            } else {
                app.status = crate::tr("apri prima una chat").into();
            }
        }
        ClipboardHit::Empty => {
            app.status = if cfg!(target_os = "linux") {
                crate::tr("appunti vuoti (serve wl-clipboard)")
            } else {
                crate::tr("appunti vuoti")
            }
            .into()
        }
    }
}

/// Un file del disco pronto da inviare: il testo del compose sarà la
/// didascalia. Prende il posto di un'eventuale foto in attesa.
pub fn attach_file(app: &mut App, path: std::path::PathBuf) {
    if !path.is_file() {
        app.status = crate::trf!("non è un file: {}", path.display());
        return;
    }
    app.discard_pending_photo();
    app.disarm_delete();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    app.status = if crate::model::is_photo_file(&path) {
        crate::trf!("foto «{}» pronta — Invio invia, il testo del compose è la didascalia", name)
    } else {
        crate::trf!("file «{}» pronto — Invio invia, il testo del compose è la didascalia", name)
    };
    app.pending_file = Some(path);
}

/// I file di un text/uri-list (file manager): solo gli URI file:// locali,
/// con i %XX decodificati.
pub fn uri_list_paths(list: &str) -> Vec<std::path::PathBuf> {
    list.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        // file://host/percorso: l'host (di solito vuoto) va saltato
        .filter_map(|l| l.find('/').map(|i| &l[i..]))
        .map(|l| std::path::PathBuf::from(percent_decode(l)))
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------- appunti (wl-clipboard su Linux, arboard altrove) ---
// Ctrl-V non blocca più il ciclo: il probe gira a thread e la conseguenza
// arriva come Response::Clipboard, con lo stato della UI rilegto allora.

/// wl-paste con argomenti extra: `None` se il processo manca o fallisce.
#[cfg(target_os = "linux")]
pub fn wl_paste_bytes(extra: &[&str]) -> Option<Vec<u8>> {
    let mut cmd = std::process::Command::new("wl-paste");
    for a in extra {
        cmd.arg(a);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .output()
        .ok()
        .map(|o| o.stdout)
}

/// Tipi e byte degli appunti: prima i file copiati, poi l'immagine
/// (png/jpeg), poi il testo.
#[cfg(target_os = "linux")]
pub fn probe_clipboard() -> ClipboardHit {
    let types = String::from_utf8_lossy(&wl_paste_bytes(&["--list-types"]).unwrap_or_default())
        .into_owned();
    if types.lines().any(|t| t.trim() == "text/uri-list") {
        let list = wl_paste_bytes(&["--type", "text/uri-list"]).unwrap_or_default();
        let paths = uri_list_paths(&String::from_utf8_lossy(&list));
        if !paths.is_empty() {
            return ClipboardHit::Files(paths);
        }
    }
    let mime = ["image/png", "image/jpeg"]
        .iter()
        .copied()
        .find(|m| types.lines().any(|t| t.trim().eq_ignore_ascii_case(m)));
    if let Some(m) = mime {
        if let Some(bytes) = wl_paste_bytes(&["--type", m]) {
            return ClipboardHit::Image(bytes, m);
        }
    }
    let text = String::from_utf8_lossy(&wl_paste_bytes(&[]).unwrap_or_default()).into_owned();
    if text.is_empty() {
        ClipboardHit::Empty
    } else {
        ClipboardHit::Text(text)
    }
}

/// Fuori da Linux (arboard): prima l'immagine (convertita in PNG), poi il
/// testo. I file copiati dal file manager non si leggono.
#[cfg(not(target_os = "linux"))]
pub fn probe_clipboard() -> ClipboardHit {
    let Ok(mut cb) = arboard::Clipboard::new() else {
        return ClipboardHit::Empty;
    };
    if let Ok(img) = cb.get_image() {
        if let Some(png) = rgba_to_png(img.width, img.height, &img.bytes) {
            return ClipboardHit::Image(png, "image/png");
        }
    }
    match cb.get_text() {
        Ok(t) if !t.is_empty() => ClipboardHit::Text(t),
        _ => ClipboardHit::Empty,
    }
}

/// I pixel RGBA degli appunti in un PNG (come quelli di wl-paste).
#[cfg(not(target_os = "linux"))]
fn rgba_to_png(width: usize, height: usize, rgba: &[u8]) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(width as u32, height as u32, rgba.to_vec())?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// L'immagine degli appunti in file in tmp (0600): (percorso, KB).
pub fn clipboard_png_file(bytes: &[u8], mime: &str) -> Option<(std::path::PathBuf, usize)> {
    use std::io::Write as _;
    if bytes.is_empty() {
        return None;
    }
    // tmp sta dentro data_dir (0700): uno screenshot non resta leggibile
    // dagli altri utenti come farebbe un 644
    let dir = crate::paths::tmp_dir();
    crate::paths::ensure_private_dir(&dir).ok()?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let ext = if mime == "image/jpeg" { "jpg" } else { "png" };
    let path = dir.join(format!("appunti-{ts}.{ext}"));
    // 0600 sin dalla creazione, non scritto con la umask e poi sistemato
    crate::paths::private_file()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .and_then(|mut f| f.write_all(bytes))
        .ok()?;
    Some((path, bytes.len() / 1024))
}

/// Ctrl-V non blocca la UI: il probe gira a thread fuori dal
/// ciclo; il risultato torna come Response::Clipboard mentre l'utente
/// continua a usare il client (luce a volte i processi si inceppano).
pub fn ctrl_paste(app: &mut App, res: &mpsc::UnboundedSender<Response>) {
    app.disarm_delete();
    if !(app.show_search || (app.open.is_some() && app.editing.is_none())) {
        return;
    }
    app.status = crate::tr("appunti…").into();
    let res = res.clone();
    std::thread::spawn(move || {
        let hit = probe_clipboard();
        let _ = res.send(Response::Clipboard(hit));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_list_paths_decodes_local_files() {
        let list = "# commento\r\nfile:///home/m/Foto%20mare.jpg\r\nfile://host/tmp/a%2Fb\r\nhttps://x.it/y\r\n";
        assert_eq!(
            uri_list_paths(list),
            vec![
                std::path::PathBuf::from("/home/m/Foto mare.jpg"),
                std::path::PathBuf::from("/tmp/a/b"),
            ]
        );
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%e2%82%ac%zz"), "€%zz");
    }
}
