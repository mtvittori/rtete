use iced::widget::svg;
use std::sync::OnceLock;

/// Altezza del viewBox comune ai due segni: così `size` coincide con l'altezza reale.
const VIEW_HEIGHT: f32 = 12.0;

/// Larghezza del viewBox del segno singolo ("inviato"): quadrato.
const SENT_VIEW_WIDTH: f32 = 12.0;

/// Larghezza del viewBox dei due segni sovrapposti ("letto").
const READ_VIEW_WIDTH: f32 = 16.0;

/// Rapporto larghezza/altezza del segno singolo, per la larghezza in pixel.
const SENT_ASPECT: f32 = SENT_VIEW_WIDTH / VIEW_HEIGHT;

/// Rapporto larghezza/altezza dei due segni, più larghi del singolo.
const READ_ASPECT: f32 = READ_VIEW_WIDTH / VIEW_HEIGHT;

/// Spessore del tratto nel viewBox: a 11-12 px resta leggibile.
const STROKE_WIDTH: f32 = 1.6;

/// Percorso del segno singolo: un solo tratto a spunta.
const SENT_PATHS: &str = r#"<path d="M1.5 6.5 L4.5 9.5 L10.5 2.5"/>"#;

/// Percorso dei due segni sovrapposti: il secondo è spostato a destra di 4 unità.
const READ_PATHS: &str =
    r#"<path d="M1.5 6.5 L4.5 9.5 L10.5 2.5"/><path d="M5.5 6.5 L8.5 9.5 L14.5 2.5"/>"#;

/// I due documenti SVG dei segni, costruiti una volta sola.
struct Handles {
    sent: svg::Handle,
    read: svg::Handle,
}

/// Restituisce i documenti di "inviato" e "letto", creati alla prima chiamata.
/// Input: nessuno. Output: riferimento statico ai due handle.
/// Effetti: inizializza la `OnceLock` una sola volta.
fn handles() -> &'static Handles {
    static HANDLES: OnceLock<Handles> = OnceLock::new();
    HANDLES.get_or_init(|| Handles {
        sent: document(SENT_VIEW_WIDTH, SENT_PATHS),
        read: document(READ_VIEW_WIDTH, READ_PATHS),
    })
}

/// Avvolge i percorsi in un documento SVG con il viewBox dato (altezza `VIEW_HEIGHT`).
/// Input: larghezza del viewBox e percorsi. Output: handle del documento.
/// Effetti: nessuno.
fn document(view_width: f32, paths: &str) -> svg::Handle {
    let xml = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {view_width} {VIEW_HEIGHT}"><g fill="none" stroke="#000" stroke-width="{STROKE_WIDTH}" stroke-linecap="round" stroke-linejoin="round">{paths}</g></svg>"##
    );
    svg::Handle::from_memory(xml.into_bytes())
}

/// Il segno di ricevuta vettoriale: un segno se `read` è falso ("inviato"),
/// due segni sovrapposti se è vero ("letto").
/// Input: stato di lettura, colore di tinta, altezza in pixel.
/// Output: elemento SVG alto `size` e largo `size` per il rapporto del viewBox.
/// Effetti: nessuno; la dimensione non segue la scala del testo.
pub fn ticks<'a, Message: 'a>(
    read: bool,
    color: iced::Color,
    size: f32,
) -> iced::Element<'a, Message> {
    let (handle, aspect) = if read {
        (&handles().read, READ_ASPECT)
    } else {
        (&handles().sent, SENT_ASPECT)
    };
    svg(handle.clone())
        .width(size * aspect)
        .height(size)
        .style(move |_theme, _status| svg::Style { color: Some(color) })
        .into()
}
