//! Messaggi importanti (la stella di WhatsApp): Telegram non ha niente
//! di simile, quindi restano sul PC in `~/.local/share/rondine/starred.json`
//! con una copia del testo, così la lista si legge anche senza rete.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Star {
    /// la chat, in formato Bot API
    pub chat: i64,
    pub chat_name: String,
    pub id: i32,
    /// secondi Unix
    pub date: i64,
    pub from: String,
    pub text: String,
}

fn path() -> PathBuf {
    crate::tg::data_dir().join("starred.json")
}

pub fn load() -> Vec<Star> {
    let Ok(raw) = std::fs::read_to_string(path()) else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str(&raw) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|v| {
            Some(Star {
                chat: v["chat"].as_i64()?,
                chat_name: v["chat_name"].as_str().unwrap_or_default().to_string(),
                id: v["id"].as_i64()? as i32,
                date: v["date"].as_i64().unwrap_or(0),
                from: v["from"].as_str().unwrap_or_default().to_string(),
                text: v["text"].as_str().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

pub fn save(list: &[Star]) {
    let items: Vec<serde_json::Value> = list
        .iter()
        .map(|s| {
            serde_json::json!({
                "chat": s.chat, "chat_name": s.chat_name, "id": s.id,
                "date": s.date, "from": s.from, "text": s.text,
            })
        })
        .collect();
    if crate::tg::ensure_private_dir(&crate::tg::data_dir()).is_ok() {
        let _ = std::fs::write(path(), serde_json::Value::Array(items).to_string());
    }
}
