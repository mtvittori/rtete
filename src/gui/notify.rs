//! Notifiche desktop con `notify-send`: una per chat (la nuova sostituisce
//! la vecchia), con la foto del contatto; il click apre la chat.

use std::collections::HashMap;
use std::io::BufRead;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use rondine::model::ChatId;

use crate::net;

/// id della notifica aperta per chat: la successiva la rimpiazza
static OPEN: Mutex<Option<HashMap<ChatId, u32>>> = Mutex::new(None);

pub fn show(env: net::Env, peer: ChatId, title: &str, body: &str, icon: Option<&std::path::Path>) {
    let replace = OPEN
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(&peer).copied()));
    let mut cmd = Command::new("notify-send");
    // il nome dell'app dice da quale ambiente arriva
    let app_name = match env {
        net::Env::Telegram => "Rondine · Telegram",
        net::Env::WhatsApp => "Rondine · WhatsApp",
    };
    cmd.args(["-a", app_name, "-p", "-A"]);
    cmd.arg(format!("default={}", rondine::tr("Apri")));
    match icon {
        Some(p) => cmd.arg("-i").arg(p),
        None => cmd.args(["-i", if env == net::Env::WhatsApp { "whatsapp" } else { "telegram" }]),
    };
    if let Some(id) = replace {
        cmd.arg("-r").arg(id.to_string());
    }
    cmd.arg("--").arg(title).arg(body);
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return;
    };
    // notify-send con un'azione resta in attesa finché la notifica vive:
    // un thread leggero per notifica (una per chat al massimo)
    std::thread::spawn(move || {
        let Some(out) = child.stdout.take() else {
            return;
        };
        let mut lines = std::io::BufReader::new(out).lines();
        if let Some(Ok(id)) = lines.next() {
            if let (Ok(id), Ok(mut g)) = (id.trim().parse::<u32>(), OPEN.lock()) {
                g.get_or_insert_with(HashMap::new).insert(peer, id);
            }
        }
        if let Some(Ok(action)) = lines.next() {
            if action.trim() == "default" {
                net::open_peer(env, peer);
                focus_window();
            }
        }
        let _ = child.wait();
        if let Ok(mut g) = OPEN.lock() {
            if let Some(m) = g.as_mut() {
                m.remove(&peer);
            }
        }
    });
}

/// Toglie la notifica della chat appena la si apre.
pub fn dismiss(peer: ChatId) {
    let id = OPEN
        .lock()
        .ok()
        .and_then(|mut g| g.as_mut().and_then(|m| m.remove(&peer)));
    if let Some(id) = id {
        let _ = Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest=org.freedesktop.Notifications",
                "--object-path=/org/freedesktop/Notifications",
                "--method=org.freedesktop.Notifications.CloseNotification",
            ])
            .arg(id.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

fn focus_window() {
    crate::focus_existing();
}
