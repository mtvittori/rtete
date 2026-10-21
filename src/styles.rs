//! Colori del tema del terminale: sfondo e testo predefiniti più la palette
//! ANSI a 16 colori, così rtete segue il tema Omarchy (foot/kitty) senza RGB
//! fissi. La barra di selezione si ottiene invertendo i colori (REVERSED).

pub mod palette {
    use ratatui::style::Color;

    pub const BG: Color = Color::Reset;
    pub const FG: Color = Color::Reset;
    pub const FAINT: Color = Color::DarkGray;
    pub const ACCENT: Color = Color::LightRed;
    pub const ACCENT_DIM: Color = Color::Red;
    pub const OUTGOING: Color = ACCENT;
    pub const INCOMING: Color = FAINT;
    pub const WARNING: Color = Color::Red;
}

pub use palette::*;
