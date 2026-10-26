//! Appunti (wl-clipboard): probe a thread fuori dal ciclo eventi, incolla testo e foto.

use std::os::unix::fs::OpenOptionsExt;

use tokio::sync::mpsc;

use crate::app::App;
use crate::send::Response;
use crate::tg;

/// Esito del probe degli appunti (wl-clipboard gira a thread, fuori dal
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
pub fn copy_text(r: &tg::HistoryRow) -> Option<String> {
    match &r.kind {
        tg::RowKind::Text(t) => Some(t.clone()),
        tg::RowKind::Note(t) | tg::RowKind::Service(t) | tg::RowKind::Video { note: t, .. } => {
            let t = t.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        tg::RowKind::Sticker { emoji, .. } => {
            let t = emoji.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        tg::RowKind::Photo { caption, .. } => {
            let t = caption.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        tg::RowKind::Doc { name, caption, .. } => {
            let t = pick_caption(caption, name);
            (!t.is_empty()).then(|| t.to_string())
        }
        tg::RowKind::Audio { name, caption, .. } => {
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
/// selezione, l'ultimo messaggio con testo della chat). wl-copy si serve
/// da sé in background: nessuna libreria in-process.
pub fn ctrl_copy(app: &mut App) {
    app.status.clear();
    if app.show_search || app.show_sticker {
        return;
    }
    let Some(chat) = app.current_chat() else {
        app.status = "apri prima una chat".into();
        return;
    };
    let ring = app.histories.get(&chat.peer_id);
    let picked = match (ring, app.msg_cursor) {
        (Some(ring), Some(id)) => ring.iter().rev().find(|r| r.id == id).and_then(copy_text),
        (Some(ring), None) => ring.iter().rev().find_map(copy_text),
        (None, _) => None,
    };
    let Some(text) = picked else {
        app.status = "niente testo da copiare in questo messaggio".into();
        return;
    };
    let head: String = text.chars().take(24).collect();
    app.status = format!("copiato negli appunti: «{head}…»");
    std::thread::spawn(move || {
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
    });
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
                app.status = format!(
                    "foto ({kb} KB) pronta — Invio invia, il testo del compose è la didascalia"
                );
            }
            None => app.status = "appunti: non leggo l'immagine".into(),
        },
        ClipboardHit::Files(paths) => {
            let n = paths.len();
            if let Some(path) = paths.into_iter().next() {
                attach_file(app, path);
                if n > 1 {
                    app.status.push_str(&format!(" (solo il primo di {n} file)"));
                }
            }
        }
        ClipboardHit::Text(text) => {
            if app.show_search {
                let clean = paste_text(&text, false, 200);
                app.search_q.push_str(&clean);
                app.status = "appunti incollati nella ricerca".into();
            } else if app.open.is_some() && app.editing.is_none() {
                let cap = app.compose_cap().saturating_sub(app.compose.chars().count());
                let clean = paste_text(&text, true, cap);
                app.compose.push_str(&clean);
                app.disarm_delete();
                let n = clean.chars().count();
                app.status = format!("appunti: {n} caratteri incollati");
            } else {
                app.status = "apri prima una chat".into();
            }
        }
        ClipboardHit::Empty => app.status = "appunti vuoti (serve wl-clipboard)".into(),
    }
}

/// Un file del disco pronto da inviare: il testo del compose sarà la
/// didascalia. Prende il posto di un'eventuale foto in attesa.
pub fn attach_file(app: &mut App, path: std::path::PathBuf) {
    if !path.is_file() {
        app.status = format!("non è un file: {}", path.display());
        return;
    }
    app.discard_pending_photo();
    app.disarm_delete();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let what = if tg::is_photo_file(&path) { "foto" } else { "file" };
    app.status = format!("{what} «{name}» pronto — Invio invia, il testo del compose è la didascalia");
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

// ---------------- appunti (wl-clipboard) ------------------------------
// Ctrl-V non blocca più il ciclo: il probe gira a thread e la conseguenza
// arriva come Response::Clipboard, con lo stato della UI rilegto allora.

/// wl-paste con argomenti extra: `None` se il processo manca o fallisce.
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

/// L'immagine degli appunti in file in tmp (0600): (percorso, KB).
pub fn clipboard_png_file(bytes: &[u8], mime: &str) -> Option<(std::path::PathBuf, usize)> {
    use std::io::Write as _;
    if bytes.is_empty() {
        return None;
    }
    // tmp sta dentro data_dir (0700): uno screenshot non resta leggibile
    // dagli altri utenti come farebbe un 644
    let dir = tg::tmp_dir();
    tg::ensure_private_dir(&dir).ok()?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let ext = if mime == "image/jpeg" { "jpg" } else { "png" };
    let path = dir.join(format!("appunti-{ts}.{ext}"));
    // 0600 sin dalla creazione, non scritto con la umask e poi sistemato
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut f| f.write_all(bytes))
        .ok()?;
    Some((path, bytes.len() / 1024))
}

/// Ctrl-V non blocca la UI: il probe wl-paste gira a thread fuori dal
/// ciclo; il risultato torna come Response::Clipboard mentre l'utente
/// continua a usare il client (luce a volte i processi si inceppano).
pub fn ctrl_paste(app: &mut App, res: &mpsc::UnboundedSender<Response>) {
    app.disarm_delete();
    if !(app.show_search || (app.open.is_some() && app.editing.is_none())) {
        return;
    }
    app.status = "appunti…".into();
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
