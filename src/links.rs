//! I link di Telegram (`tg://…`, `https://t.me/…`) che si aprono dentro
//! Rondine: solo il riconoscimento del testo, la risoluzione la fa il
//! backend Telegram.

use crate::model::ChatId;

/// Un link Telegram (`tg://…` o `https://t.me/…`) che si apre dentro Rondine.
#[derive(Clone, Debug, PartialEq)]
pub enum TgLink {
    /// chat pubblica per username, eventualmente su un messaggio
    Public { name: String, post: Option<i32> },
    /// canale/supergruppo privato per id (t.me/c/ID/POST)
    Private { channel: i64, post: Option<i32> },
    /// invito a una chat privata (t.me/+HASH, joinchat, tg://join)
    Invite(String),
}

/// La chat di un link privato (`t.me/c/ID`): id del canale nel formato
/// Bot API, come i `ChatId` del backend Telegram.
pub fn channel_chat(channel: i64) -> Option<ChatId> {
    // canali e supergruppi, poi i monoforum (come `PeerId::channel`)
    let ok = (1..=997852516352).contains(&channel) || (1002147483649..=2999999999999).contains(&channel);
    ok.then(|| ChatId(-(1000000000000 + channel)))
}

/// Riconosce i link di Telegram; `None` per tutto il resto (va al browser).
pub fn parse_link(url: &str) -> Option<TgLink> {
    let url = url.trim();
    let query = |q: &str, key: &str| {
        q.split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
    };
    if let Some(rest) = url.strip_prefix("tg://") {
        let (what, q) = rest.split_once('?').unwrap_or((rest, ""));
        return match what.trim_end_matches('/') {
            "resolve" => Some(TgLink::Public {
                name: query(q, "domain")?,
                post: query(q, "post").and_then(|p| p.parse().ok()),
            }),
            "join" => Some(TgLink::Invite(query(q, "invite")?)),
            "privatepost" => Some(TgLink::Private {
                channel: query(q, "channel")?.parse().ok()?,
                post: query(q, "post").and_then(|p| p.parse().ok()),
            }),
            _ => None,
        };
    }
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let path = rest
        .strip_prefix("t.me/")
        .or_else(|| rest.strip_prefix("telegram.me/"))
        .or_else(|| rest.strip_prefix("www.t.me/"))?;
    let path = path.split(['?', '#']).next().unwrap_or("");
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    let first = parts.next()?;
    let second = parts.next();
    if let Some(hash) = first.strip_prefix('+') {
        return Some(TgLink::Invite(hash.to_string()));
    }
    match first {
        "joinchat" => Some(TgLink::Invite(second?.to_string())),
        "c" => Some(TgLink::Private {
            channel: second?.parse().ok()?,
            post: parts.next().and_then(|p| p.parse().ok()),
        }),
        "s" => Some(TgLink::Public { name: second?.to_string(), post: None }),
        // pagine di servizio, non chat
        "addstickers" | "addemoji" | "share" | "proxy" | "socks" | "login" | "setlanguage"
        | "addtheme" | "bg" | "invoice" | "boost" => None,
        name => Some(TgLink::Public {
            name: name.to_string(),
            post: second.and_then(|p| p.parse().ok()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- parse_link ----------
    #[test]
    fn parse_link_riconosce_i_formati() {
        let public = |n: &str, p: Option<i32>| Some(TgLink::Public { name: n.into(), post: p });
        assert_eq!(parse_link("https://t.me/durov"), public("durov", None));
        assert_eq!(parse_link("t.me/durov/42"), public("durov", Some(42)));
        assert_eq!(parse_link("https://t.me/s/canale"), public("canale", None));
        assert_eq!(parse_link("tg://resolve?domain=durov&post=7"), public("durov", Some(7)));
        assert_eq!(parse_link("https://t.me/+AbC_1"), Some(TgLink::Invite("AbC_1".into())));
        assert_eq!(parse_link("https://t.me/joinchat/XyZ"), Some(TgLink::Invite("XyZ".into())));
        assert_eq!(parse_link("tg://join?invite=XyZ"), Some(TgLink::Invite("XyZ".into())));
        assert_eq!(
            parse_link("https://t.me/c/123456/89"),
            Some(TgLink::Private { channel: 123456, post: Some(89) })
        );
        assert_eq!(parse_link("https://t.me/addstickers/set"), None);
        assert_eq!(parse_link("https://example.com/t.me/x"), None);
        assert_eq!(parse_link("tg://settings"), None);
    }

    #[test]
    fn channel_chat_come_bot_api() {
        assert_eq!(channel_chat(1234567890), Some(ChatId(-1001234567890)));
        assert!(channel_chat(1234567890).unwrap().own_msg_ids());
        assert_eq!(channel_chat(0), None);
    }
}
