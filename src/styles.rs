//! Palette chiara in stile jellyfin-tui: carta crema, inchiostro scuro e
//! Ambra (arancio fuoco) come colore d'accento dei pannelli attivi.

pub mod palette {
    pub const BG: ratatui::style::Color = ratatui::style::Color::Rgb(252, 247, 235);
    pub const FG: ratatui::style::Color = ratatui::style::Color::Rgb(58, 51, 42);
    pub const FAINT: ratatui::style::Color = ratatui::style::Color::Rgb(158, 150, 133);
    pub const ACCENT: ratatui::style::Color = ratatui::style::Color::Rgb(226, 92, 42);
    pub const ACCENT_DIM: ratatui::style::Color = ratatui::style::Color::Rgb(208, 140, 98);
    pub const SELECT: ratatui::style::Color = ratatui::style::Color::Rgb(46, 40, 32);
    /// testo sopra la barra di selezione scura
    pub const ON_SELECT: ratatui::style::Color = ratatui::style::Color::Rgb(252, 247, 235);
    pub const OUTGOING: ratatui::style::Color = ACCENT;
    pub const INCOMING: ratatui::style::Color = FAINT;
    pub const WARNING: ratatui::style::Color = ratatui::style::Color::Rgb(186, 60, 50);
}

pub use palette::*;
