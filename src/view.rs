//! Vista TUI in stile jellyfin-tui: pannelli arrotondati su carta crema,
//! titolo incastonato nella cornice, conteggio sul lato destro della cornice,
//! barra di selezione scura con `>`, cornice ambra sul pannello attivo e
//! suggerimenti dei tasti nella cornice in basso. Le posizioni degli sticker
//! vengono proiettate in `Cmd::Place` che il ciclo principale esegue con il
//! kitty graphics protocol dopo ogni frame ratatui.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;

use crate::kitty;
use crate::styles::*;
use crate::tg;
use crate::STICKER_COLS;

/// Altezza del riquadro «in evidenza» sotto la griglia sticker.
const CARD_H: u16 = 10;

/// RTETE_SCALE (0.5–4): moltiplicatore della grandezza delle immagini
/// (sticker nella griglia e nel messaggio, foto, gif). 1.0 di default.
fn scale() -> f32 {
    static S: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *S.get_or_init(|| {
        std::env::var("RTETE_SCALE")
            .ok()
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
        }
    }
}

/// Larghezza della colonna autore nelle righe di storia.
pub const LABEL_W: usize = 8;

/// Colonna autore: nome troncato su 8 caratteri + separatore, es.
/// "Pippo    │ " — le righe successive ripetono la colonna vuota.
pub fn msg_label(name: &str) -> String {
    let trunc: String = name.chars().take(LABEL_W).collect();
    format!("{trunc:<LABEL_W$} │ ")
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

pub fn draw(f: &mut Frame, app: &crate::App, cmds: &mut Vec<crate::Cmd>) {
    let area = f.area();
    // carta crema su tutto lo schermo
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
fn draw_breadcrumb(f: &mut Frame, app: &crate::App, area: Rect) {
    let mut spans = vec![Span::styled("rtete", Style::new().fg(FAINT))];
    if let Some(chat) = app.current_chat() {
        spans.push(Span::styled(" › ", Style::new().fg(FAINT)));
        spans.push(Span::styled(
            chat.name.clone(),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
        if app.show_sticker {
            spans.push(Span::styled(" › sticker", Style::new().fg(FAINT)));
        } else if app.show_search {
            spans.push(Span::styled(" › ricerca", Style::new().fg(FAINT)));
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
            Style::new().fg(BG).bg(SELECT),
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

fn draw_sidebar(f: &mut Frame, app: &crate::App, area: Rect) {
    if app.contacts.is_empty() {
        f.render_widget(pane("Conversazioni", false), area);
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
            if d.unread > 0 {
                spans.push(Span::styled(badge, Style::new().fg(FAINT)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .style(Style::new().bg(BG))
        .highlight_style(Style::new().bg(SELECT).fg(ON_SELECT))
        .highlight_symbol("▶ ");
    let mut state = ListState::default();
    state.select(Some(app.contact_cursor.min(app.contacts.len() - 1)));
    f.render_stateful_widget(list, area, &mut state);
    render_count(f, area, &format!("({})", app.contacts.len()));
    rail(f, area, app.contacts.len(), app.contact_cursor);
}

/// Disegna la cronologia della chat aperta (o un pannello vuoto).
fn draw_history(f: &mut Frame, app: &crate::App, area: Rect, cmds: &mut Vec<crate::Cmd>) {
    let Some(chat) = app.current_chat() else {
        let hints = hints_line(&[("apri", "Invio"), ("sticker", "Tab"), ("esci", "q")]);
        let block = pane("RTete", true).title_bottom(hints);
        let inner = block.inner(area);
        f.render_widget(block, area);
        if inner.height > 1 {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "↑↓ scegli una conversazione",
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
        ("invia", "Invio"),
        ("edita", "Ctrl-E"),
        ("canc", "Ctrl-D"),
        ("audio", "Ctrl-P"),
        ("sticker", "Tab"),
        ("ricerca", "Ctrl-F"),
    ]);
    let block = pane(&chat.name.clone(), true).title_bottom(hints);
    let msgs = block.inner(body[0]);
    f.render_widget(block, body[0]);

    // riquadro di composizione: ambra in modifica, giallo se cancella
    let (c_border, c_title): (Color, Option<String>) = if app.del_arm {
        (
            WARNING,
            Some(" \u{2620} Invio = CANCELLA \u{b7} Esc annulla ".into()),
        )
    } else if let Some(id) = app.editing {
        (
            ACCENT,
            Some(format!(
                " \u{270e} modifica #{id} \u{b7} Invio salva \u{b7} Esc annulla "
            )),
        )
    } else {
        (FAINT, None)
    };
    draw_input_box(f, body[1], &app.compose, c_title, c_border);

    let width = msgs.width.saturating_sub(1) as usize;
    let lines = app.history_lines(width);

    // scegli le righe che entrano da quelle più recenti verso le vecchie
    let avail = msgs.height as usize;
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
    let start = start.min(lines.len());

    let mut y = msgs.y;
    let y_end = msgs.y + avail as u16;
    for line in &lines[start..] {
        if y >= y_end {
            break;
        }
        if line.sticker {
            // etichetta nella prima riga riservata
            f.render_widget(
                Paragraph::new(Span::styled(
                    line.text.clone(),
                    Style::new().fg(line.color).bg(BG),
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
                    let (fw, fh) = kitty::square_fit(w.max(2) as u32, band as u32);
                    let rect = Rect {
                        x: msgs.x,
                        y,
                        width: (fw.max(1) as u16).min(msgs.width),
                        height: (fh.max(1) as u16).min(y_end.saturating_sub(y)).min(band),
                    };
                    cmds.push(crate::Cmd::Place {
                        rect,
                        path: path.clone(),
                    });
                } else if !app.pending.is_empty() && !line.animated {
                    f.render_widget(
                        Paragraph::new(Span::styled(
                            "(in arrivo…)".to_string(),
                            Style::new().fg(INCOMING).bg(BG),
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
            y += line.rows.saturating_sub(1);
        } else {
            f.render_widget(
                Paragraph::new(Span::styled(
                    line.text.clone(),
                    Style::new().fg(line.color).bg(BG),
                )),
                Rect {
                    x: msgs.x,
                    y,
                    width: msgs.width,
                    height: 1,
                },
            );
            y += 1;
        }
    }
    rail(f, area, lines.len(), start);
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
fn draw_search(f: &mut Frame, app: &crate::App, area: Rect) {
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
            " scrivi la frase, Invio la cerca sul server",
            Style::new().fg(FAINT),
        ))
        .left_aligned(),
        Some(hits) => Line::from(vec![
            Span::styled(
                format!(
                    " {} risultati · cursore {} ",
                    hits.len(),
                    app.search_cursor + 1
                ),
                Style::new().fg(ACCENT),
            ),
            Span::styled("esci ", Style::new().fg(BG).bg(SELECT)),
            Span::styled("<Esc>", Style::new().fg(ACCENT)),
        ])
        .left_aligned(),
    };
    let block = pane("Ricerca", true).title_bottom(bottom);
    let inner = block.inner(res_part);
    f.render_widget(block, res_part);

    // riquadro della query
    let (q_border, q_title): (Color, Option<String>) = if app.search_q.is_empty() {
        (
            FAINT,
            Some(" \u{270e} query — Invio cerca \u{b7} Esc chiude ".into()),
        )
    } else {
        (ACCENT, None)
    };
    draw_input_box(f, bag_part, &app.search_q, q_title, q_border);

    let Some(hits) = app.search_results.as_ref() else {
        if inner.height > 1 {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "scrivi la frase da cercare e premi Invio",
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
                "nessun risultato",
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
        .highlight_style(Style::new().bg(SELECT).fg(ON_SELECT));
    let mut state = ListState::default();
    state.select(Some(app.search_cursor));
    f.render_stateful_widget(list, inner, &mut state);
    rail(f, res_part, hits.len(), app.search_cursor);
}

/// la prima riga utile di un testo lungo (per gli snippet)
fn view_wrap_first(text: &str, max: usize) -> String {
    text.chars().take(max).collect::<String>()
}

fn draw_stickers(f: &mut Frame, app: &crate::App, area: Rect, cmds: &mut Vec<crate::Cmd>) {
    if !app.set_level {
        // lista dei set installati
        let block = pane("Set sticker", true)
            .title_bottom(hints_line(&[("apri", "Invio"), ("chiudi", "Tab")]));
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
                        "caricamento set…",
                        Style::new().fg(ACCENT_DIM).bg(BG),
                    )),
                    inner,
                );
            }
            Some(sets) if sets.is_empty() => {
                f.render_widget(
                    Paragraph::new(Span::styled(
                        "nessun set di sticker installato",
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
                    .highlight_style(Style::new().bg(SELECT).fg(ON_SELECT))
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
            Paragraph::new(Span::styled("set vuoto", Style::new().fg(INCOMING).bg(BG))),
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
        ("invia", "Invio"),
        ("indietro", "h"),
        ("chiudi", "Tab"),
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
                    .bg(if selected { SELECT } else { BG })
                    .fg(if selected {
                        ON_SELECT
                    } else if emoji.is_empty() {
                        INCOMING
                    } else {
                        FG
                    })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            )];
            if selected {
                spans.push(Span::styled(" ", Style::new().bg(SELECT)));
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
                let (w, h) = kitty::square_fit((cell_w.saturating_sub(1)) as u32, (ch - 1) as u32);
                let rect = Rect {
                    x: x + 1,
                    y,
                    width: (w.max(1) as u16).min(cell_w.saturating_sub(1)),
                    height: (h.max(1) as u16).min(ch - 1),
                };
                cmds.push(crate::Cmd::Place {
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
    app: &crate::App,
    area: Rect,
    cmds: &mut Vec<crate::Cmd>,
    sel: Option<(&grammers_client::media::Media, usize, usize)>,
) {
    let block = pane("In evidenza", false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some((media, idx, total)) = sel else {
        f.render_widget(
            Paragraph::new(Span::styled(
                "muoviti nella griglia con ←→↑↓",
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
        let (w, h) = kitty::square_fit(art.width as u32, art.height as u32);
        let rect = Rect {
            x: art.x,
            y: art.y,
            width: (w.max(1) as u16).min(art.width),
            height: (h.max(1) as u16).min(art.height),
        };
        cmds.push(crate::Cmd::Place {
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
        format!(" sticker {} di {}", idx + 1, total),
        Style::new().fg(FAINT).bg(BG),
    )));
    lines.push(Line::from(Span::styled(" ", Style::new().fg(FG).bg(BG))));
    lines.push(Line::from(Span::styled(
        " ⏎ invia alla chat",
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
        assert_eq!(msg_label("Pippo").chars().count(), LABEL_W + 3);
        assert_eq!(msg_label("NomeLunghissimo"), "NomeLung │ ");
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
}
