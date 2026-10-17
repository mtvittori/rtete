//! Vista TUI: lista conversazioni, cronologia con sticker inline e pannello
//! sticker. Le posizioni degli sticker vengono proiettate in `Cmd::Place`
//! che il ciclo principale esegue con il kitty graphics protocol dopo ogni
//! frame ratatui.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::kitty;
use crate::styles::*;
use crate::tg;
use crate::STICKER_COLS;

/// Righe verticali riservate a una immagine sticker.
pub const STICKER_ROWS: u16 = 5;

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
    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
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
        .split(main[0]);
    draw_sidebar(f, app, body[0]);
    if app.show_sticker {
        draw_stickers(f, app, body[1], cmds);
    } else {
        draw_history(f, app, body[1], cmds);
    }
    draw_status(f, app, main[1]);
}

fn draw_sidebar(f: &mut Frame, app: &crate::App, area: Rect) {
    if app.contacts.is_empty() {
        f.render_widget(
            Paragraph::new("nessuna conversazione")
                .style(Style::default().bg(PANEL).fg(INCOMING))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Conversazioni "),
                ),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = app
        .contacts
        .iter()
        .map(|d| {
            let badge = if d.unread > 0 {
                format!(" [{}]", d.unread)
            } else {
                String::new()
            };
            let icon = if d.is_group { "👥" } else { "👤" };
            let mut style = Style::default().fg(if d.unread > 0 { ACCENT } else { FG });
            if d.unread > 0 {
                style = style.add_modifier(Modifier::BOLD);
            }
            ListItem::new(Line::from(Span::styled(
                format!("{icon} {}{badge}", d.name),
                style,
            )))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(Style::default().bg(PANEL))
                .title(Span::styled(
                    " Conversazioni ".to_string(),
                    Style::default().fg(ACCENT),
                )),
        )
        .style(Style::default().bg(PANEL))
        .highlight_style(Style::default().bg(PANEL_HI))
        .highlight_symbol("▶ ");
    let mut state = ListState::default();
    state.select(Some(app.contact_cursor.min(app.contacts.len() - 1)));
    f.render_stateful_widget(list, area, &mut state);
}

/// Disegna la cronologia della chat aperta (o un hint).
fn draw_history(f: &mut Frame, app: &crate::App, area: Rect, cmds: &mut Vec<crate::Cmd>) {
    let Some(chat) = app.current_chat() else {
        f.render_widget(
            Paragraph::new("↑↓ naviga · ⏎ apri una conversazione · Tab sticker · q esci")
                .style(Style::default().bg(BG).fg(INCOMING))
                .alignment(ratatui::layout::Alignment::Center),
            area,
        );
        return;
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(BG))
        .title(Span::styled(
            format!(" {} ", chat.name),
            Style::default().fg(ACCENT),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // riga di composizione: ultima riga interna
    let compose = Rect {
        x: inner.x,
        y: inner.y + inner.height.saturating_sub(1),
        width: inner.width,
        height: 1,
    };
    let msgs = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };

    let width = msgs.width.saturating_sub(3) as usize;
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
                    Style::default().fg(line.color).bg(BG),
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
                            format!("(in arrivo…{:1$})", line.text.clone(), 0).replace("{:1$}", ""),
                            Style::default().fg(INCOMING).bg(BG),
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
                    Style::default().fg(line.color).bg(BG),
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

    f.render_widget(
        Paragraph::new(Span::styled(
            format!("❯ {}", app.compose),
            Style::default().fg(ACCENT).bg(BG),
        )),
        compose,
    );
}

const CELL_H: u16 = 6; // righe per cella della griglia sticker

fn draw_stickers(f: &mut Frame, app: &crate::App, area: Rect, cmds: &mut Vec<crate::Cmd>) {
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(PANEL))
        .title(Span::styled(
            " Sticker · Tab chiude · ↑↓←→ o h/l · ⏎ invia ".to_string(),
            Style::default().fg(ACCENT),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if !app.set_level {
        // lista dei set installati
        match app.sets.as_ref() {
            None => {
                f.render_widget(
                    Paragraph::new("caricamento set…")
                        .style(Style::default().bg(PANEL).fg(ACCENT_DIM)),
                    inner,
                );
            }
            Some(sets) if sets.is_empty() => {
                f.render_widget(
                    Paragraph::new("nessun set di sticker installato")
                        .style(Style::default().bg(PANEL).fg(INCOMING)),
                    inner,
                );
            }
            Some(sets) => {
                let items: Vec<ListItem> = sets
                    .iter()
                    .map(|s| {
                        let loaded = if app.set_docs.contains_key(&s.set_id) {
                            "✓"
                        } else {
                            "…?"
                        };
                        ListItem::new(Line::from(Span::styled(
                            format!("{loaded} {}", s.title),
                            Style::default().fg(FG),
                        )))
                    })
                    .collect();
                let list = List::new(items)
                    .style(Style::default().bg(PANEL))
                    .highlight_style(Style::default().bg(PANEL_HI).fg(ACCENT))
                    .highlight_symbol("▶ ");
                let mut state = ListState::default();
                state.select(Some(app.set_cursor.min(sets.len().saturating_sub(1))));
                f.render_stateful_widget(list, inner, &mut state);
            }
        }
        return;
    }

    // griglia sticker del set aperto
    let Some(docs) = app.open_set.and_then(|id| app.set_docs.get(&id)) else {
        f.render_widget(
            Paragraph::new("scaricamento set…").style(Style::default().bg(PANEL).fg(ACCENT_DIM)),
            inner,
        );
        return;
    };
    if docs.is_empty() {
        f.render_widget(
            Paragraph::new("set vuoto").style(Style::default().bg(PANEL).fg(INCOMING)),
            inner,
        );
        return;
    }
    let cols = STICKER_COLS;
    let per_row = STICKER_COLS as u16;
    let cell_w = inner.width.max(per_row) / per_row;
    let grid_rows = (inner.height / CELL_H) as usize;
    let total_rows = docs.len().div_ceil(cols);
    let rows_shown = grid_rows.min(total_rows);
    // allinea la vista alla riga che contiene il cursore
    let cursor_row = app.sticker_cursor / cols;
    let first_row = cursor_row
        .saturating_sub(rows_shown.saturating_sub(1))
        .min(total_rows - rows_shown);

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
            let mut style =
                Style::default()
                    .bg(PANEL)
                    .fg(if emoji.is_empty() { INCOMING } else { FG });
            if selected {
                style = style.bg(PANEL_HI).fg(ACCENT).add_modifier(Modifier::BOLD);
            }
            f.render_widget(
                Paragraph::new(Span::styled(emoji.clone(), style)),
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
}

fn draw_status(f: &mut Frame, app: &crate::App, area: Rect) {
    let text = if app.status.is_empty() {
        "↑↓←→ o j/k naviga · ⏎ apri/invia · Tab pannello sticker · Esc azzera · q esci".to_string()
    } else {
        app.status.clone()
    };
    f.render_widget(
        Paragraph::new(Span::styled(text, Style::default().fg(ACCENT).bg(PANEL_HI))),
        area,
    );
}
