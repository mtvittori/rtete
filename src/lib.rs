//! Il cuore di RTete condiviso fra la TUI (`rtete`) e la GUI iced
//! (`rtete-gui`): rete Telegram, worker, cache delle anteprime e stato.

pub mod app;
pub mod clipboard;
pub mod loader;
pub mod send;
pub mod sync;
pub mod tg;
pub mod worker;
