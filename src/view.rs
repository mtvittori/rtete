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

/// Righe verticali riservate a una immagine sticker.
pub const STICKER_ROWS: u16 = 5;

/// Altezza del riquadro «in evidenza» sotto la griglia sticker.
const CARD_H: u16 = 10;

/// Una riga di cronologia già proiettata per il rendering.
pub struct HistoryLine {
    pub text: String,
    pub color: Color,
    pub sticker: bool,
    pub animated: bool,
    pub sticker_media: Option<grammers_client::media::Media>,
    /// quante righe occupa nel terminale (più di 1 solo per gli sticker).
    pub rows: u16,
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
        }
    }
}

/// Word wrapping lineare, senza dipendenze.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width < 4 {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut cur = String::new();
        for word in para.split_whitespace() {
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
    if app.show_sticker {
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
    // composizione e suggerimenti vivono nella cornice inferiore
    let bottom: Line<'static> = if app.compose.is_empty() {
        hints_line(&[("invia", "Invio"), ("sticker", "Tab"), ("esci", "q")])
    } else {
        Line::from(Span::styled(
            format!(" ❯ {}", app.compose),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .left_aligned()
    };
    let block = pane(&chat.name.clone(), true).title_bottom(bottom);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let msgs = inner;
    let width = msgs.width.saturating_sub(1) as usize;
    let lines = app.history_lines(width);

    // scegli le righe che entrano da quelle più recenti verso le vecchie
    let avail = msgs.height as usize;
    let mut used = 0usize;
    let mut start = lines.len();
    for (i, line) in lines.iter().enumerate().rev() {
        let h = if line.sticker { STICKER_ROWS + 1 } else { 1 } as usize;
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
            // immagine sticker (se convertita) nelle righe successive
            if let Some(media) = &line.sticker_media {
                let key = tg::media_key(media);
                if let Some(path) = app.previews.get(&key) {
                    let (w, h) = kitty::square_fit((msgs.width / 2) as u32, STICKER_ROWS as u32);
                    let w = w.max(1) as u16;
                    let h = h.max(1) as u16;
                    let rect = Rect {
                        x: msgs.x,
                        y,
                        width: w.min(msgs.width),
                        height: h.min(y_end.saturating_sub(y)),
                    };
                    cmds.push(crate::Cmd::Place {
                        rect,
                        path: path.clone(),
                    });
                } else {
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
            y += STICKER_ROWS;
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

const CELL_H: u16 = 6; // righe per cella della griglia sticker

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
    let grid_rows = (cells[0].height / CELL_H) as usize;
    let total_rows = docs.len().div_ceil(cols);
    let rows_shown = grid_rows.min(total_rows);
    // allinea la vista alla riga che contiene il cursore
    let cursor_row = app.sticker_cursor / cols;
    let first_row = cursor_row
        .saturating_sub(rows_shown.saturating_sub(1))
        .min(total_rows - rows_shown);

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
        let y = inner.y + (r as u16) * CELL_H;
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
                    y: y + CELL_H - 1,
                    width: cell_w,
                    height: 1,
                },
            );
            // anteprima immagine (kitty) sulle righe sopra la cella
            let key = tg::media_key(media);
            if let Some(path) = app.previews.get(&key) {
                let (w, h) =
                    kitty::square_fit((cell_w.saturating_sub(1)) as u32, (CELL_H - 1) as u32);
                let rect = Rect {
                    x: x + 1,
                    y,
                    width: (w.max(1) as u16).min(cell_w.saturating_sub(1)),
                    height: (h.max(1) as u16).min(CELL_H - 1),
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
