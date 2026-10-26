//! Vista TUI in stile jellyfin-tui: pannelli arrotondati su carta crema,
//! titolo incastonato nella cornice, conteggio sul lato destro della cornice,
//! barra di selezione scura con `>`, cornice ambra sul pannello attivo e
//! suggerimenti dei tasti nella cornice in basso. Le posizioni degli sticker
//! vengono proiettate in `Cmd::Place` che il ciclo principale esegue con il
//! kitty graphics protocol dopo ogni frame ratatui.

use std::collections::{HashMap, VecDeque};

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;

use crate::gfx;
use crate::styles::*;
use crate::tg;
use rondine::{tr, trf};
use crate::app::{App, STICKER_COLS};

/// Comando grafico da eseguire dopo il frame ratatui.
pub enum Cmd {
    Place {
        rect: ratatui::layout::Rect,
        path: std::path::PathBuf,
    },
}

/// Altezza del riquadro «in evidenza» sotto la griglia sticker.
const CARD_H: u16 = 10;

/// RTETE_SCALE (0.5–4): moltiplicatore della grandezza delle immagini
/// (sticker nella griglia e nel messaggio, foto, gif). 1.0 di default.
fn scale() -> f32 {
    static S: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *S.get_or_init(|| {
        rondine::env_var("SCALE")
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| (0.5..=4.0).contains(v))
            .unwrap_or(1.0)
    })
}

/// righe dedicate all'immagine nel messaggio: sticker (false) o foto/gif (true)
pub fn band_rows(photo: bool) -> u16 {
    let base: f32 = if photo { 8.0 } else { 5.0 };
    ((base * scale()).round() as u16).clamp(2, 18)
}

/// altezza (in righe) di una cella della griglia sticker
fn cell_h() -> u16 {
    ((6.0 * scale()).round() as u16).clamp(4, 16)
}

/// Una riga di cronologia già proiettata per il rendering.
pub struct HistoryLine {
    pub text: String,
    pub color: Color,
    pub sticker: bool,
    pub animated: bool,
    pub sticker_media: Option<grammers_client::media::Media>,
    /// quante righe occupa nel terminale (più di 1 solo per le immagini).
    pub rows: u16,
    /// larghezza immagine in % della larghezza del pannello.
    pub frac: u16,
    /// id del messaggio di origine (0 = riga di servizio/continuazione
    /// priva di selezione).
    pub id: i32,
    /// link cliccabili sulla riga: (colonna inizio, colonna fine, url).
    pub links: Vec<(u16, u16, String)>,
}

impl HistoryLine {
    pub fn sep() -> Self {
        Self {
            text: " ".to_string(),
            color: FG,
            sticker: false,
            animated: false,
            sticker_media: None,
            rows: 1,
            frac: 0,
            id: 0,
            links: Vec::new(),
        }
    }
}

/// larghezza della colonna contatti in base alla larghezza dello schermo
/// (condivisa dal disegno e dallo routing della rotella).
pub fn side_w(width: u16) -> u16 {
    if width > 90 {
        28
    } else if width > 60 {
        22
    } else {
        16
    }
}

/// Larghezza del nome autore nelle righe di storia.
pub const NAME_W: usize = 8;

/// Larghezza della colonna prima del separatore: "HH:MM " + nome.
pub const LABEL_W: usize = NAME_W + 6;

/// "HH:MM" nel fuso locale; `date` 0 (riga locale senza orario) → vuoto.
pub fn fmt_hhmm(date: i64, tz: &jiff::tz::TimeZone) -> String {
    if date == 0 {
        return "     ".into();
    }
    jiff::Timestamp::from_second(date)
        .map(|t| t.to_zoned(tz.clone()).strftime("%H:%M").to_string())
        .unwrap_or_else(|_| "     ".into())
}

/// Colonna autore: orario, nome troncato su 8 caratteri + separatore, es.
/// "14:32 Pippo    │ " — le righe successive ripetono la colonna vuota.
pub fn msg_label(time: &str, name: &str) -> String {
    let trunc: String = name.chars().take(NAME_W).collect();
    format!("{time} {trunc:<NAME_W$} │ ")
}

/// Marcatore in testa al testo: inoltro. La risposta ha una riga sua sopra
/// il messaggio (`reply_line`).
fn row_lead(row: &tg::HistoryRow) -> String {
    let mut out = String::new();
    match &row.fwd {
        Some(f) if f.is_empty() => out.push_str("⤳ "),
        Some(f) => out.push_str(&format!("⤳ {f}: ")),
        None => {}
    }
    out
}

/// Nome dell'autore di una riga: il proprio per i messaggi in uscita.
fn author<'a>(row: &'a tg::HistoryRow, me: &'a str) -> &'a str {
    if row.outgoing {
        if me.is_empty() {
            "Io"
        } else {
            me
        }
    } else if row.from.is_empty() {
        "Lui"
    } else {
        row.from.as_str()
    }
}

