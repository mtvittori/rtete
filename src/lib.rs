//! Il cuore di Rondine condiviso fra la TUI (`rondine`) e la GUI iced
//! (`rondine-gui`): rete Telegram, worker, cache delle anteprime e stato.

pub mod app;
pub mod clipboard;
pub mod loader;
pub mod send;
pub mod sync;
pub mod tg;
pub mod worker;

/// Variabile d'ambiente `RONDINE_<key>`; vale ancora il vecchio nome
/// `RTETE_<key>` di quando l'app si chiamava RTete.
pub fn env_var(key: &str) -> Option<String> {
    std::env::var(format!("RONDINE_{key}"))
        .or_else(|_| std::env::var(format!("RTETE_{key}")))
        .ok()
}
