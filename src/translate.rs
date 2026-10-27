//! Traduzione dei messaggi con un servizio esterno: DeepL oppure un
//! servizio compatibile OpenAI (Groq, OpenRouter, Ollama…). Le chiamate
//! sono HTTP sincrone (ureq): il worker le fa girare in `spawn_blocking`.
//!
//! La configurazione sta in `~/.config/rondine/translate.conf` (0600:
//! contiene le chiavi API) e la leggono sia la GUI sia la TUI.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use crate::model::ChatId;

use crate::i18n::{self, tr};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provider {
    DeepL,
    /// qualunque servizio con l'API chat/completions di OpenAI
    OpenAi,
}

/// Groq di default: veloce, con un piano gratuito.
pub const DEFAULT_OA_URL: &str = "https://api.groq.com/openai/v1";
pub const DEFAULT_OA_MODEL: &str = "llama-3.3-70b-versatile";

/// Servizi compatibili OpenAI pronti: (nome, indirizzo, modello).
pub const PRESETS: [(&str, &str, &str); 3] = [
    ("Groq", DEFAULT_OA_URL, DEFAULT_OA_MODEL),
    ("OpenRouter", "https://openrouter.ai/api/v1", "meta-llama/llama-3.3-70b-instruct"),
    ("Ollama", "http://localhost:11434/v1", "llama3.2"),
];

/// Le lingue di destinazione proposte: (codice, nome nella lingua stessa).
pub const TARGETS: [(&str, &str); 12] = [
    ("it", "Italiano"),
    ("en", "English"),
    ("de", "Deutsch"),
    ("fr", "Français"),
    ("es", "Español"),
    ("pt", "Português"),
    ("nl", "Nederlands"),
    ("pl", "Polski"),
    ("ru", "Русский"),
    ("uk", "Українська"),
    ("ja", "日本語"),
    ("zh", "中文"),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub provider: Provider,
    pub deepl_key: String,
    pub oa_url: String,
    pub oa_key: String,
    pub oa_model: String,
    /// lingua di destinazione; `None` = quella dell'interfaccia
    pub target: Option<String>,
    /// chat tradotte da sole (id in formato Bot API)
    pub auto: Vec<i64>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: Provider::DeepL,
            deepl_key: String::new(),
            oa_url: DEFAULT_OA_URL.into(),
            oa_key: String::new(),
            oa_model: DEFAULT_OA_MODEL.into(),
            target: None,
            auto: Vec::new(),
        }
    }
}

fn path() -> PathBuf {
    crate::paths::config_dir().join("translate.conf")
}

impl Config {
    pub fn load() -> Self {
        let mut c = Self::default();
        let Ok(raw) = std::fs::read_to_string(path()) else {
            return c;
        };
        for (k, v) in raw.lines().filter_map(|l| l.split_once('=')) {
            let v = v.trim().to_string();
            match k.trim() {
                "servizio" => c.provider = if v == "openai" { Provider::OpenAi } else { Provider::DeepL },
                "deepl_chiave" => c.deepl_key = v,
                "openai_url" if !v.is_empty() => c.oa_url = v,
                "openai_chiave" => c.oa_key = v,
                "openai_modello" if !v.is_empty() => c.oa_model = v,
                "lingua" => c.target = (!v.is_empty() && v != "auto").then_some(v),
                "auto" => c.auto = v.split(',').filter_map(|s| s.trim().parse().ok()).collect(),
                _ => {}
            }
        }
        c
    }

    /// Salva con permessi 0600: il file contiene le chiavi API.
    pub fn save(&self) -> std::io::Result<()> {
        crate::paths::ensure_private_dir(&crate::paths::config_dir())?;
        let provider = match self.provider {
            Provider::DeepL => "deepl",
            Provider::OpenAi => "openai",
        };
        let auto: Vec<String> = self.auto.iter().map(|i| i.to_string()).collect();
        let body = format!(
            "servizio = {provider}\ndeepl_chiave = {}\nopenai_url = {}\nopenai_chiave = {}\nopenai_modello = {}\nlingua = {}\nauto = {}\n",
            self.deepl_key.trim(),
            self.oa_url.trim(),
            self.oa_key.trim(),
            self.oa_model.trim(),
            self.target.as_deref().unwrap_or("auto"),
            auto.join(","),
        );
        let mut f = crate::paths::private_file()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path())?;
        f.write_all(body.as_bytes())
    }

    /// C'è una chiave per il servizio scelto?
    pub fn ready(&self) -> bool {
        match self.provider {
            Provider::DeepL => !self.deepl_key.trim().is_empty(),
            // Ollama in locale non vuole chiavi: basta l'indirizzo
            Provider::OpenAi => !self.oa_key.trim().is_empty() || self.oa_url.contains("localhost"),
        }
    }

    /// La lingua in cui tradurre (codice ISO a due lettere).
    pub fn target_lang(&self) -> String {
        self.target.clone().unwrap_or_else(|| i18n::lang().code().to_string())
    }

    pub fn is_auto(&self, peer: ChatId) -> bool {
        self.auto.contains(&peer.0)
    }

    pub fn set_auto(&mut self, peer: ChatId, on: bool) {
        let id = peer.0;
        self.auto.retain(|x| *x != id);
        if on {
            self.auto.push(id);
        }
    }
}

/// Esito di una traduzione.
#[derive(Clone, Debug, PartialEq)]
pub struct Translated {
    pub text: String,
    /// lingua d'origine riconosciuta (codice a due lettere, minuscolo)
    pub source: Option<String>,
}