/// Contenuto di una riga in breve, su una riga sola, per la citazione.
fn row_summary(row: &tg::HistoryRow) -> String {
    let text = match &row.kind {
        tg::RowKind::Text(t) => t.clone(),
        tg::RowKind::Sticker { emoji, .. } => format!("sticker {emoji}"),
        tg::RowKind::Photo { caption, gif, .. } => {
            let what = if *gif { "🎞 gif" } else { tr("📷 foto") };
            if caption.is_empty() {
                what.to_string()
            } else {
                format!("{what} {caption}")
            }
        }
        tg::RowKind::Doc { name, .. } => format!("📄 {name}"),
        tg::RowKind::Audio { name, voice, .. } => {
            if *voice {
                tr("🎙 vocale").to_string()
            } else {
                format!("🎧 {name}")
            }
        }
        tg::RowKind::Note(t) | tg::RowKind::Service(t) | tg::RowKind::Video { note: t, .. } => {
            t.clone()
        }
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Righe di citazione massime sopra una risposta: oltre, "…" in fondo.
const QUOTE_ROWS: usize = 3;

/// Righe di citazione sopra una risposta, a capo entro `width` caratteri e al
/// massimo `QUOTE_ROWS`: "↱ Autore: messaggio citato", le righe seguenti
/// rientrate sotto il testo dopo "↱ ". Il messaggio citato si cerca
/// nella cronologia caricata, poi fra quelli chiesti a parte (`quoted`);
/// finché non arriva resta il marcatore (o la citazione parziale, che
/// Telegram manda insieme alla risposta).
fn reply_line(
    row: &tg::HistoryRow,
    rows: &VecDeque<tg::HistoryRow>,
    quoted: Option<&HashMap<i32, Option<tg::HistoryRow>>>,
    me: &str,
    width: usize,
) -> Option<Vec<String>> {
    let id = row.reply_to?;
    let fetched = quoted.and_then(|q| q.get(&id));
    if matches!(fetched, Some(None)) && row.reply_quote.is_none() {
        return Some(vec![fit(tr("↱ messaggio eliminato").to_string(), width)]);
    }
    let orig = rows
        .iter()
        .find(|r| r.id == id)
        .or_else(|| fetched.and_then(|f| f.as_ref()));
    let body = row
        .reply_quote
        .as_deref()
        .map(|q| format!("“{}”", q.split_whitespace().collect::<Vec<_>>().join(" ")))
        .or_else(|| orig.map(row_summary));
    let text = match (orig, body) {
        (Some(o), Some(b)) => format!("{}: {b}", author(o, me)),
        (None, Some(b)) => b,
        _ => "…".to_string(),
    };
    let mut pieces = wrap(&text, width.saturating_sub(2).max(1));
    if pieces.len() > QUOTE_ROWS {
        pieces.truncate(QUOTE_ROWS);
        let last = pieces.pop().unwrap_or_default();
        // "…" deve entrare: se la riga è piena si toglie un carattere
        pieces.push(fit(format!("{last}…"), width.saturating_sub(2)));
    }
    Some(
        pieces
            .into_iter()
            .enumerate()
            .map(|(i, p)| format!("{} {p}", if i == 0 { "↱" } else { " " }))
            .collect(),
    )
}

/// Taglia a `width` caratteri, con "…" in fondo se serve.
fn fit(text: String, width: usize) -> String {
    if text.chars().count() <= width {
        return text;
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// Riga informativa mostrata alla selezione di un messaggio.
pub fn row_meta(row: &tg::HistoryRow, tz: &jiff::tz::TimeZone) -> String {
    let mut parts = vec![format!("#{}", row.id)];
    if row.date != 0 {
        parts.push(fmt_hhmm(row.date, tz));
    }
    if let Some(r) = row.reply_to {
        parts.push(trf!("risposta a #{}", r));
    }
    match &row.fwd {
        Some(f) if f.is_empty() => parts.push(tr("inoltrato").into()),
        Some(f) => parts.push(trf!("inoltrato da {}", f)),
        None => {}
    }
    if row.edited {
        parts.push(tr("modificato").into());
    }
    parts.push(tr("Esc per uscire").into());
    parts.join(" — ")
}

/// La barra delle reazioni veloci: l'emoji scelta tra parentesi quadre.
pub fn react_bar(idx: usize) -> String {
    tg::QUICK_REACTIONS
        .iter()
        .enumerate()
        .map(|(i, e)| {
            if i == idx {
                format!("[{e}]")
            } else {
                format!(" {e}  ")
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// Word wrapping lineare, senza dipendenze. Le parole più larghe della riga
/// vengono spezzate a livello di carattere (URL, parole appiccicate…).
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width < 4 {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut cur = String::new();
        for word in para.split_whitespace() {
            // parola sola più larga della riga: spezzala carattere per carattere
            if word.chars().count() > width {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                for ch in word.chars() {
                    if cur.chars().count() == width {
                        out.push(std::mem::take(&mut cur));
                    }
                    cur.push(ch);
                }
                continue;
            }
            if cur.chars().count() + word.chars().count() + 1 > width && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
        out.push(cur);
    }
    out
}

/// un link cliccabile su una riga: (colonna inizio, colonna fine esclusiva, url).
pub type LinkSpan = (u16, u16, String);

/// Trova gli URL in un testo: (inizio, fine esclusiva, url) come indici char.
pub fn find_urls(text: &str) -> Vec<(usize, usize, String)> {
    const HTTP: [char; 7] = ['h', 't', 't', 'p', ':', '/', '/'];
    const HTTPS: [char; 8] = ['h', 't', 't', 'p', 's', ':', '/', '/'];
    let ch: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < ch.len() {
        let scheme = if ch[i..].starts_with(&HTTPS) {
            8
        } else if ch[i..].starts_with(&HTTP) {
            7
        } else {
            i += 1;
            continue;
        };
        let mut j = i + scheme;
        while j < ch.len() && !ch[j].is_whitespace() {
            j += 1;
        }
        while j > i + scheme
            && matches!(
                ch[j - 1],
                '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '"'
            )
        {
            j -= 1;
        }
        if j > i + scheme + 1 {
            out.push((i, j, ch[i..j].iter().collect()));
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Link su una riga già composta (senza wrap): (col inizio, col fine, url).
pub fn scan_links(line: &str) -> Vec<(u16, u16, String)> {
    find_urls(line)
        .into_iter()
        .map(|(s, e, url)| (s as u16, e as u16, url))
        .collect()
}

/// Come `wrap`, ma per ogni riga ritorna anche gli intervalli di colonna dei
/// link. Un URL spezzato su più righe compare su ciascuna con l'URL completo.
pub fn wrap_links(text: &str, width: usize) -> (Vec<String>, Vec<Vec<LinkSpan>>) {
    let urls = find_urls(text);
    let lines = wrap(text, width);
    if urls.is_empty() {
        return (lines, Vec::new());
    }
    // rigenera il wrap tracciando la posizione (riga, colonna) di ogni char
    // emesso, poi proietta gli intervalli dei link su quelle colonne.
    let ch: Vec<char> = text.chars().collect();
    let mut at: Vec<Option<(usize, u16)>> = vec![None; ch.len()];
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut line_no = 0usize;
    let mut ai = 0usize; // indice char assoluto nel testo originale
    for para in text.split('\n') {
        let pchars: Vec<char> = para.chars().collect();
        let mut w = 0usize;
        while w < pchars.len() {
            if pchars[w].is_whitespace() {
                ai += 1;
                w += 1;
                continue;
            }
            let wstart = w;
            while w < pchars.len() && !pchars[w].is_whitespace() {
                w += 1;
            }
            let wlen = w - wstart;
            // parola sola più larga della riga: spezza char per char (come wrap)
            if wlen > width {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    line_no += 1;
                }
                for k in 0..wlen {
                    if cur.chars().count() == width {
                        out.push(std::mem::take(&mut cur));
                        line_no += 1;
                    }
                    at[ai] = Some((line_no, cur.chars().count() as u16));
                    cur.push(pchars[wstart + k]);
                    ai += 1;
                }
                continue;
            }
            if cur.chars().count() + wlen + 1 > width && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                line_no += 1;
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            for k in 0..wlen {
                at[ai] = Some((line_no, cur.chars().count() as u16));
                cur.push(pchars[wstart + k]);
                ai += 1;
            }
        }
        out.push(std::mem::take(&mut cur));
        line_no += 1;
        if ai < ch.len() {
            ai += 1; // il '\n' tra i paragrafi
        }
    }
    // proietta ogni URL sulle righe che tocca
    let mut spans: Vec<Vec<LinkSpan>> = vec![Vec::new(); out.len()];
    for (s, e, url) in &urls {
        let mut per_line: Vec<(usize, u16, u16)> = Vec::new();
        for pos in &at[*s..*e] {
            if let Some((ln, col)) = *pos {
                match per_line.iter_mut().find(|(l, _, _)| *l == ln) {
                    Some((_, c0, c1)) => {
                        *c0 = (*c0).min(col);
                        *c1 = (*c1).max(col + 1);
                    }
                    None => per_line.push((ln, col, col + 1)),
                }
            }
        }
        for (ln, c0, c1) in per_line {
            if let Some(sl) = spans.get_mut(ln) {
                sl.push((c0, c1, url.clone()));
            }
        }
    }
    (out, spans)
}

pub fn draw(f: &mut Frame, app: &mut App, cmds: &mut Vec<Cmd>) {
    let area = f.area();
    // sfondo del terminale su tutto lo schermo
    f.render_widget(Block::new().style(Style::new().bg(BG)), area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    draw_breadcrumb(f, app, rows[0]);
    let side_w: u16 = if area.width > 90 {
        28
    } else if area.width > 60 {
        22
    } else {
        16
    };
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(side_w), Constraint::Min(20)])
        .split(rows[1]);
    draw_sidebar(f, app, body[0]);
    if app.show_search {
        draw_search(f, app, body[1]);
    } else if app.show_sticker {
        draw_stickers(f, app, body[1], cmds);
    } else {
        draw_history(f, app, body[1], cmds);
    }
}

/// Prima riga: percorso corrente a sinistra, stato locale a destra
/// (come il volume di jellyfin-tui).
fn draw_breadcrumb(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled("rondine", Style::new().fg(FAINT))];
    if let Some(chat) = app.current_chat() {
        spans.push(Span::styled(" › ", Style::new().fg(FAINT)));
        spans.push(Span::styled(
            chat.name.clone(),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
        if app.show_sticker {
            spans.push(Span::styled(" › sticker", Style::new().fg(FAINT)));
        } else if app.show_search {
            spans.push(Span::styled(tr(" › ricerca"), Style::new().fg(FAINT)));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    if !app.status.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(app.status.clone(), Style::new().fg(ACCENT)))
                .alignment(Alignment::Right),
            area,
        );
    }
}

/// Pannello arrotondato: cornice scura per i pannelli dormienti, ambra
/// per quello attivo; il titolo vive nella riga superiore.
fn pane(title: &str, focused: bool) -> Block<'static> {
    let color = if focused { ACCENT } else { FG };
    Block::bordered()
        .border_set(border::ROUNDED)
        .border_style(Style::new().fg(color))
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ))
}

/// Conteggio sul lato destro della riga superiore della cornice
/// (arrestato prima dell'angolo, che resta disegnato dal bordo).
fn render_count(f: &mut Frame, area: Rect, text: &str) {
    if text.is_empty() || area.width < text.len() as u16 + 6 {
        return;
    }
    f.render_widget(
        Paragraph::new(Span::styled(text.to_string(), Style::new().fg(FAINT)))
            .alignment(Alignment::Right),
        Rect {
            x: area.x,
            y: area.y,
            width: area.width.saturating_sub(2),
            height: 1,
        },
    );
}

/// Coppiehint «nome <tasto>» per la cornice inferiore, in stile jellyfin.
fn hints_line(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::with_capacity(pairs.len() * 3 + 1);
    for (name, key) in pairs {
        spans.push(Span::styled(
            format!(" {name} "),
            Style::new().add_modifier(Modifier::REVERSED),
        ));
        spans.push(Span::styled(format!("<{key}>"), Style::new().fg(ACCENT)));
    }
    Line::from(spans).centered()
}

/// Barra di scorrimento disegnata nella colonna destra della cornice.
fn rail(f: &mut Frame, area: Rect, total: usize, pos: usize) {
    if total < 2 || area.height < 4 {
        return;
    }
    let rect = Rect {
        x: area.right().saturating_sub(1),
        y: area.y + 1,
        width: 1,
        height: area.height.saturating_sub(2),
    };
    if rect.height == 0 {
        return;
    }
    let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(Some("↑"))
        .end_symbol(Some("↓"))
        .style(Style::new().fg(FAINT))
        .thumb_style(Style::new().fg(FG));
    let mut state = ScrollbarState::new(total).position(pos);
    f.render_stateful_widget(sb, rect, &mut state);
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    if app.contacts.is_empty() {
        f.render_widget(pane(tr("Conversazioni"), false), area);
        return;
    }
    let items: Vec<ListItem> = app
        .contacts
        .iter()
        .map(|d| {
            let badge = if d.unread > 0 {
                format!(" ({})", d.unread)
            } else {
                String::new()
            };
            // segnali tui in stile IRC: # gruppo, @ chat privata
            let mut spans = vec![Span::styled(
                if d.is_group { "#" } else { "@" },
                Style::new().fg(if d.is_group { ACCENT_DIM } else { FAINT }),
            )];
            spans.push(Span::styled(
                format!(" {}", d.name),
                Style::new().fg(if d.unread > 0 { ACCENT } else { FG }),
            ));
            if d.pinned {
                spans.push(Span::styled(" 📌", Style::new().fg(ACCENT)));
            }
            if d.unread > 0 {
                spans.push(Span::styled(badge, Style::new().fg(FAINT)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .style(Style::new().bg(BG))
        .highlight_style(Style::new().fg(FG).bg(BG).add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    let mut state = ListState::default();
    state.select(Some(app.contact_cursor.min(app.contacts.len() - 1)));
    f.render_stateful_widget(list, area, &mut state);
    render_count(f, area, &format!("({})", app.contacts.len()));
    rail(f, area, app.contacts.len(), app.contact_cursor);
}

/// Disegna la cronologia della chat aperta (o un pannello vuoto).
fn draw_history(f: &mut Frame, app: &mut App, area: Rect, cmds: &mut Vec<Cmd>) {
    let Some(chat) = app.current_chat() else {
        app.mouse_map.clear();
        app.link_map.clear();
        let hints = hints_line(&[(tr("apri"), tr("Invio")), ("sticker", "Tab"), (tr("esci"), "q")]);
        let block = pane("Rondine", true).title_bottom(hints);
        let inner = block.inner(area);
        f.render_widget(block, area);
        if inner.height > 1 {
            f.render_widget(
                Paragraph::new(Span::styled(
                    tr("↑↓ scegli una conversazione"),
                    Style::new().fg(FAINT),
                ))
                .alignment(Alignment::Center),
                inner,
            );
        }
        return;
    };
    // area messaggi + riquadro composizione multifila in basso
    let inner_w = area.width.saturating_sub(2) as usize;
    let comp_lines = (wrap(&app.compose, inner_w.saturating_sub(2)).len() as u16).clamp(1, 5);
    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(comp_lines + 2)])
        .split(area);

    // suggerimenti fissi nella cornice dei messaggi
    let hints = hints_line(&[
        (tr("invia"), tr("Invio")),
        (tr("rispondi"), "Ctrl-R"),
        (tr("edita"), "Ctrl-E"),
        (tr("canc"), "Ctrl-D"),
        (tr("copia"), "Ctrl-Y"),
        ("audio", "Ctrl-P"),
        ("sticker", "Tab"),
        (tr("ricerca"), "Ctrl-F"),
    ]);
    let block = pane(&chat.name.clone(), true).title_bottom(hints);
    let msgs = block.inner(body[0]);
    f.render_widget(block, body[0]);

    // riquadro di composizione: ambra in modifica, giallo se cancella,
    // l'immagine quando c'è una foto dagli appunti da inviare,
    // la barra delle reazioni quando è aperta (sostituisce il testo)
    let react_bar = app.react_open.then(|| react_bar(app.react_idx));
    let (c_border, c_title): (Color, Option<String>) = if app.react_open {
        (
            ACCENT,
            Some(tr(" reazioni · ←/→ o 1..8 · Invio toggle · Esc chiude ").into()),
        )
    } else if app.del_arm {
        (
            WARNING,
            Some(tr(" ☠ Invio = CANCELLA · Esc annulla ").into()),
        )
    } else if app.pending_photo.is_some() {
        (
            ACCENT,
            Some(tr(" 🖼 foto dagli appunti · Invio invia · testo = didascalia · Esc scarta ").into()),
        )
    } else if let Some(path) = &app.pending_file {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        (
            ACCENT,
            Some(trf!(" 📎 {} · Invio invia · testo = didascalia · Esc scarta ", name)),
        )
    } else if let Some(id) = app.editing {
        (
            ACCENT,
            Some(trf!(" ✎ modifica #{} · Invio salva · Esc annulla ", id)),
        )
    } else if let Some(id) = app.replying {
        // "↱ Autore: inizio del messaggio · Esc annulla", entro il bordo
        let target = app
            .open_history()
            .and_then(|ring| ring.iter().find(|r| r.id == id));
        let what = target
            .map(|r| format!("{}: {}", author(r, &app.me), row_summary(r)))
            .unwrap_or_else(|| format!("#{id}"));
        let tail = tr(" · Esc annulla ");
        let room = (body[1].width as usize)
            .saturating_sub(4 + 3 + tail.chars().count());
        (
            ACCENT,
            Some(format!(" \u{21b1} {}{tail}", fit(what, room))),
        )
    } else {
        (FAINT, None)
    };
    draw_input_box(
        f,
        body[1],
        react_bar
            .as_deref()
            .unwrap_or(app.compose.as_str()),
        c_title,
        c_border,
    );

    let width = msgs.width.saturating_sub(1) as usize;
    let lines = history_lines(app, width);

    // scegli le righe che entrano: col cursore sul messaggio selezionato,
    // altrimenti auto-scroll dal basso come prima
    let avail = msgs.height as usize;
    let sel = app.msg_cursor;
    let start = match sel.and_then(|sid| lines.iter().position(|l| l.id == sid)) {
        Some(fi) => {
            // piazza il messaggio selezionato circa a 1/3 dall'alto
            let mut budget = avail * 4 / 10;
            let mut s = fi;
            for (i, l) in lines.iter().enumerate().take(fi).rev() {
                let h = if l.sticker { l.rows } else { 1 } as usize;
                if budget < h {
                    break;
                }
                budget -= h;
                s = i;
            }
            s
        }
        None => {
            let mut used = 0usize;
            let mut start = lines.len();
            for (i, line) in lines.iter().enumerate().rev() {
                let h = if line.sticker { line.rows } else { 1 } as usize;
                if used + h > avail {
                    start = i + 1;
                    break;
                }
                used += h;
                start = i;
            }
            start.min(lines.len())
        }
    };
    let start = start.min(lines.len());

    // mappa delle righe a schermo per il click del mouse → selezione
    app.mouse_map.clear();
    app.link_map.clear();

    let mut y = msgs.y;
    let y_end = msgs.y + avail as u16;
    for line in &lines[start..] {
        if y >= y_end {
            break;
        }
        // il messaggio sotto il cursore prende la barra di selezione (colori invertiti)
        let picked = line.id != 0 && app.msg_cursor == Some(line.id);
        let (fgc, bgc) = (line.color, BG);
        let sel = if picked {
            Modifier::REVERSED
        } else {
            Modifier::empty()
        };
        if line.sticker {
            // etichetta nella prima riga riservata
            f.render_widget(
                Paragraph::new(Span::styled(
                    line.text.clone(),
                    Style::new().fg(fgc).bg(bgc).add_modifier(sel),
                )),
                Rect {
                    x: msgs.x,
                    y,
                    width: msgs.width,
                    height: 1,
                },
            );
            y += 1;
            // immagine (se convertita) nelle righe successive
            if let Some(media) = &line.sticker_media {
                let key = tg::media_key(media);
                if let Some(path) = app.previews.get(&key) {
                    let band = line.rows.saturating_sub(1);
                    let w = msgs.width.saturating_sub(1) * line.frac.max(1) / 100;
                    let (fw, fh) = gfx::square_fit(w.max(2) as u32, band as u32);
                    let rect = Rect {
                        x: msgs.x,
                        y,
                        width: (fw.max(1) as u16).min(msgs.width),
                        height: (fh.max(1) as u16).min(y_end.saturating_sub(y)).min(band),
                    };
                    cmds.push(Cmd::Place {
                        rect,
                        path: path.clone(),
                    });
                } else if !app.pending.is_empty() && !line.animated {
                    f.render_widget(
                        Paragraph::new(Span::styled(
                            tr("(in arrivo…)").to_string(),
                            Style::new()
                                .fg(INCOMING)
                                .bg(BG)
                                .add_modifier(sel),
                        )),
                        Rect {
                            x: msgs.x,
                            y,
                            width: msgs.width,
                            height: 1,
                        },
                    );
                }
            }
            // etichetta + banda immagine: click → selezione unica del blocco
            app.mouse_map.push((y - 1, line.id));
            for k in 0..line.rows.saturating_sub(1) {
                app.mouse_map.push((y + k, line.id));
            }
            y += line.rows.saturating_sub(1);
        } else {
            // colora anche le righe vuote della barra per un blocco continuo
            let width_fix = line.text.chars().count().max(msgs.width as usize);
            let padded = format!(
                "{:<width_fix$}",
                line.text,
                width_fix = width_fix.min(msgs.width as usize + 1)
            );
            f.render_widget(
                Paragraph::new(Span::styled(
                    padded,
                    Style::new().fg(fgc).bg(bgc).add_modifier(sel),
                )),
                Rect {
                    x: msgs.x,
                    y,
                    width: msgs.width,
                    height: 1,
                },
            );
            app.mouse_map.push((y, line.id));
            // i link della riga diventano cliccabili alle loro colonne
            for (c0, c1, url) in &line.links {
                app.link_map.push((y, msgs.x + c0, msgs.x + c1, url.clone()));
            }
            y += 1;
        }
    }
    rail(f, body[0], lines.len(), start);
}

/// Pannello dei risultati di ricerca nella chat aperta (Ctrl-F).
/// Disegna il riquadro multifila di input: bordo tondo arrotondato con
/// titolo opzionale, testo avvolto su quante righe ha l'area e cursore
/// blocco ambra alla fine dell'ultima riga visibile.
/// `area` comprende i bordi.
fn draw_input_box(f: &mut Frame, area: Rect, text: &str, title: Option<String>, border: Color) {
    let mut block = Block::bordered()
        .border_set(border::ROUNDED)
        .border_style(Style::new().fg(border));
    if let Some(t) = title {
        block = block
            .title_top(Line::from(t).left_aligned())
            .title_style(Style::new().fg(border));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);

    let wrapped = wrap(text, inner.width.saturating_sub(1) as usize);
    let skip = wrapped.len().saturating_sub(inner.height as usize);
    let last = wrapped.len().saturating_sub(1);
    let mut rows: Vec<Line> = Vec::new();
    for (i, piece) in wrapped.iter().skip(skip).enumerate() {
        let lead = if skip > 0 && i == 0 {
            "\u{2026} "
        } else if i == 0 {
            "\u{276f} "
        } else {
            "  "
        };
        if i + skip == last {
            rows.push(Line::from(vec![
                Span::styled(format!("{lead}{piece}"), Style::new().fg(FG)),
                Span::styled(
                    "\u{25ae}",
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
            ]));
        } else {
            rows.push(Line::from(Span::styled(
                format!("{lead}{piece}"),
                Style::new().fg(FG),
            )));
        }
    }
    if rows.is_empty() {
        rows.push(Line::from(Span::styled(
            "\u{276f} ",
            Style::new().fg(FAINT),
        )));
    }
    f.render_widget(Paragraph::new(rows), inner);
}

/// Pannello dei risultati di ricerca nella chat aperta (Ctrl-F):
/// lista dei colpi sopra e riquadro multifila con la query sotto.
fn draw_search(f: &mut Frame, app: &App, area: Rect) {
    // riquadro query multifila in basso (1..5 righe)
    let inner_w = area.width.saturating_sub(2) as usize;
    let q_lines = (wrap(&app.search_q, inner_w.saturating_sub(2)).len() as u16).clamp(1, 5);
    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(q_lines + 2)])
        .split(area);

    let (res_part, bag_part) = (body[0], body[1]);
    let bottom: Line<'static> = match app.search_results.as_ref() {
        None => Line::from(Span::styled(
            tr(" scrivi la frase, Invio la cerca sul server"),
            Style::new().fg(FAINT),
        ))
        .left_aligned(),
        Some(hits) => Line::from(vec![
            Span::styled(
                trf!(" {} risultati · cursore {} ", hits.len(), app.search_cursor + 1),
                Style::new().fg(ACCENT),
            ),
            Span::styled(tr("esci "), Style::new().add_modifier(Modifier::REVERSED)),
            Span::styled("<Esc>", Style::new().fg(ACCENT)),
        ])
        .left_aligned(),
    };
    let block = pane(tr("Ricerca"), true).title_bottom(bottom);
    let inner = block.inner(res_part);
    f.render_widget(block, res_part);

    // riquadro della query
    let (q_border, q_title): (Color, Option<String>) = if app.search_q.is_empty() {
        (
            FAINT,
            Some(tr(" ✎ query — Invio cerca · Esc chiude ").into()),
        )
    } else {
        (ACCENT, None)
    };
    draw_input_box(f, bag_part, &app.search_q, q_title, q_border);

    let Some(hits) = app.search_results.as_ref() else {
        if inner.height > 1 {
            f.render_widget(
                Paragraph::new(Span::styled(
                    tr("scrivi la frase da cercare e premi Invio"),
                    Style::new().fg(FAINT),
                ))
                .alignment(Alignment::Center),
                inner,
            );
        }
        return;
    };
    if hits.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                tr("nessun risultato"),
                Style::new().fg(INCOMING).bg(BG),
            )),
            inner,
        );
        return;
    }
    let items: Vec<ListItem> = hits
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let who = if !h.from.is_empty() {
                h.from.clone()
            } else if h.outgoing {
                "Io".to_string()
            } else {
                "Lui".to_string()
            };
            let mut spans = vec![
                Span::styled(
                    format!("{:>3} {} {} · ", i + 1, h.date, who),
                    Style::new().fg(FAINT),
                ),
                Span::styled(
                    view_wrap_first(&h.snippet, 60),
                    Style::new().fg(if h.outgoing { ACCENT } else { FG }),
                ),
            ];
            if i == app.search_cursor {
                spans.insert(
                    0,
                    Span::styled("▶ ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
                );
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .style(Style::new().bg(BG))
        .highlight_style(Style::new().fg(FG).bg(BG).add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    state.select(Some(app.search_cursor));
    f.render_stateful_widget(list, inner, &mut state);
    rail(f, res_part, hits.len(), app.search_cursor);
}

/// la prima riga utile di un testo lungo (per gli snippet)
fn view_wrap_first(text: &str, max: usize) -> String {
    text.chars().take(max).collect::<String>()
}

fn draw_stickers(f: &mut Frame, app: &App, area: Rect, cmds: &mut Vec<Cmd>) {
    if !app.set_level {
        // lista dei set installati
        let block = pane(tr("Set sticker"), true)
            .title_bottom(hints_line(&[(tr("apri"), tr("Invio")), (tr("chiudi"), "Tab")]));
        let inner = block.inner(area);
        f.render_widget(block, area);
        render_count(
            f,
            area,
            &format!("({})", app.sets.as_ref().map(|s| s.len()).unwrap_or(0)),
        );
        match app.sets.as_ref() {
            None => {
                f.render_widget(
                    Paragraph::new(Span::styled(
                        tr("caricamento set…"),
                        Style::new().fg(ACCENT_DIM).bg(BG),
                    )),
                    inner,
                );
            }
            Some(sets) if sets.is_empty() => {
                f.render_widget(
                    Paragraph::new(Span::styled(
                        tr("nessun set di sticker installato"),
                        Style::new().fg(INCOMING).bg(BG),
                    )),
                    inner,
                );
            }
            Some(sets) => {
                let items: Vec<ListItem> = sets
                    .iter()
                    .map(|s| {
                        let marker = if app.set_docs.contains_key(&s.set_id) {
                            Span::styled("✓ ", Style::new().fg(ACCENT))
                        } else {
                            Span::styled("… ", Style::new().fg(FAINT))
                        };
                        ListItem::new(Line::from(vec![
                            marker,
                            Span::styled(s.title.clone(), Style::new().fg(FG)),
                        ]))
                    })
                    .collect();
                let list = List::new(items)
                    .style(Style::new().bg(BG))
                    .highlight_style(Style::new().fg(FG).bg(BG).add_modifier(Modifier::REVERSED))
                    .highlight_symbol("▶ ");
                let mut state = ListState::default();
                state.select(Some(app.set_cursor.min(sets.len().saturating_sub(1))));
                f.render_stateful_widget(list, inner, &mut state);
                rail(f, area, sets.len(), app.set_cursor);
            }
        }
        return;
    }

    // griglia sticker del set aperto + riquadro «in evidenza»
    let cells = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(CARD_H)])
        .split(area);
    let Some(docs) = app.open_set.and_then(|id| app.set_docs.get(&id)) else {
        let block = pane("Sticker", true);
        f.render_widget(block, cells[0]);
        draw_card(f, app, cells[1], cmds, None);
        return;
    };
    if docs.is_empty() {
        let block = pane("Sticker", true);
        let inner = block.inner(cells[0]);
        f.render_widget(block, cells[0]);
        f.render_widget(
            Paragraph::new(Span::styled(tr("set vuoto"), Style::new().fg(INCOMING).bg(BG))),
            inner,
        );
        draw_card(f, app, cells[1], cmds, None);
        return;
    }
    let cols = STICKER_COLS;
    let per_row = STICKER_COLS as u16;
    let cell_w = cells[0].width.max(per_row) / per_row;
    let grid_rows = (cells[0].height / cell_h()) as usize;
    let total_rows = docs.len().div_ceil(cols);
    let rows_shown = grid_rows.min(total_rows);
    // allinea la vista alla riga che contiene il cursore
    let cursor_row = app.sticker_cursor / cols;
    let first_row = cursor_row
        .saturating_sub(rows_shown.saturating_sub(1))
        .min(total_rows - rows_shown);

    let ch = cell_h();
    let sel_title = app
        .sets
        .as_ref()
        .and_then(|sets| {
            app.open_set
                .and_then(|id| sets.iter().find(|s| s.set_id == id))
        })
        .map(|s| s.title.clone())
        .unwrap_or_else(|| "Sticker".to_string());
    let block = pane(&sel_title.clone(), true).title_bottom(hints_line(&[
        (tr("invia"), tr("Invio")),
        (tr("indietro"), "h"),
        (tr("chiudi"), "Tab"),
    ]));
    let inner = block.inner(cells[0]);
    f.render_widget(block, cells[0]);
    render_count(f, cells[0], &format!("({})", docs.len()));

    for r in 0..rows_shown {
        let row_idx = first_row + r;
        let y = inner.y + (r as u16) * ch;
        for c in 0..cols {
            let idx = row_idx * cols + c;
            let x = inner.x + (c as u16) * cell_w;
            let Some(media) = docs.get(idx) else { break };
            let emoji = tg::sticker_emoji(media);
            let selected = idx == app.sticker_cursor;
            // testo emoji in fondo alla cella
            let mut spans = vec![Span::styled(
                format!(" {}", emoji),
                Style::new()
                    .bg(BG)
                    .fg(if emoji.is_empty() { INCOMING } else { FG })
                    .add_modifier(if selected {
                        Modifier::BOLD | Modifier::REVERSED
                    } else {
                        Modifier::empty()
                    }),
            )];
            if selected {
                spans.push(Span::styled(" ", Style::new().add_modifier(Modifier::REVERSED)));
            }
            f.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect {
                    x,
                    y: y + ch - 1,
                    width: cell_w,
                    height: 1,
                },
            );
            // anteprima immagine (kitty) sulle righe sopra la cella
            let key = tg::media_key(media);
            if let Some(path) = app.previews.get(&key) {
                let (w, h) = gfx::square_fit((cell_w.saturating_sub(1)) as u32, (ch - 1) as u32);
                let rect = Rect {
                    x: x + 1,
                    y,
                    width: (w.max(1) as u16).min(cell_w.saturating_sub(1)),
                    height: (h.max(1) as u16).min(ch - 1),
                };
                cmds.push(Cmd::Place {
                    rect,
                    path: path.clone(),
                });
            }
        }
    }
    rail(f, cells[0], total_rows, first_row);

    // riquadro «in evidenza» con l'anteprima grande dello sticker selezionato
    let sel_idx = app.sticker_cursor.min(docs.len().saturating_sub(1));
    draw_card(
        f,
        app,
        cells[1],
        cmds,
        docs.get(sel_idx).map(|m| (m, sel_idx, docs.len())),
    );
}

/// Riquadro «in evidenza»: anteprima grande a sinistra, dettagli a destra,
/// come la card «Now playing» di jellyfin-tui.
fn draw_card(
    f: &mut Frame,
    app: &App,
    area: Rect,
    cmds: &mut Vec<Cmd>,
    sel: Option<(&grammers_client::media::Media, usize, usize)>,
) {
    let block = pane(tr("In evidenza"), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some((media, idx, total)) = sel else {
        f.render_widget(
            Paragraph::new(Span::styled(
                tr("muoviti nella griglia con ←→↑↓"),
                Style::new().fg(FAINT).bg(BG),
            )),
            inner,
        );
        return;
    };
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(16), Constraint::Min(1)])
        .split(inner);
    // anteprima grande (ritaglio quadrato entro il riquadro)
    let art = Rect {
        x: cols[0].x,
        y: cols[0].y,
        width: cols[0].width.saturating_sub(1),
        height: inner.height,
    };
    let key = tg::media_key(media);
    if let Some(path) = app.previews.get(&key) {
        let (w, h) = gfx::square_fit(art.width as u32, art.height as u32);
        let rect = Rect {
            x: art.x,
            y: art.y,
            width: (w.max(1) as u16).min(art.width),
            height: (h.max(1) as u16).min(art.height),
        };
        cmds.push(Cmd::Place {
            rect,
            path: path.clone(),
        });
    } else {
        f.render_widget(
            Paragraph::new(Span::styled("…", Style::new().fg(FAINT).bg(BG))),
            art,
        );
    }
    // colonna destra: emoji, posizione nel set e suggerimento
    let emoji = tg::sticker_emoji(media);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if !emoji.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(" {}", emoji),
            Style::new().fg(ACCENT).bg(BG).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(" ", Style::new().fg(FG))));
    }
    lines.push(Line::from(Span::styled(
        trf!(" sticker {} di {}", idx + 1, total),
        Style::new().fg(FAINT).bg(BG),
    )));
    lines.push(Line::from(Span::styled(" ", Style::new().fg(FG).bg(BG))));
    lines.push(Line::from(Span::styled(
        tr(" ⏎ invia alla chat"),
        Style::new().fg(FAINT).bg(BG),
    )));
    f.render_widget(
        Paragraph::new(lines),
        Rect {
            x: cols[1].x,
            y: cols[1].y,
            width: cols[1].width,
            height: cols[1].height,
        },
    );
}

