//! Colori e stili: una palette scura sul modello di Telegram Desktop.

use iced::widget::{button, container};
use iced::{border, color, Background, Border, Color, Theme};

pub const BG: Color = color!(0x0e1621);
pub const SIDEBAR: Color = color!(0x17212b);
pub const HEADER: Color = color!(0x17212b);
pub const BUBBLE_IN: Color = color!(0x182533);
pub const BUBBLE_OUT: Color = color!(0x2b5278);
pub const ACCENT: Color = color!(0x5288c1);
pub const TEXT: Color = color!(0xf5f5f5);
pub const MUTED: Color = color!(0x7f91a4);
pub const LINK: Color = color!(0x71b7f5);
pub const DANGER: Color = color!(0xe5534b);
pub const SELECTED: Color = color!(0x2b5278);
pub const HOVER: Color = color!(0x202b36);

/// colori dei nomi nei gruppi e degli avatar, scelti dall'id
const NAME_COLORS: [Color; 7] = [
    color!(0xe17076),
    color!(0x7bc862),
    color!(0x65aadd),
    color!(0xe5ca77),
    color!(0xee7aae),
    color!(0x6ec9cb),
    color!(0xfaa774),
];

pub fn name_color(seed: &str) -> Color {
    let h = seed.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    NAME_COLORS[h as usize % NAME_COLORS.len()]
}

pub fn theme() -> Theme {
    Theme::custom(
        "RTete",
        iced::theme::Palette {
            background: BG,
            text: TEXT,
            primary: ACCENT,
            success: color!(0x4fae4e),
            warning: color!(0xe5ca77),
            danger: DANGER,
        },
    )
}

pub fn fill(c: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(c)),
        text_color: Some(TEXT),
        ..Default::default()
    }
}

pub fn bubble(outgoing: bool, selected: bool) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(if outgoing { BUBBLE_OUT } else { BUBBLE_IN })),
        text_color: Some(TEXT),
        border: Border {
            radius: border::radius(12),
            width: if selected { 2.0 } else { 0.0 },
            color: LINK,
        },
        ..Default::default()
    }
}

pub fn pill(c: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(c)),
        text_color: Some(TEXT),
        border: border::rounded(10),
        ..Default::default()
    }
}

pub fn avatar(c: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(c)),
        text_color: Some(Color::WHITE),
        border: border::rounded(100),
        ..Default::default()
    }
}

/// voce della lista chat: evidenziata se aperta, chiara al passaggio
pub fn chat_item(open: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| button::Style {
        background: Some(Background::Color(match (open, status) {
            (true, _) => SELECTED,
            (false, button::Status::Hovered | button::Status::Pressed) => HOVER,
            _ => SIDEBAR,
        })),
        text_color: TEXT,
        ..Default::default()
    }
}

/// pulsante piatto (azioni, sticker, reazioni)
pub fn flat(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| button::Style {
        background: match (active, status) {
            (true, _) => Some(Background::Color(SELECTED)),
            (false, button::Status::Hovered | button::Status::Pressed) => {
                Some(Background::Color(HOVER))
            }
            _ => None,
        },
        text_color: TEXT,
        border: border::rounded(8),
        ..Default::default()
    }
}

pub fn primary(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered | button::Status::Pressed => LINK,
            _ => ACCENT,
        })),
        text_color: Color::WHITE,
        border: border::rounded(8),
        ..Default::default()
    }
}

pub fn danger(_: &Theme, _: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(DANGER)),
        text_color: Color::WHITE,
        border: border::rounded(8),
        ..Default::default()
    }
}