impl Translated {
    /// Il messaggio era già nella lingua di destinazione?
    pub fn same_as(&self, target: &str) -> bool {
        self.source.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(target))
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(25)))
        // gli errori HTTP si leggono dal corpo, con un messaggio chiaro
        .http_status_as_error(false)
        .build()
        .into()
}

/// Traduce `text` verso `target` col servizio configurato. Bloccante.
pub fn translate(cfg: &Config, text: &str, target: &str) -> Result<Translated, String> {
    match cfg.provider {
        Provider::DeepL => deepl(cfg.deepl_key.trim(), text, target),
        Provider::OpenAi => openai(cfg, text, target),
    }
}

/// Codice di destinazione per DeepL: alcune lingue vogliono la variante.
fn deepl_target(code: &str) -> String {
    match code {
        "en" => "EN-US".into(),
        "pt" => "PT-BR".into(),
        "zh" => "ZH-HANS".into(),
        c => c.to_ascii_uppercase(),
    }
}

fn deepl(key: &str, text: &str, target: &str) -> Result<Translated, String> {
    if key.is_empty() {
        return Err(tr("manca la chiave DeepL (Impostazioni → Traduzione)").into());
    }
    // le chiavi del piano gratuito finiscono con «:fx» e hanno un altro indirizzo
    let host = if key.ends_with(":fx") { "api-free.deepl.com" } else { "api.deepl.com" };
    let body = serde_json::json!({ "text": [text], "target_lang": deepl_target(target) });
    let mut resp = agent()
        .post(format!("https://{host}/v2/translate"))
        .header("Authorization", format!("DeepL-Auth-Key {key}"))
        .send_json(&body)
        .map_err(|e| crate::trf!("rete: {}", e))?;
    let status = resp.status().as_u16();
    match status {
        200 => {}
        403 => return Err(tr("chiave DeepL non valida").into()),
        456 => return Err(tr("quota mensile di DeepL esaurita").into()),
        429 => return Err(tr("troppe richieste: riprova fra poco").into()),
        s => {
            let msg = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!("DeepL {s}: {}", msg.trim()));
        }
    }
    let v: serde_json::Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;
    let t = &v["translations"][0];
    let text = t["text"].as_str().ok_or_else(|| tr("risposta di DeepL non valida").to_string())?;
    Ok(Translated {
        text: text.to_string(),
        source: t["detected_source_language"].as_str().map(|s| s.to_ascii_lowercase()),
    })
}

fn openai(cfg: &Config, text: &str, target: &str) -> Result<Translated, String> {
    let lang = TARGETS.iter().find(|(c, _)| *c == target).map_or(target, |(_, n)| *n);
    let system = format!(
        "You are a translation engine for a chat app. Detect the language of the user's message \
         and translate it into {lang} ({target}). Keep the tone, emojis, links and formatting; \
         do not add explanations. Reply only with JSON: \
         {{\"source\": \"<ISO 639-1 code of the original>\", \"text\": \"<translation>\"}}"
    );
    let body = serde_json::json!({
        "model": cfg.oa_model.trim(),
        "temperature": 0,
        "response_format": { "type": "json_object" },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": text },
        ],
    });
    let url = format!("{}/chat/completions", cfg.oa_url.trim().trim_end_matches('/'));
    let mut req = agent().post(&url);
    if !cfg.oa_key.trim().is_empty() {
        req = req.header("Authorization", format!("Bearer {}", cfg.oa_key.trim()));
    }
    let mut resp = req.send_json(&body).map_err(|e| crate::trf!("rete: {}", e))?;
    let status = resp.status().as_u16();
    if status != 200 {
        let raw = resp.body_mut().read_to_string().unwrap_or_default();
        let msg = serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
            .unwrap_or(raw);
        return Err(match status {
            401 | 403 => tr("chiave API non valida").to_string(),
            429 => tr("troppe richieste: riprova fra poco").to_string(),
            s => format!("{s}: {}", msg.trim()),
        });
    }
    let v: serde_json::Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;
    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| tr("risposta del servizio non valida").to_string())?;
    Ok(parse_llm(content))
}

/// La risposta del modello: il JSON chiesto o, se ne ha fatto di testa
/// sua, il testo così com'è.
fn parse_llm(content: &str) -> Translated {
    let json = match (content.find('{'), content.rfind('}')) {
        (Some(a), Some(b)) if a < b => serde_json::from_str::<serde_json::Value>(&content[a..=b]).ok(),
        _ => None,
    };
    match json.as_ref().and_then(|v| v["text"].as_str().map(|t| (t, v))) {
        Some((t, v)) => Translated {
            text: t.to_string(),
            source: v["source"].as_str().map(|s| s.trim().to_ascii_lowercase()).filter(|s| s.len() == 2),
        },
        None => Translated { text: content.trim().to_string(), source: None },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risposta_del_modello() {
        let t = parse_llm("```json\n{\"source\": \"EN\", \"text\": \"ciao\"}\n```");
        assert_eq!(t, Translated { text: "ciao".into(), source: Some("en".into()) });
        assert!(t.same_as("EN"));
        let t = parse_llm("ciao a tutti");
        assert_eq!(t, Translated { text: "ciao a tutti".into(), source: None });
    }

    #[test]
    fn codici_deepl() {
        assert_eq!(deepl_target("en"), "EN-US");
        assert_eq!(deepl_target("it"), "IT");
        assert_eq!(deepl_target("zh"), "ZH-HANS");
    }
}
