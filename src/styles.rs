//! Shared TUI palette.

pub mod palette {
    pub const BG: ratatui::style::Color = ratatui::style::Color::Rgb(24, 24, 32);
    pub const FG: ratatui::style::Color = ratatui::style::Color::Rgb(220, 220, 228);
    pub const ACCENT: ratatui::style::Color = ratatui::style::Color::Rgb(120, 220, 180);
    pub const ACCENT_DIM: ratatui::style::Color = ratatui::style::Color::Rgb(70, 130, 110);
    pub const PANEL: ratatui::style::Color = ratatui::style::Color::Rgb(30, 30, 40);
    pub const PANEL_HI: ratatui::style::Color = ratatui::style::Color::Rgb(40, 42, 54);
    pub const OUTGOING: ratatui::style::Color = ratatui::style::Color::Rgb(132, 156, 222);
    pub const INCOMING: ratatui::style::Color = ratatui::style::Color::Rgb(96, 98, 110);
    pub const WARNING: ratatui::style::Color = ratatui::style::Color::Rgb(250, 160, 90);
}

pub use palette::*;
