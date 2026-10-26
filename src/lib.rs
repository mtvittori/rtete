//! Il cuore di Rondine condiviso fra la TUI (`rondine`) e la GUI iced
//! (`rondine-gui`): stato, worker, cache delle anteprime e i backend di
//! rete. L'app parla coi backend solo coi tipi di `model` e il protocollo
//! di `send`; tutto quello che è di un servizio sta in `backend::<nome>`.

pub mod app;
pub mod backend;
pub mod clipboard;
pub mod i18n;
pub mod links;
pub mod loader;
pub mod model;
pub mod paths;
pub mod send;
pub mod sync;
pub mod translate;
pub mod worker;

/// Il backend Telegram (grammers), col nome breve di sempre.
pub use backend::tg;

/// Variabile d'ambiente `RONDINE_<key>`; vale ancora il vecchio nome
/// `RTETE_<key>` di quando l'app si chiamava RTete.
pub fn env_var(key: &str) -> Option<String> {
    std::env::var(format!("RONDINE_{key}"))
        .or_else(|_| std::env::var(format!("RTETE_{key}")))
        .ok()
}

pub use i18n::tr;