/// Proietta la cronologia della chat aperta in righe schedulate dal TUI.
pub fn history_lines(app: &App, width: usize) -> Vec<HistoryLine> {
    let Some(rows) = app.open_history() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    let tz = jiff::tz::TimeZone::system();
    for row in rows {
        // etichetta: nome proprio per i messaggi in uscita, mittente per gli altri
        let name = author(row, &app.me);
        // risposta: la citazione sta sopra, nella colonna del testo
        let quoted = app
            .current_chat()
            .and_then(|c| app.quoted.get(&c.peer_id));
        let quote = reply_line(row, rows, quoted, &app.me, width.saturating_sub(LABEL_W + 3));
        for q in quote.into_iter().flatten() {
            lines.push(HistoryLine {
                text: format!("{} │ {q}", " ".repeat(LABEL_W)),
                color: FAINT,
                id: row.id,
                ..HistoryLine::sep()
            });
        }
        let label = msg_label(&fmt_hhmm(row.date, &tz), name);
        let name_style = if row.outgoing {
            OUTGOING
        } else {
            FG
        };
        match &row.kind {
            tg::RowKind::Text(text) => {
                let text = &format!(
                    "{}{text}{}",
                    row_lead(row),
                    if row.edited { tr(" (modificato)") } else { "" }
                );
                let (pieces, mut span_lines) =
                    wrap_links(text, width.saturating_sub(LABEL_W + 3));
                if pieces.is_empty() {
                    continue;
                }
                // le colonne dei link slittano della larghezza dell'etichetta
                let shift = (LABEL_W + 3) as u16;
                let first_spans = span_lines
                    .first_mut()
                    .map(|v| {
                        v.iter()
                            .map(|(a, b, u)| (a + shift, b + shift, u.clone()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                lines.push(HistoryLine {
                    text: format!("{label}{}", pieces[0]),
                    color: name_style,
                    sticker: false,
                    animated: false,
                    sticker_media: None,
                    rows: 1,
                    frac: 0,
                    id: row.id,
                    links: first_spans,
                });
                for (i, piece) in pieces[1..].iter().enumerate() {
                    let links = span_lines
                        .get(i + 1)
                        .map(|v| {
                            v.iter()
                                .map(|(a, b, u)| (a + shift, b + shift, u.clone()))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    lines.push(HistoryLine {
                        text: format!("{} │ {}", " ".repeat(LABEL_W), piece),
                        color: name_style,
                        ..HistoryLine::sep()
                    });
                    if let Some(last) = lines.last_mut() {
                        last.id = row.id;
                        last.links = links;
                    }
                }
                // la traduzione sotto il testo, nella stessa colonna
                let tr_lines = match app.open.and_then(|p| app.translations.get(&(p, row.id))) {
                    Some(crate::app::TrState::Shown(t)) => {
                        wrap(&format!("🌐 {}", t.text), width.saturating_sub(LABEL_W + 3))
                    }
                    Some(crate::app::TrState::Busy) => vec![tr("🌐 traduco…").to_string()],
                    _ => Vec::new(),
                };
                for piece in tr_lines {
                    lines.push(HistoryLine {
                        text: format!("{} │ {}", " ".repeat(LABEL_W), piece),
                        color: ACCENT_DIM,
                        ..HistoryLine::sep()
                    });
                    if let Some(last) = lines.last_mut() {
                        last.id = row.id;
                    }
                }
            }
            tg::RowKind::Sticker {
                media,
                emoji,
                animated,
            } => {
                lines.push(HistoryLine {
                    text: format!(
                        "{label}sticker {emoji}{}",
                        if *animated {
                            tr(" (tgs animato, non renderizzato)")
                        } else {
                            ""
                        }
                    ),
                    color: if *animated {
                        WARNING
                    } else {
                        ACCENT
                    },
                    sticker: !*animated,
                    animated: *animated,
                    sticker_media: Some(media.clone()),
                    rows: if *animated {
                        1
                    } else {
                        band_rows(false) + 1
                    },
                    frac: 50,
                    id: row.id,
                    links: Vec::new(),
                });
            }
            tg::RowKind::Photo {
                media,
                caption,
                gif,
            } => {
                let cap_txt = if caption.is_empty() {
                    String::new()
                } else {
                    format!(" — “{caption}”")
                };
                let line_text = format!(
                    "{label}{}{}",
                    if *gif { "🎞 gif" } else { tr("📷 foto") },
                    cap_txt
                );
                lines.push(HistoryLine {
                    text: line_text.clone(),
                    color: name_style,
                    sticker: true,
                    animated: *gif,
                    sticker_media: Some(media.clone()),
                    rows: band_rows(true) + 1,
                    frac: 66,
                    id: row.id,
                    links: scan_links(&line_text),
                });
            }
            tg::RowKind::Doc {
                media: _,
                name,
                size,
                caption,
            } => {
                let size_txt = size
                    .map(|s| format!(" ({})", tg::fmt_size(s as f64)))
                    .unwrap_or_default();
                let cap_txt = if caption.is_empty() {
                    String::new()
                } else {
                    format!(" — “{caption}”")
                };
                lines.push(HistoryLine {
                    text: format!("{label}📄 {name}{size_txt}{cap_txt}{}", tr(" · Ctrl-O salva")),
                    color: ACCENT_DIM,
                    ..HistoryLine::sep()
                });
                if let Some(last) = lines.last_mut() {
                    last.id = row.id;
                    last.links = scan_links(&last.text);
                }
            }
            tg::RowKind::Audio {
                name,
                seconds,
                caption,
                voice,
                ..
            } => {
                let main = if *voice {
                    trf!("🎙 vocale {}", tg::fmt_secs(*seconds))
                } else {
                    format!("🎧 {}", name)
                };
                let cap_txt = if caption.is_empty() {
                    String::new()
                } else {
                    format!(" — “{caption}”")
                };
                lines.push(HistoryLine {
                    text: format!("{label}{main}{cap_txt}{}", tr(" · Ctrl-P riproduce")),
                    color: ACCENT_DIM,
                    ..HistoryLine::sep()
                });
                if let Some(last) = lines.last_mut() {
                    last.links = scan_links(&last.text);
                }
            }
            tg::RowKind::Note(text) | tg::RowKind::Video { note: text, .. } => {
                lines.push(HistoryLine {
                    text: format!("{label}🎥 {text}"),
                    color: ACCENT_DIM,
                    ..HistoryLine::sep()
                });
                if let Some(last) = lines.last_mut() {
                    last.id = row.id;
                    last.links = scan_links(&last.text);
                }
            }
            tg::RowKind::Service(text) => {
                lines.push(HistoryLine {
                    text: format!("· {text}"),
                    color: INCOMING,
                    ..HistoryLine::sep()
                });
                if let Some(last) = lines.last_mut() {
                    last.id = row.id;
                    last.links = scan_links(&last.text);
                }
            }
        }
        // reazioni visibili sotto il messaggio
        if !row.reactions.is_empty() {
            let mut rtxt = String::new();
            for (i, r) in row.reactions.iter().enumerate() {
                if i > 0 {
                    rtxt.push_str("  ");
                }
                rtxt.push_str(&format!("{} {}", r.emoticon, r.count));
                if r.mine {
                    rtxt.push_str(" ✓");
                }
            }
            lines.push(HistoryLine {
                text: format!("{} │ {}", " ".repeat(LABEL_W), rtxt),
                color: FAINT,
                ..HistoryLine::sep()
            });
            if let Some(last) = lines.last_mut() {
                last.id = row.id;
            }
        }
        lines.push(HistoryLine::sep());
    }
    if !lines.is_empty() {
        lines.pop(); // rimuove l'ultimo separatore
    }
    lines
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Il riquadro di composizione deve crescere oltre una riga coi testi lunghi.
    #[test]
    fn compose_grows_multiline() {
        let breve = wrap("ciao", 70);
        assert_eq!(breve.len(), 1);
        let lungo = wrap(
            "questa è una frase piuttosto lunga che supera la larghezza della colonna e deve andare a capo",
            40,
        );
        assert!(
            lungo.len() > 1,
            "wrap doveva produrre più righe, ne ha {}",
            lungo.len()
        );
    }

    #[test]
    fn msg_label_fits() {
        assert_eq!(msg_label("14:32", "Pippo").chars().count(), LABEL_W + 3);
        assert_eq!(msg_label("14:32", "NomeLunghissimo"), "14:32 NomeLung │ ");
    }

    /// Gli URL si catturano fino allo spazio, senza punteggiatura finale.
    #[test]
    fn find_urls_stops_at_space_and_punctuation() {
        let u = find_urls("guarda https://esempio.it/a?b=1. e (http://x.it)");
        assert_eq!(u.len(), 2);
        assert_eq!(u[0].2, "https://esempio.it/a?b=1");
        assert_eq!(u[1].2, "http://x.it");
    }

    /// Un URL spezzato su più righe resta cliccabile su ogni frammento,
    /// sempre con l'URL completo.
    #[test]
    fn wrap_links_keeps_full_url_on_fragments() {
        let (lines, spans) = wrap_links("https://lungo.example/aaaaaaaaaaaaaaaaaaaaaaaa", 10);
        assert!(lines.len() > 1);
        assert_eq!(spans.len(), lines.len());
        for s in &spans {
            assert_eq!(s.len(), 1);
            assert_eq!(s[0].2, "https://lungo.example/aaaaaaaaaaaaaaaaaaaaaaaa");
        }
    }

    /// Le parole senza spazi più larghe della riga vanno spezzate.
    #[test]
    fn wrap_breaks_long_words() {
        let w = wrap("ciao_world_come_stai_oggi_bene_molto_bene_grazie_molto", 12);
        assert!(
            w.len() >= 4,
            "l'URL doveva essere spezzato in più righe, ne ha {w:?}"
        );
        for piece in &w {
            assert!(piece.chars().count() <= 12, "riga troppo lunga: {piece}");
        }
    }

    #[test]
    fn hhmm_follows_timezone() {
        let utc = jiff::tz::TimeZone::UTC;
        assert_eq!(fmt_hhmm(1_700_000_000, &utc), "22:13");
        assert_eq!(fmt_hhmm(0, &utc), "     ");
        let rome = jiff::tz::TimeZone::get("Europe/Rome").unwrap();
        assert_eq!(fmt_hhmm(1_700_000_000, &rome), "23:13");
    }

    #[test]
    fn meta_lists_reply_forward_edit() {
        let row = tg::HistoryRow {
            id: 7,
            outgoing: false,
            from: String::new(),
            kind: tg::RowKind::Text("x".into()),
            reactions: Vec::new(),
            date: 1_700_000_000,
            reply_to: Some(3),
            reply_quote: None,
            fwd: Some("Anna".into()),
            edited: true,
        };
        let m = row_meta(&row, &jiff::tz::TimeZone::UTC);
        assert_eq!(
            m,
            "#7 — 22:13 — risposta a #3 — inoltrato da Anna — modificato — Esc per uscire"
        );
        assert_eq!(row_lead(&row), "⤳ Anna: ");
    }

    fn text_row(id: i32, outgoing: bool, from: &str, text: &str, reply_to: Option<i32>) -> tg::HistoryRow {
        tg::HistoryRow {
            id,
            outgoing,
            from: from.into(),
            kind: tg::RowKind::Text(text.into()),
            reactions: Vec::new(),
            date: 0,
            reply_to,
            reply_quote: None,
            fwd: None,
            edited: false,
        }
    }

    #[test]
    fn reply_line_quotes_author_and_text() {
        let rows: VecDeque<_> = [
            text_row(1, true, "", "Spe faccio\n una prova", None),
            text_row(2, false, "Bice", "ok", Some(1)),
            text_row(3, false, "Bice", "boh", Some(99)),
        ]
        .into();
        assert_eq!(reply_line(&rows[0], &rows, None, "Marco", 40), None);
        assert_eq!(
            reply_line(&rows[1], &rows, None, "Marco", 40).unwrap(),
            vec!["↱ Marco: Spe faccio una prova"]
        );
        // lungo: va a capo rientrato, oltre QUOTE_ROWS righe chiude con "…"
        assert_eq!(
            reply_line(&rows[1], &rows, None, "Marco", 12).unwrap(),
            vec!["↱ Marco: Spe", "  faccio una", "  prova"]
        );
        assert_eq!(
            reply_line(&rows[1], &rows, None, "Marco", 9).unwrap(),
            vec!["↱ Marco:", "  Spe", "  faccio…"]
        );
        // citato fuori dalla cronologia: in attesa, poi dalla cache o eliminato
        assert_eq!(reply_line(&rows[2], &rows, None, "Marco", 40).unwrap(), vec!["↱ …"]);
        let mut quoted = HashMap::new();
        quoted.insert(99, Some(text_row(99, false, "Gark", "vecchio", None)));
        assert_eq!(
            reply_line(&rows[2], &rows, Some(&quoted), "Marco", 40).unwrap(),
            vec!["↱ Gark: vecchio"]
        );
        quoted.insert(99, None);
        assert_eq!(
            reply_line(&rows[2], &rows, Some(&quoted), "Marco", 40).unwrap(),
            vec!["↱ messaggio eliminato"]
        );
        let mut partial = text_row(4, false, "Bice", "sì", Some(1));
        partial.reply_quote = Some("una  prova".into());
        assert_eq!(
            reply_line(&partial, &rows, None, "Marco", 40).unwrap(),
            vec!["↱ Marco: “una prova”"]
        );
    }
}
