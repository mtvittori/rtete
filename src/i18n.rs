//! Lingue dell'interfaccia. Il testo nel codice è l'italiano e fa da
//! chiave: `tr("carico…")` lo restituisce com'è in italiano, o la
//! traduzione da `locales/<lingua>.txt`. Per i testi con valori dentro
//! c'è `trf!("inoltro a {}…", nome)`: i `{}` si riempiono in ordine.
//!
//! I file delle lingue sono blocchi di due righe (italiano, traduzione)
//! separati da una riga vuota; `#` apre un commento, `\n` è un a capo.
//! Un test controlla che ogni testo del codice abbia la sua traduzione.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    It,
    En,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::It, Lang::En];

    /// il nome della lingua scritto nella lingua stessa
    pub fn name(self) -> &'static str {
        match self {
            Lang::It => "Italiano",
            Lang::En => "English",
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::It => "it",
            Lang::En => "en",
        }
    }

    pub fn from_code(code: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|l| code.starts_with(l.code()))
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn set(lang: Lang) {
    LANG.store(lang as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::En,
        _ => Lang::It,
    }
}

/// La lingua del sistema (LC_ALL, LC_MESSAGES, LANG): italiano solo se è
/// quella, altrimenti inglese.
pub fn detect() -> Lang {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .and_then(|v| Lang::from_code(&v.to_ascii_lowercase()))
        .unwrap_or(Lang::En)
}

const EN: &str = include_str!("../locales/en.txt");

/// Legge un file di lingua: coppie (italiano, traduzione).
pub fn parse(raw: &str) -> Vec<(String, String)> {
    let unescape = |s: &str| s.replace("\\n", "\n");
    raw.split("\n\n")
        .filter_map(|block| {
            let mut lines = block.lines().filter(|l| !l.starts_with('#') && !l.is_empty());
            Some((unescape(lines.next()?), unescape(lines.next()?)))
        })
        .collect()
}

fn table(lang: Lang) -> Option<&'static HashMap<String, String>> {
    static EN_MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    match lang {
        Lang::It => None,
        Lang::En => Some(EN_MAP.get_or_init(|| parse(EN).into_iter().collect())),
    }
}

/// Il testo nella lingua scelta (l'italiano se manca la traduzione).
pub fn tr(it: &'static str) -> &'static str {
    match table(lang()).and_then(|t| t.get(it)) {
        Some(s) => s.as_str(),
        None => it,
    }
}

/// Riempie i `{}` del modello, in ordine.
pub fn fill(template: &str, args: &[&dyn std::fmt::Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut args = args.iter();
    let mut rest = template;
    while let Some(i) = rest.find("{}") {
        out.push_str(&rest[..i]);
        if let Some(a) = args.next() {
            out.push_str(&a.to_string());
        }
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

/// `trf!("inoltro a {}…", nome)`: tradotto, poi con i valori al posto dei `{}`.
#[macro_export]
macro_rules! trf {
    ($tpl:literal $(, $arg:expr)* $(,)?) => {
        $crate::i18n::fill($crate::i18n::tr($tpl), &[$(&$arg as &dyn std::fmt::Display),*])
    };
}

/// Giorni della settimana abbreviati, da lunedì.
pub fn weekdays() -> [&'static str; 7] {
    match lang() {
        Lang::It => ["lun", "mar", "mer", "gio", "ven", "sab", "dom"],
        Lang::En => ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
    }
}

/// Nomi dei mesi.
pub fn months() -> [&'static str; 12] {
    match lang() {
        Lang::It => [
            "gennaio", "febbraio", "marzo", "aprile", "maggio", "giugno", "luglio", "agosto",
            "settembre", "ottobre", "novembre", "dicembre",
        ],
        Lang::En => [
            "January", "February", "March", "April", "May", "June", "July", "August",
            "September", "October", "November", "December",
        ],
    }
}

/// Separatore decimale: virgola in italiano, punto in inglese.
pub fn decimal_sep() -> char {
    match lang() {
        Lang::It => ',',
        Lang::En => '.',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// I testi passati a `tr(` e `trf!(` in un sorgente.
    fn keys_in(src: &str) -> Vec<String> {
        let mut out = Vec::new();
        for pat in ["tr(", "trf!("] {
            let mut rest = src;
            while let Some(i) = rest.find(pat) {
                // `str(`, `attr(`…: solo la parola intera
                let before = rest[..i].chars().next_back();
                rest = &rest[i + pat.len()..];
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                // il testo può andare a capo dopo la parentesi
                let Some(lit) = rest.trim_start().strip_prefix('"') else {
                    continue;
                };
                rest = lit;
                let mut key = String::new();
                let mut chars = rest.chars();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('n') => key.push('\n'),
                            Some(o) => key.push(o),
                            None => {}
                        },
                        c => key.push(c),
                    }
                }
                out.push(key);
            }
        }
        out
    }

    fn sources(dir: &std::path::Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                sources(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("i18n.rs") {
                out.push(std::fs::read_to_string(p).unwrap());
            }
        }
    }

    #[test]
    fn ogni_testo_ha_la_traduzione_inglese() {
        let mut srcs = Vec::new();
        sources(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")), &mut srcs);
        let en: HashMap<String, String> = parse(EN).into_iter().collect();
        let mut used = std::collections::HashSet::new();
        let mut missing = Vec::new();
        for key in srcs.iter().flat_map(|s| keys_in(s)) {
            match en.get(&key) {
                Some(t) => {
                    assert_eq!(
                        key.matches("{}").count(),
                        t.matches("{}").count(),
                        "segnaposto diversi in «{key}»"
                    );
                }
                None => missing.push(key.clone()),
            }
            used.insert(key);
        }
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "senza traduzione:\n{}", missing.join("\n"));
        let mut unused: Vec<_> = en.keys().filter(|k| !used.contains(*k)).collect();
        unused.sort();
        assert!(unused.is_empty(), "traduzioni non usate:\n{unused:?}");
    }

    #[test]
    fn fill_riempie_in_ordine() {
        assert_eq!(fill("a {} b {}", &[&1, &"x"]), "a 1 b x");
        assert_eq!(fill("nessuno", &[]), "nessuno");
    }
}
