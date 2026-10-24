//! Colori e stili. La palette è globale e cambia a caldo: quella di RTete
//! (scura sul modello di Telegram Desktop) o quella del tema Omarchy
//! corrente, letta da `~/.local/state/omarchy/current/theme/colors.toml`.

use std::path::PathBuf;
use std::sync::RwLock;
use std::time::SystemTime;

use iced::widget::{button, container};
use iced::{border, color, Background, Border, Color, Theme};

#[derive(Clone, Copy)]
pub struct Pal {
    pub bg: Color,
    pub sidebar: Color,
    pub header: Color,
    pub bubble_in: Color,
    pub bubble_out: Color,
    pub accent: Color,
    pub text: Color,
    pub muted: Color,
    pub link: Color,
    pub danger: Color,
    pub selected: Color,
    pub hover: Color,
    /// colori dei nomi nei gruppi e degli avatar, scelti dal nome
    pub names: [Color; 7],
}

pub const RTETE: Pal = Pal {
    bg: color!(0x0e1621),
    sidebar: color!(0x17212b),
    header: color!(0x17212b),
    bubble_in: color!(0x182533),
    bubble_out: color!(0x2b5278),
    accent: color!(0x5288c1),
    text: color!(0xf5f5f5),
    muted: color!(0x7f91a4),
    link: color!(0x71b7f5),
    danger: color!(0xe5534b),
    selected: color!(0x2b5278),
    hover: color!(0x202b36),
    names: [
        color!(0xe17076),
        color!(0x7bc862),
        color!(0x65aadd),
        color!(0xe5ca77),
        color!(0xee7aae),
        color!(0x6ec9cb),
        color!(0xfaa774),
    ],
};

static PAL: RwLock<Pal> = RwLock::new(RTETE);

/// la palette in uso
pub fn p() -> Pal {
    PAL.read().map(|g| *g).unwrap_or(RTETE)
}

pub fn set(pal: Pal) {
    if let Ok(mut g) = PAL.write() {
        *g = pal;
    }
}

fn omarchy_dir() -> PathBuf {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    state.join("omarchy/current")
}

/// Il nome leggibile del tema Omarchy corrente («Osaka Jade»).
pub fn omarchy_name() -> Option<String> {
    let raw = std::fs::read_to_string(omarchy_dir().join("theme.name")).ok()?;
    let words: Vec<String> = raw
        .trim()
        .split('-')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        })
        .collect();
    Some(words.join(" "))
}

/// Quando è cambiato il tema: il cambio tema riscrive colors.toml.
pub fn omarchy_stamp() -> Option<SystemTime> {
    std::fs::metadata(omarchy_dir().join("theme/colors.toml"))
        .and_then(|m| m.modified())
        .ok()
}

fn hex(v: &str) -> Option<Color> {
    let v = v.trim().trim_matches('"').trim_start_matches('#');
    if v.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(v, 16).ok()?;
    Some(Color::from_rgb8((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color::from_rgb(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
    )
}

/// La palette dal colors.toml del tema Omarchy (righe `chiave = "#rrggbb"`).
pub fn omarchy() -> Option<Pal> {
    let raw = std::fs::read_to_string(omarchy_dir().join("theme/colors.toml")).ok()?;
    let get = |key: &str| {
        raw.lines()
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == key)
            .and_then(|(_, v)| hex(v))
    };
    let bg = get("background")?;
    let text = get("foreground")?;
    let accent = get("accent").or_else(|| get("blue"))?;
    let dark_bg = get("dark_background").unwrap_or(bg);
    let lighter = get("lighter_background").unwrap_or_else(|| mix(bg, text, 0.08));
    let selection = get("selection").unwrap_or_else(|| mix(bg, accent, 0.35));
    let or = |key: &str, i: usize| get(key).unwrap_or(RTETE.names[i]);
    Some(Pal {
        bg,
        sidebar: dark_bg,
        header: dark_bg,
        bubble_in: lighter,
        bubble_out: selection,
        accent,
        text,
        // il «muted» dei temi è pensato per i bordi: per il testo secondario
        // serve più contrasto
        muted: mix(text, bg, 0.45),
        link: get("cyan").unwrap_or(accent),
        danger: get("red").unwrap_or(RTETE.danger),
        selected: selection,
        hover: lighter,
        names: [
            or("red", 0),
            or("green", 1),
            or("blue", 2),
            or("bright_yellow", 3),
            or("magenta", 4),
            or("cyan", 5),
            or("orange", 6),
        ],
    })
}

pub fn name_color(seed: &str) -> Color {
    let h = seed.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    let names = p().names;
    names[h as usize % names.len()]
}

pub fn theme() -> Theme {
    let p = p();
    Theme::custom(
        "RTete",
        iced::theme::Palette {
            background: p.bg,
            text: p.text,
            primary: p.accent,
            success: p.names[1],
            warning: p.names[3],
            danger: p.danger,
        },
    )
}

pub fn fill(c: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(c)),
        text_color: Some(p().text),
        ..Default::default()
    }
}

pub fn bubble(outgoing: bool, selected: bool) -> impl Fn(&Theme) -> container::Style {
    move |_| {
        let p = p();
        container::Style {
            background: Some(Background::Color(if outgoing { p.bubble_out } else { p.bubble_in })),
            text_color: Some(p.text),
            border: Border {
                radius: border::radius(12),
                width: if selected { 2.0 } else { 0.0 },
                color: p.link,
            },
            ..Default::default()
        }
    }
}

pub fn pill(c: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(c)),
        text_color: Some(p().text),
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
    move |_, status| {
        let p = p();
        button::Style {
            background: Some(Background::Color(match (open, status) {
                (true, _) => p.selected,
                (false, button::Status::Hovered | button::Status::Pressed) => p.hover,
                _ => p.sidebar,
            })),
            text_color: p.text,
            ..Default::default()
        }
    }
}

/// pulsante piatto (azioni, sticker, reazioni)
pub fn flat(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let p = p();
        button::Style {
            background: match (active, status) {
                (true, _) => Some(Background::Color(p.selected)),
                (false, button::Status::Hovered | button::Status::Pressed) => {
                    Some(Background::Color(p.hover))
                }
                _ => None,
            },
            text_color: p.text,
            border: border::rounded(8),
            ..Default::default()
        }
    }
}

pub fn primary(_: &Theme, status: button::Status) -> button::Style {
    let p = p();
    button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered | button::Status::Pressed => p.link,
            _ => p.accent,
        })),
        text_color: Color::WHITE,
        border: border::rounded(8),
        ..Default::default()
    }
}

pub fn danger(_: &Theme, _: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(p().danger)),
        text_color: Color::WHITE,
        border: border::rounded(8),
        ..Default::default()
    }
}
