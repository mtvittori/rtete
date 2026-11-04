//! Sezione FONT di Impostazioni → Aspetto: menu del font, scelta dello spessore
//! e dimensione del testo. Legge le impostazioni ed emette i messaggi; l'effetto
//! su font e scala sta in `Gui::update`.

use iced::widget::{column, pick_list, row, slider, Row};
use iced::{Center, Element};
use rondine::{tr, trf};

use crate::fonts::{self, FontPick};
use crate::style;
use crate::ui::{choice, section, text};
use crate::{Gui, Msg};

/// Estremi e passo dello slider del testo: gli stessi di `settings.rs`, privati lì.
const TEXT_SCALE_MIN: f32 = 0.70;
const TEXT_SCALE_MAX: f32 = 1.60;
const TEXT_SCALE_STEP: f32 = 0.05;
/// Fattore per mostrare la scala come percentuale.
const PERCENT: f32 = 100.0;
/// Larghezza dell'etichetta a sinistra dello slider, in pixel al 100%.
const LABEL_WIDTH_PX: f32 = 170.0;
/// Spaziature della sezione: fra le righe principali, fra riga e slider, fra le scelte.
const SECTION_SPACING: f32 = 14.0;
const ROW_SPACING: f32 = 12.0;
const CHOICE_SPACING: f32 = 8.0;

/// Vista della sezione FONT, da inserire in Impostazioni → Aspetto.
/// Input: `g`, con le impostazioni correnti e l'elenco dei font se già caricato.
/// Output: la colonna della sezione. Effetti: nessuno.
pub fn view(g: &Gui) -> Element<'_, Msg> {
    let list = fonts::list();
    let picked = list.and_then(|list| selected(list, g.settings.font.as_deref()));
    let mut page = column![section(tr("FONT")), font_menu(list, picked)].spacing(SECTION_SPACING);
    if let Some(weights) = picked.and_then(family_weights) {
        page = page.push(weight_choices(g, weights));
    }
    page.push(text_size_row(g)).into()
}

/// Il menu dei font, o un testo muto finché l'elenco non è caricato.
/// Input: l'elenco (`None` mentre carica) e la voce selezionata. Output: il menu o il testo.
/// Effetti: nessuno.
fn font_menu<'a>(list: Option<&'static [FontPick]>, picked: Option<&'static FontPick>) -> Element<'a, Msg> {
    let Some(list) = list else {
        return text(tr("carico l'elenco dei font…")).color(style::p().muted).into();
    };
    pick_list(list, picked, Msg::Font).text_size(style::fs(style::BODY_PX)).into()
}

/// La voce da mostrare nel menu: quella salvata se presente nell'elenco, altrimenti il default.
/// Input: l'elenco e il nome del font salvato (`None` per il default). Output: la voce, `None`
/// se l'elenco non ha il default. Effetti: nessuno.
fn selected<'a>(list: &'a [FontPick], saved: Option<&str>) -> Option<&'a FontPick> {
    saved
        .and_then(|wanted| {
            list.iter()
                .find(|pick| matches!(pick, FontPick::Family { name, .. } if name.as_str() == wanted))
        })
        .or_else(|| list.iter().find(|pick| matches!(pick, FontPick::Default)))
}

/// I pesi disponibili della famiglia, `None` per il default o per una famiglia senza pesi.
/// Input: la voce scelta. Output: i pesi. Effetti: nessuno.
fn family_weights(pick: &FontPick) -> Option<&[u16]> {
    match pick {
        FontPick::Family { weights, .. } if !weights.is_empty() => Some(weights.as_slice()),
        _ => None,
    }
}

/// Le scelte di spessore della famiglia, con quello realmente in uso evidenziato.
/// Input: `g` (per il peso salvato) e i pesi disponibili. Output: etichetta e scelte. Effetti: nessuno.
fn weight_choices<'a>(g: &Gui, weights: &[u16]) -> Element<'a, Msg> {
    let in_use = fonts::normal_weight(weights, g.settings.font_weight);
    let choices = weights.iter().map(|&weight| {
        choice(weight_label(weight), Some(weight) == in_use, Msg::FontWeight(weight))
    });
    column![text(tr("Spessore")), Row::with_children(choices).spacing(CHOICE_SPACING).wrap()]
        .spacing(CHOICE_SPACING)
        .into()
}

/// Il nome del peso CSS (100-900) o, per gli altri valori, il numero stesso.
/// Input: il peso. Output: l'etichetta. Effetti: nessuno.
fn weight_label(weight: u16) -> String {
    match weight {
        100 => tr("Sottilissimo").to_string(),
        200 => tr("Extra leggero").to_string(),
        300 => tr("Leggero").to_string(),
        400 => tr("Normale").to_string(),
        500 => tr("Medio").to_string(),
        600 => tr("Semigrassetto").to_string(),
        700 => tr("Grassetto").to_string(),
        800 => tr("Extra grassetto").to_string(),
        900 => tr("Nero").to_string(),
        _ => weight.to_string(),
    }
}

/// La riga dello slider della dimensione del testo, sempre visibile.
/// Input: `g`, con la scala salvata. Output: la riga. Effetti: nessuno.
fn text_size_row<'a>(g: &Gui) -> Element<'a, Msg> {
    let scale = g.settings.text_scale;
    row![
        text(trf!("Testo  {}%", format!("{:.0}", scale * PERCENT))).width(style::fs(LABEL_WIDTH_PX)),
        slider(TEXT_SCALE_MIN..=TEXT_SCALE_MAX, scale, Msg::TextScale).step(TEXT_SCALE_STEP),
    ]
    .spacing(ROW_SPACING)
    .align_y(Center)
    .into()
}

impl std::fmt::Display for FontPick {
    /// Il nome da mostrare nel menu: «Predefinito» per il font di sistema, altrimenti la famiglia.
    /// Input: il formatter. Output: l'esito della scrittura. Effetti: nessuno.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FontPick::Default => f.write_str(tr("Predefinito")),
            FontPick::Family { name, .. } => f.write_str(name),
        }
    }
}
