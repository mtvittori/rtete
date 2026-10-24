//! Le viste iced: lista chat a sinistra, cronologia a bolle a destra,
//! barra di scrittura e pannello sticker.

use std::collections::VecDeque;

use grammers_client::media::Media;
use iced::font::Weight;
use iced::widget::text::Span;
use iced::widget::{
    button, column, container, image, mouse_area, rich_text, row, scrollable, space, span, text,
    text_input, Column, Row,
};
use iced::{Alignment, Center, Element, Fill, Font, Length};

use rtete::app::App;
use rtete::tg::{self, HistoryRow, RowKind};

use crate::style;
use crate::settings::ThemeChoice;
use crate::{Gui, Msg, Phase, COMPOSE, FWD_SEARCH, HISTORY};

const BOLD: Font = Font {
    weight: Weight::Bold,
    ..Font::DEFAULT
};
const BUBBLE_W: f32 = 520.0;
const STICKER_PX: f32 = 170.0;
const PHOTO_PX: f32 = 320.0;
const GRID_COLS: usize = 6;
/// caratteri della citazione sopra una risposta
const QUOTE_CHARS: usize = 44;
const SIDEBAR_W: f32 = 310.0;
const COMPACT_W: f32 = 76.0;
/// sotto questa larghezza la lista chat mostra solo gli avatar
const COMPACT_BELOW: f32 = 760.0;

/// Taglia con «…» a circa `max` caratteri (iced non ha l'ellissi).
fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

pub fn view(g: &Gui) -> Element<'_, Msg> {
    match &g.phase {
        Phase::Connecting(s) => centered(column![
            text("RTete").size(32).font(BOLD),
            text(s.as_str()).color(style::p().muted),
        ]),
        Phase::Failed(e) => centered(column![
            text("RTete non riesce a partire").size(22).font(BOLD),
            text(e.as_str()).color(style::p().danger),
        ]),
        Phase::Ready => iced::widget::responsive(move |size| {
            let right = if g.forwarding.is_some() {
                forward_picker(g)
            } else if g.show_settings {
                settings(g)
            } else {
                chat(g)
            };
            let mut pane = Column::new();
            if let Some((_, title, members)) = &g.app.invite {
                pane = pane.push(invite(title, *members));
            }
            // finestra stretta: la lista diventa una colonna di avatar
            let compact = size.width < COMPACT_BELOW;
            let main: Element<'_, Msg> = row![
                sidebar(g, compact),
                container(pane.push(right)).width(Fill).height(Fill)
            ]
            .into();
            match &g.viewer {
                Some(path) => iced::widget::stack![main, photo_viewer(path)].into(),
                None => main,
            }
        })
        .into(),
    }
}

/// Foto a tutta finestra: rotella per lo zoom, trascina per spostarla,
/// Esc o ✕ per chiudere.
fn photo_viewer(path: &std::path::Path) -> Element<'_, Msg> {
    let p = style::p();
    let bar = row![
        space().width(Fill),
        button(text("Apri fuori").size(13))
            .on_press(Msg::Link(format!("file://{}", path.display())))
            .style(style::flat(false)),
        button(text("✕").size(18)).on_press(Msg::Cancel).style(style::flat(false)),
    ]
    .spacing(8)
    .align_y(Center);
    container(
        column![
            container(bar).padding(10),
            image::viewer(image::Handle::from_path(path))
                .width(Fill)
                .height(Fill)
                .min_scale(0.5)
                .max_scale(8.0)
                .scale_step(0.15),
            container(text("rotella per lo zoom · trascina per spostare · Esc chiude").size(12).color(p.muted))
                .center_x(Fill)
                .padding(8),
        ],
    )
    .width(Fill)
    .height(Fill)
    .style(style::fill(iced::Color { a: 0.94, ..style::mix(p.bg, iced::Color::BLACK, 0.6) }))
    .into()
}

fn centered<'a>(c: Column<'a, Msg>) -> Element<'a, Msg> {
    container(c.spacing(12).align_x(Center))
        .center(Fill)
        .style(style::fill(style::p().bg))
        .into()
}

// ---------------------------------------------------------------- sidebar

fn sidebar(g: &Gui, compact: bool) -> Element<'_, Msg> {
    let app = &g.app;
    let p = style::p();
    let needle = g.filter.to_lowercase();
    let open = app.open;
    let items = app
        .contacts
        .iter()
        .filter(|d| needle.is_empty() || d.name.to_lowercase().contains(&needle))
        .map(|d| {
            let is_open = open.is_some_and(|o| tg::same_peer(o, d.peer_id));
            if compact {
                let mut face = iced::widget::Stack::new().push(avatar(app, d, 46.0));
                if d.unread > 0 {
                    face = face.push(
                        container(
                            container(text(d.unread.min(999).to_string()).size(10))
                                .padding([0, 5])
                                .style(style::pill(p.accent)),
                        )
                        .width(46)
                        .align_x(iced::Right),
                    );
                }
                return button(container(face).center_x(Fill))
                    .on_press(Msg::Open(d.peer_id))
                    .padding([6, 4])
                    .width(Fill)
                    .style(style::chat_item(is_open))
                    .into();
            }
            // spazio per il nome: larghezza fissa meno avatar, ora, icone
            let time = d.last.as_ref().map(|l| list_time(g, l.date)).unwrap_or_default();
            let mut room = SIDEBAR_W - 46.0 - 52.0 - time.chars().count() as f32 * 7.5;
            if d.last.as_ref().is_some_and(|l| l.outgoing) {
                room -= 20.0;
            }
            if d.is_group {
                room -= 20.0;
            }
            let name_max = (room / 8.6).max(4.0) as usize;
            let preview_max = ((SIDEBAR_W - 46.0 - 80.0) / 7.2) as usize;
            // riga 1: (gruppo) nome … spunte e ora, come Telegram
            let mut top = Row::new().spacing(6).align_y(Center);
            if d.broadcast {
                top = top.push(text("📢").size(12));
            } else if d.is_group {
                top = top.push(text("👥").size(12));
            }
            top = top.push(
                container(text(ellipsize(&d.name, name_max)).font(BOLD).wrapping(text::Wrapping::None))
                    .width(Fill)
                    .clip(true),
            );
            if let Some(last) = &d.last {
                if last.outgoing {
                    top = top.push(ticks(last.id, d.read_out, 12.0));
                }
                top = top.push(text(time).size(12).color(p.muted));
            }
            // riga 2: anteprima (o «sta scrivendo…») … badge o puntina
            let (preview, color) = match app.typing_in(d.peer_id) {
                Some(t) => (format!("{t}{}", dots(g)), p.link),
                None => (
                    d.last
                        .as_ref()
                        .and_then(|l| l.text.lines().next())
                        .unwrap_or("")
                        .to_string(),
                    p.muted,
                ),
            };
            let mut bottom = row![container(
                text(ellipsize(&preview, preview_max)).size(13).color(color).wrapping(text::Wrapping::None)
            )
            .width(Fill)
            .clip(true)]
            .spacing(6)
            .align_y(Center);
            if d.unread > 0 {
                bottom = bottom.push(
                    container(text(d.unread.to_string()).size(12))
                        .padding([1, 7])
                        .style(style::pill(p.accent)),
                );
            } else if d.pinned {
                bottom = bottom.push(text("📌").size(12));
            }
            let line = row![
                avatar(app, d, 46.0),
                column![top, bottom].spacing(3).width(Fill),
            ]
            .spacing(10)
            .align_y(Center);
            button(line)
                .on_press(Msg::Open(d.peer_id))
                .padding([7, 10])
                .width(Fill)
                .style(style::chat_item(is_open))
                .into()
        });
    let search = text_input("Cerca chat…", &g.filter)
        .style(style::input)
        .on_input(Msg::Filter)
        .padding(9);
    let gear = button(text("⚙").size(18))
        .on_press(Msg::ToggleSettings)
        .padding([6, 10])
        .style(style::flat(g.show_settings));
    let head: Element<'_, Msg> = if compact {
        container(gear).center_x(Fill).padding(10).into()
    } else {
        container(row![gear, search].spacing(6).align_y(Center)).padding(10).into()
    };
    container(column![
        head,
        scrollable(Column::with_children(items).padding(iced::Padding::ZERO.right(10)))
            .height(Fill),
    ])
    .width(if compact { COMPACT_W } else { SIDEBAR_W })
    .height(Fill)
    .style(style::fill(p.sidebar))
    .into()
}

/// ✓ inviato, ✓✓ letto (nel colore dei link)
fn ticks<'a>(id: i32, read_out: i32, size: f32) -> Element<'a, Msg> {
    let p = style::p();
    if id > 0 && id <= read_out {
        text("✓✓").size(size).color(p.link).into()
    } else {
        text("✓").size(size).color(p.muted).into()
    }
}

/// ora di oggi, giorno della settimana, oppure la data
fn list_time(g: &Gui, secs: i64) -> String {
    const GIORNI: [&str; 7] = ["lun", "mar", "mer", "gio", "ven", "sab", "dom"];
    let Some(z) = local(g, secs) else {
        return String::new();
    };
    let today = jiff::Zoned::now().date();
    let d = z.date();
    if d == today {
        z.strftime("%H:%M").to_string()
    } else if (today - d).get_days() < 7 && d < today {
        GIORNI[d.weekday().to_monday_zero_offset() as usize].to_string()
    } else {
        z.strftime("%d/%m/%y").to_string()
    }
}

/// Foto profilo tonda (o iniziale su colore) con il pallino «online».
fn avatar<'a>(app: &App, d: &tg::DialogInfo, size: f32) -> Element<'a, Msg> {
    let face: Element<'a, Msg> = match d.photo_id.and_then(|id| app.avatars.get(&id)) {
        Some(path) => image(image::Handle::from_path(path))
            .width(size)
            .height(size)
            .content_fit(iced::ContentFit::Cover)
            .border_radius(size / 2.0)
            .into(),
        None => initial(&d.name, size),
    };
    if !d.seen.online(jiff::Timestamp::now().as_second()) {
        return face;
    }
    let p = style::p();
    let dot = container(space())
        .width(size * 0.28)
        .height(size * 0.28)
        .style(move |_: &iced::Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(p.names[1])),
            border: iced::Border {
                radius: (size * 0.14).into(),
                width: 2.0,
                color: p.sidebar,
            },
            ..Default::default()
        });
    iced::widget::stack![
        face,
        container(dot).width(size).height(size).align_x(iced::Right).align_y(iced::Bottom)
    ]
    .into()
}

fn initial<'a>(name: &str, size: f32) -> Element<'a, Msg> {
    let initial: String = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".into());
    container(text(initial).size(size * 0.42).font(BOLD))
        .width(size)
        .height(size)
        .align_x(Center)
        .align_y(Center)
        .style(style::avatar(style::name_color(name)))
        .into()
}

// ------------------------------------------------------------------- chat

fn chat(g: &Gui) -> Element<'_, Msg> {
    let app = &g.app;
    let Some(chat) = app.current_chat() else {
        return container(text("Scegli una chat a sinistra").color(style::p().muted))
            .center(Fill)
            .style(style::fill(style::p().bg))
            .into();
    };
    let header = container(
        row![
            avatar(app, chat, 38.0),
            column![
                text(chat.name.as_str()).font(BOLD).size(16),
                subtitle(g, chat),
            ]
        ]
        .spacing(12)
        .align_y(Center),
    )
    .padding([10, 16])
    .width(Fill)
    .style(style::fill(style::p().header));

    let mut body = column![header, history(g)];
    // il pannello sale e scende: resta nel layout finché l'animazione corre
    let panel_h = g.panel.interpolate(0.0, 300.0, g.now);
    if panel_h > 0.5 {
        body = body.push(container(stickers(app)).height(panel_h).clip(true));
    }
    body = body.push(banners(app)).push(composer(app));
    if !app.status.is_empty() {
        body = body.push(
            container(text(app.status.as_str()).size(12).color(style::p().muted)).padding([4, 16]),
        );
    }
    container(body)
        .width(Fill)
        .height(Fill)
        .style(style::fill(style::p().bg))
        .into()
}

/// sotto il nome della chat: sta scrivendo, online, ultimo accesso
fn subtitle<'a>(g: &Gui, chat: &tg::DialogInfo) -> Element<'a, Msg> {
    let p = style::p();
    if let Some(t) = g.app.typing_in(chat.peer_id) {
        return text(format!("{t}{}", dots(g))).size(12).color(p.link).into();
    }
    let now = jiff::Timestamp::now().as_second();
    let line = match chat.seen {
        _ if chat.broadcast => "canale".to_string(),
        _ if chat.is_group => "gruppo".to_string(),
        s if s.online(now) => return text("online").size(12).color(p.link).into(),
        tg::Seen::Online(t) | tg::Seen::Offline(t) => match local(g, t) {
            Some(z) if z.date() == jiff::Zoned::now().date() => {
                format!("ultimo accesso alle {}", z.strftime("%H:%M"))
            }
            Some(z) => format!("ultimo accesso il {}", z.strftime("%d/%m alle %H:%M")),
            None => "chat privata".into(),
        },
        tg::Seen::Recently => "ultimo accesso di recente".into(),
        tg::Seen::LastWeek => "ultimo accesso questa settimana".into(),
        tg::Seen::LastMonth => "ultimo accesso questo mese".into(),
        tg::Seen::Unknown => "chat privata".into(),
    };
    text(line).size(12).color(p.muted).into()
}

fn history(g: &Gui) -> Element<'_, Msg> {
    let app = &g.app;
    let Some(ring) = app.open_history() else {
        return container(text("carico la cronologia…").color(style::p().muted))
            .center(Fill)
            .into();
    };
    let peer = app.open.expect("chat aperta");
    let mut col = Column::new().padding([12, 18]);
    if !app.older_done.contains(&peer) {
        let label = if app.older_busy.contains(&peer) {
            "carico…"
        } else {
            "messaggi precedenti"
        };
        col = col.push(
            container(
                button(text(label).size(12))
                    .on_press(Msg::Older)
                    .style(style::flat(false)),
            )
            .center_x(Fill),
        );
    }
    let mut last_day = None;
    let mut prev: Option<&HistoryRow> = None;
    for r in ring {
        let day = local(g, r.date).map(|z| z.date());
        let new_day = day.is_some() && day != last_day;
        if let Some(d) = day.filter(|_| new_day) {
            last_day = day;
            col = col.push(
                container(
                    container(text(day_label(d)).size(12))
                        .padding([3, 10])
                        .style(style::pill(style::p().bubble_in)),
                )
                .center_x(Fill)
                .padding([10, 0]),
            );
        }
        // come Telegram: i messaggi di fila dello stesso autore (entro 5
        // minuti) stanno stretti e il nome compare solo sul primo
        let first = new_day
            || prev.is_none_or(|p| {
                p.outgoing != r.outgoing
                    || p.from != r.from
                    || matches!(p.kind, RowKind::Service(_))
                    || r.date - p.date > 300
            });
        col = col.push(
            container(message(g, ring, r, first)).padding(iced::Padding::ZERO.top(if first { 10 } else { 2 })),
        );
        prev = Some(r);
    }
    scrollable(col)
        .id(HISTORY)
        .anchor_bottom()
        .on_scroll(Msg::Scrolled)
        .height(Fill)
        .into()
}

/// ora, «modificato» e spunte in fondo alla bolla
fn meta<'a>(g: &Gui, r: &HistoryRow) -> Row<'a, Msg> {
    let p = style::p();
    let mut label = String::new();
    if r.edited {
        label.push_str("modificato ");
    }
    if let Some(z) = local(g, r.date) {
        label.push_str(&z.strftime("%H:%M").to_string());
    }
    let mut m = row![text(label).size(11).color(p.muted)].spacing(3).align_y(Center);
    if r.outgoing {
        let read_out = g.app.current_chat().map_or(0, |c| c.read_out);
        m = m.push(ticks(r.id, read_out, 11.0));
    }
    m
}

fn message<'a>(
    g: &'a Gui,
    ring: &'a VecDeque<HistoryRow>,
    r: &'a HistoryRow,
    first: bool,
) -> Element<'a, Msg> {
    let app = &g.app;
    let p = style::p();
    if let RowKind::Service(s) = &r.kind {
        return container(text(s.as_str()).size(12).color(p.muted)).center_x(Fill).into();
    }
    let selected = app.msg_cursor == Some(r.id);
    let group = app.current_chat().is_some_and(|c| c.is_group && !c.broadcast);
    let mut content = Column::new().spacing(4);
    if first && group && !r.outgoing && !r.from.is_empty() {
        content = content.push(
            text(r.from.as_str())
                .size(13)
                .font(BOLD)
                .color(style::name_color(&r.from)),
        );
    }
    if let Some(f) = &r.fwd {
        // senza autore noto il core mette «inoltrato»: niente doppione
        let label = if f == "inoltrato" { "↪ inoltrato".to_string() } else { format!("↪ inoltrato da {f}") };
        content = content.push(text(label).size(12).color(p.link));
    }
    if r.reply_to.is_some() {
        content = content.push(quote(app, ring, r));
    }
    let bare = matches!(r.kind, RowKind::Sticker { animated: false, .. } | RowKind::Video { round: true, .. });
    // testo: l'ora sta in fondo a destra accanto all'ultima riga e la
    // bolla è larga quanto il messaggio; media: l'ora va sotto
    content = match &r.kind {
        RowKind::Text(t) => content.push(
            row![container(linked(t)).max_width(BUBBLE_W - 90.0), meta(g, r)]
                .spacing(10)
                .align_y(iced::Bottom),
        ),
        _ => content.push(column![body(g, r), meta(g, r)].spacing(4).align_x(Alignment::End)),
    };
    if !r.reactions.is_empty() {
        let chips = r.reactions.iter().map(|it| {
            let emoticon = tg::QUICK_REACTIONS
                .iter()
                .find(|q| **q == it.emoticon)
                .copied();
            let b = button(text(format!("{} {}", it.emoticon, it.count)).size(13))
                .padding([2, 8])
                .style(style::flat(it.mine));
            match emoticon {
                Some(e) => b.on_press(Msg::React(r.id, e)),
                None => b,
            }
            .into()
        });
        content = content.push(Row::with_children(chips).spacing(4));
    }

    let bubble: Element<'a, Msg> = if bare {
        container(content).max_width(BUBBLE_W).padding(2).into()
    } else {
        container(content)
            .max_width(BUBBLE_W)
            .padding([7, 11])
            .style(style::bubble(r.outgoing, selected))
            .into()
    };
    // click: azioni; doppio click: risposta al volo
    let bubble = mouse_area(bubble)
        .on_press(Msg::Select(r.id))
        .on_double_click(Msg::Reply(r.id));
    let align = if r.outgoing { Alignment::End } else { Alignment::Start };
    let mut col = column![bubble].align_x(align).width(Fill);
    if selected {
        // la barra si apre scendendo
        let h = g.bar.interpolate(0.0, 160.0, g.now);
        col = col.push(
            container(actions(r)).max_height(h).clip(true).padding(iced::Padding::ZERO.top(4)),
        );
    }
    col.into()
}

/// la riga «↱ Autore: inizio del messaggio» sopra una risposta
fn quote<'a>(app: &'a App, ring: &'a VecDeque<HistoryRow>, r: &'a HistoryRow) -> Element<'a, Msg> {
    let id = r.reply_to.unwrap_or_default();
    let target = ring.iter().find(|x| x.id == id).map(Some).or_else(|| {
        let peer = app.open?;
        app.quoted.get(&peer)?.get(&id).map(|o| o.as_ref())
    });
    let (who, what) = match target {
        Some(Some(t)) => (author(app, t).to_string(), t.kind.summary()),
        Some(None) => (String::new(), "messaggio eliminato".into()),
        None => (String::new(), "…".into()),
    };
    // come Telegram: barretta colorata, autore sopra, inizio del testo
    // sotto; tutto tagliato con «…» e chiuso nella bolla (prima una
    // citazione lunga usciva dai bordi)
    let p = style::p();
    let what = r.reply_quote.clone().unwrap_or(what);
    let what = ellipsize(what.lines().next().unwrap_or(""), QUOTE_CHARS);
    let tint = if who.is_empty() { p.link } else { style::name_color(&who) };
    let tint = style::mix(tint, iced::Color::WHITE, 0.2);
    let mut lines = Column::new().spacing(1);
    if !who.is_empty() {
        lines = lines.push(
            text(ellipsize(&who, QUOTE_CHARS))
                .size(12)
                .font(BOLD)
                .color(tint)
                .wrapping(text::Wrapping::None),
        );
    }
    // testo quasi pieno: il grigio «muted» sullo sfondo della bolla si perdeva
    let body = iced::Color { a: 0.85, ..p.text };
    lines = lines.push(text(what).size(12).color(body).wrapping(text::Wrapping::None));
    let bar = container(space())
        .width(3)
        .height(if who.is_empty() { 16 } else { 32 })
        .style(style::pill(tint));
    container(row![bar, lines].spacing(7).align_y(Center))
        .padding([4, 8])
        .max_width(BUBBLE_W - 30.0)
        .clip(true)
        .style(style::quote_box(r.outgoing))
        .into()
}

fn author<'a>(app: &'a App, r: &'a HistoryRow) -> &'a str {
    if r.outgoing {
        &app.me
    } else {
        &r.from
    }
}

fn preview<'a>(app: &App, media: &Media, px: f32) -> Option<Element<'a, Msg>> {
    let path = app.previews.get(&tg::media_key(media))?;
    Some(
        image(image::Handle::from_path(path))
            .width(px)
            .content_fit(iced::ContentFit::ScaleDown)
            .into(),
    )
}

fn body<'a>(g: &'a Gui, r: &'a HistoryRow) -> Element<'a, Msg> {
    let app = &g.app;
    match &r.kind {
        RowKind::Video { media, seconds, caption, round, .. } => {
            let p = style::p();
            let px = if *round { 220.0 } else { PHOTO_PX };
            let thumb: Element<'a, Msg> = match app.previews.get(&tg::media_key(media)) {
                Some(path) => {
                    let img = image(image::Handle::from_path(path)).width(px);
                    if *round {
                        img.height(px)
                            .content_fit(iced::ContentFit::Cover)
                            .border_radius(px / 2.0)
                            .into()
                    } else {
                        img.content_fit(iced::ContentFit::ScaleDown).border_radius(8).into()
                    }
                }
                None => container(text("🎥").size(40))
                    .width(px)
                    .height(px * 0.6)
                    .align_x(Center)
                    .align_y(Center)
                    .style(style::pill(p.bubble_in))
                    .into(),
            };
            let play = button(text("▶").size(22).color(iced::Color::WHITE))
                .on_press(Msg::Play(r.id))
                .padding([10, 16])
                .style(|_: &iced::Theme, _| button::Style {
                    background: Some(iced::Background::Color(iced::Color::from_rgba(0.0, 0.0, 0.0, 0.55))),
                    border: iced::border::rounded(100),
                    ..Default::default()
                });
            let dur = container(text(tg::fmt_secs(*seconds)).size(11).color(iced::Color::WHITE))
                .padding([1, 6])
                .style(style::pill(iced::Color::from_rgba(0.0, 0.0, 0.0, 0.55)));
            let mut c = column![iced::widget::stack![
                thumb,
                container(play).center(Fill),
                container(dur).padding(6).align_left(Fill).align_bottom(Fill),
            ]]
            .spacing(6);
            if !caption.is_empty() {
                c = c.push(linked(caption));
            }
            c.into()
        }
        RowKind::Text(t) => linked(t),
        RowKind::Sticker { media, emoji, animated } => match (animated, preview(app, media, STICKER_PX)) {
            (false, Some(img)) => img,
            (true, _) => column![
                text(emoji.as_str()).size(64),
                text("sticker animato").size(11).color(style::p().muted)
            ]
            .into(),
            _ => text(emoji.as_str()).size(64).into(),
        },
        RowKind::Photo { media, caption, gif } => {
            let mut c = Column::new().spacing(6);
            // click sulla foto: visore a tutta finestra con zoom
            let pic = preview(app, media, PHOTO_PX).map(|img| -> Element<'a, Msg> {
                mouse_area(img)
                    .on_press(Msg::View(r.id))
                    .interaction(iced::mouse::Interaction::ZoomIn)
                    .into()
            });
            c = c.push(pic.unwrap_or_else(|| {
                text(if *gif { "GIF in caricamento…" } else { "🖼 foto in caricamento…" })
                    .color(style::p().muted)
                    .into()
            }));
            if !caption.is_empty() {
                c = c.push(linked(caption));
            }
            c.into()
        }
        RowKind::Doc { name, size, caption, .. } => {
            let mut info = name.clone();
            if let Some(s) = size {
                info.push_str(&format!(" · {}", tg::fmt_size(*s as f64)));
            }
            let mut c = column![row![
                text("📄").size(28),
                text(info).width(Length::Shrink),
                button(text("Salva").size(13)).on_press(Msg::Save(r.id)).style(style::primary),
            ]
            .spacing(10)
            .align_y(Center)]
            .spacing(6);
            if !caption.is_empty() {
                c = c.push(linked(caption));
            }
            c.into()
        }
        RowKind::Audio { name, seconds, caption, voice, .. } => {
            let label = if *voice { "Messaggio vocale".to_string() } else { name.clone() };
            let mut c = column![row![
                button(text("▶")).on_press(Msg::Play(r.id)).style(style::primary),
                column![
                    text(label),
                    text(tg::fmt_secs(*seconds)).size(12).color(style::p().muted)
                ],
            ]
            .spacing(10)
            .align_y(Center)]
            .spacing(6);
            if !caption.is_empty() {
                c = c.push(linked(caption));
            }
            c.into()
        }
        RowKind::Note(s) => text(s.as_str()).color(style::p().muted).into(),
        RowKind::Service(s) => text(s.as_str()).into(),
    }
}

/// testo con i link cliccabili (http/https, aperti con xdg-open)
fn linked(t: &str) -> Element<'_, Msg> {
    let mut spans: Vec<Span<'_, String>> = Vec::new();
    let mut plain = 0;
    let mut i = 0;
    while i < t.len() {
        let rest = &t[i..];
        if rest.starts_with("https://") || rest.starts_with("http://") {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ')', ';', ':', '!', '?', '»', '"']);
            if plain < i {
                spans.push(span(&t[plain..i]));
            }
            spans.push(span(url).link(url.to_string()).underline(true).color(style::p().link));
            i += url.len();
            plain = i;
            continue;
        }
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
    if plain < t.len() {
        spans.push(span(&t[plain..]));
    }
    rich_text(spans)
        .on_link_click(Msg::Link)
        .color(style::p().text)
        .wrapping(text::Wrapping::WordOrGlyph)
        .into()
}

/// la barra azioni sotto il messaggio selezionato
fn actions(r: &HistoryRow) -> Element<'_, Msg> {
    let act = |label: &'static str, m: Msg| {
        button(text(label).size(13)).on_press(m).padding([4, 8]).style(style::flat(false))
    };
    let mut bar = Row::new().spacing(2).align_y(Center);
    for e in tg::QUICK_REACTIONS {
        let mine = r.reactions.iter().any(|it| it.mine && it.emoticon == e);
        bar = bar.push(
            button(text(e).size(16))
                .on_press(Msg::React(r.id, e))
                .padding([2, 6])
                .style(style::flat(mine)),
        );
    }
    bar = bar.push(space().width(8)).push(act("↱ Rispondi", Msg::Reply(r.id)));
    if r.outgoing && matches!(r.kind, RowKind::Text(_)) {
        bar = bar.push(act("✎ Modifica", Msg::Edit(r.id)));
    }
    bar = bar.push(act("↪ Inoltra", Msg::Forward(r.id)));
    bar = bar.push(act("⧉ Copia", Msg::Copy(r.id)));
    if r.outgoing {
        bar = bar.push(
            button(text("🗑 Elimina").size(13))
                .on_press(Msg::AskDelete(r.id))
                .padding([4, 8])
                .style(style::flat(false)),
        );
    }
    container(bar.wrap())
        .padding(4)
        .style(style::pill(style::p().sidebar))
        .into()
}

// ------------------------------------------------------ scrittura e pannelli

fn banners(app: &App) -> Element<'_, Msg> {
    let close = || button(text("✕")).on_press(Msg::Cancel).style(style::flat(false));
    let bar = |label: String, extra: Option<Element<'static, Msg>>| -> Element<'static, Msg> {
        let mut r = row![text(label).color(style::p().link).width(Fill)].spacing(8).align_y(Center);
        if let Some(e) = extra {
            r = r.push(e);
        }
        container(r.push(close()))
            .padding([6, 16])
            .style(style::fill(style::p().sidebar))
            .into()
    };
    let mut col = Column::new();
    if app.del_arm {
        col = col.push(bar(
            "Eliminare il messaggio per tutti?".into(),
            Some(
                button(text("Elimina").size(13))
                    .on_press(Msg::ConfirmDelete)
                    .style(style::danger)
                    .into(),
            ),
        ));
    }
    if let Some(id) = app.replying {
        let what = app
            .open_history()
            .and_then(|ring| ring.iter().find(|r| r.id == id))
            .map(|r| format!("{}: {}", author(app, r), r.kind.summary()))
            .unwrap_or_else(|| "messaggio".into());
        let what: String = what.chars().take(100).collect();
        col = col.push(bar(format!("↱ Rispondi a {what}"), None));
    }
    if app.editing.is_some() {
        col = col.push(bar("✎ Modifica del messaggio — Invio salva".into(), None));
    }
    if let Some(e) = attachment(app) {
        col = col.push(e);
    }
    if let Some(name) = &app.uploading {
        col = col.push(
            container(
                text(format!("⬆ Invio di «{name}» in corso…")).size(13).color(style::p().muted),
            )
            .padding([6, 16])
            .width(Fill)
            .style(style::fill(style::p().sidebar)),
        );
    }
    col.into()
}

/// L'allegato in attesa: miniatura (se è un'immagine), nome e peso.
fn attachment(app: &App) -> Option<Element<'_, Msg>> {
    let (path, name) = match (&app.pending_photo, &app.pending_file) {
        (Some(p), _) => (std::path::PathBuf::from(p), "Immagine dagli appunti".to_string()),
        (None, Some(p)) => (
            p.clone(),
            p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        ),
        (None, None) => return None,
    };
    let kb = std::fs::metadata(&path).map(|m| m.len() / 1024).unwrap_or(0);
    let size = if kb >= 1024 { format!("{:.1} MB", kb as f64 / 1024.0) } else { format!("{kb} KB") };
    let thumb: Element<'_, Msg> = if tg::is_photo_file(&path) {
        image(image::Handle::from_path(&path)).height(72).into()
    } else {
        container(text("📄").size(36)).center(72).into()
    };
    let p = style::p();
    let info = column![
        text(ellipsize(&name, 60)).font(BOLD),
        text(format!("{size} · scrivi una didascalia e premi Invio")).size(12).color(p.muted),
    ]
    .spacing(2)
    .width(Fill);
    let close = button(text("✕")).on_press(Msg::Cancel).style(style::flat(false));
    Some(
        container(row![thumb, info, close].spacing(12).align_y(Center))
            .padding([8, 16])
            .style(style::fill(p.sidebar))
            .into(),
    )
}

fn composer(app: &App) -> Element<'_, Msg> {
    let attached = app.pending_photo.is_some() || app.pending_file.is_some();
    let placeholder = if attached { "Didascalia…" } else { "Scrivi un messaggio…" };
    container(
        row![
            button(text("📎").size(18))
                .on_press(Msg::Attach)
                .style(style::flat(false)),
            button(text("😊").size(18))
                .on_press(Msg::Stickers)
                .style(style::flat(app.show_sticker)),
            text_input(placeholder, &app.compose)
                .style(style::input)
                .id(COMPOSE)
                .on_input(Msg::Compose)
                .on_paste(Msg::Pasted)
                .on_submit(Msg::Submit)
                .padding(10)
                .width(Fill),
            button(text("Invia")).on_press(Msg::Submit).padding([8, 16]).style(style::primary),
        ]
        .spacing(8)
        .align_y(Center),
    )
    .padding([10, 14])
    .style(style::fill(style::p().header))
    .into()
}

fn stickers(app: &App) -> Element<'_, Msg> {
    // schede in alto come Telegram: preferiti, recenti, tutti i set
    let here = app.open_set.filter(|_| app.set_level);
    let tab = |label: &'static str, on: bool, m: Msg| {
        button(text(label).size(13)).on_press(m).padding([4, 10]).style(style::flat(on))
    };
    let title = match here {
        Some(tg::FAVED_SET) | Some(tg::RECENT_SET) | None => "",
        Some(id) => app
            .sets
            .as_ref()
            .and_then(|sets| sets.iter().find(|s| s.set_id == id))
            .map(|s| s.title.as_str())
            .unwrap_or(""),
    };
    let head = row![
        tab("⭐ Preferiti", here == Some(tg::FAVED_SET), Msg::OpenSet(tg::FAVED_SET)),
        tab("🕘 Recenti", here == Some(tg::RECENT_SET), Msg::OpenSet(tg::RECENT_SET)),
        tab("☰ Set", here.is_none(), Msg::SetsBack),
        text(title).font(BOLD).wrapping(text::Wrapping::None),
    ]
    .spacing(6)
    .align_y(Center);
    let inner: Element<'_, Msg> = match here {
        None => match &app.sets {
            None => text("carico i set…").color(style::p().muted).into(),
            Some(sets) => {
                let items = sets.iter().map(|s| {
                    button(text(s.title.as_str()))
                        .on_press(Msg::OpenSet(s.set_id))
                        .width(Fill)
                        .padding([6, 10])
                        .style(style::flat(false))
                        .into()
                });
                scrollable(Column::with_children(items).spacing(2)).height(Fill).into()
            }
        },
        Some(set_id) => {
            let grid: Element<'_, Msg> = match app.set_docs.get(&set_id) {
                None => text("scarico gli sticker…").color(style::p().muted).into(),
                Some(docs) if docs.is_empty() => text("niente qui: aggiungi sticker ai preferiti da Telegram")
                    .color(style::p().muted)
                    .into(),
                Some(docs) => {
                    let mut rows = Column::new().spacing(4);
                    for (r, chunk) in docs.chunks(GRID_COLS).enumerate() {
                        let mut line = Row::new().spacing(4);
                        for (c, media) in chunk.iter().enumerate() {
                            let face: Element<'_, Msg> = preview(app, media, 72.0).unwrap_or_else(|| {
                                let emoji = match media {
                                    Media::Sticker(s) => s.emoji().to_string(),
                                    _ => "…".into(),
                                };
                                container(text(emoji).size(36)).center(72).into()
                            });
                            line = line.push(
                                button(container(face).center(76))
                                    .on_press(Msg::SendSticker(r * GRID_COLS + c))
                                    .padding(2)
                                    .style(style::flat(false)),
                            );
                        }
                        rows = rows.push(line);
                    }
                    scrollable(rows).height(Fill).width(Fill).into()
                }
            };
            grid
        }
    };
    let inner: Element<'_, Msg> = column![head, inner].spacing(8).into();
    container(inner)
        .padding(10)
        .height(300)
        .width(Fill)
        .style(style::fill(style::p().sidebar))
        .into()
}

// ------------------------------------------------------------------ date

fn local(g: &Gui, secs: i64) -> Option<jiff::Zoned> {
    if secs == 0 {
        return None;
    }
    Some(jiff::Timestamp::from_second(secs).ok()?.to_zoned(g.tz.clone()))
}

fn day_label(d: jiff::civil::Date) -> String {
    const MESI: [&str; 12] = [
        "gennaio", "febbraio", "marzo", "aprile", "maggio", "giugno", "luglio", "agosto",
        "settembre", "ottobre", "novembre", "dicembre",
    ];
    let today = jiff::Zoned::now().date();
    if d == today {
        return "Oggi".into();
    }
    if today.yesterday().ok() == Some(d) {
        return "Ieri".into();
    }
    let mese = MESI[(d.month() - 1) as usize];
    if d.year() == today.year() {
        format!("{} {mese}", d.day())
    } else {
        format!("{} {mese} {}", d.day(), d.year())
    }
}

// ------------------------------------------------------- inviti e impostazioni

fn invite<'a>(title: &str, members: i32) -> Element<'a, Msg> {
    let p = style::p();
    container(
        row![
            text(format!("Invito a «{title}» · {members} membri")).color(p.link).width(Fill),
            button(text("Unisciti").size(13)).on_press(Msg::Join).style(style::primary),
            button(text("✕")).on_press(Msg::Cancel).style(style::flat(false)),
        ]
        .spacing(8)
        .align_y(Center),
    )
    .padding([8, 16])
    .width(Fill)
    .style(style::fill(p.header))
    .into()
}

fn settings(g: &Gui) -> Element<'_, Msg> {
    let p = style::p();
    let s = &g.settings;
    let section = |title: &'static str| text(title).size(13).font(BOLD).color(p.link);
    let choice = |label: String, on: bool, m: Msg| {
        button(text(label))
            .on_press(m)
            .padding([8, 14])
            .style(style::flat(on))
    };
    let omarchy_label = match style::omarchy_name() {
        Some(n) => format!("Tema Omarchy · {n}"),
        None => "Tema Omarchy".into(),
    };
    let handler_is_us = g.tg_handler == "rtete-gui.desktop";
    let handler_label = if g.tg_handler.is_empty() {
        "nessuna app".to_string()
    } else {
        g.tg_handler.trim_end_matches(".desktop").to_string()
    };
    let mut handler_row = row![text(format!("Oggi li apre: {handler_label}")).color(p.muted).width(Fill)]
        .spacing(8)
        .align_y(Center);
    if !handler_is_us {
        handler_row = handler_row.push(
            button(text("Usa RTete").size(13))
                .on_press(Msg::TgHandler)
                .style(style::primary),
        );
    }
    let body = column![
        row![
            text("Impostazioni").size(22).font(BOLD).width(Fill),
            button(text("✕")).on_press(Msg::ToggleSettings).style(style::flat(false)),
        ]
        .align_y(Center),
        section("ASPETTO"),
        row![
            choice(omarchy_label, s.theme == ThemeChoice::Omarchy, Msg::SetTheme(ThemeChoice::Omarchy)),
            choice("RTete scuro".into(), s.theme == ThemeChoice::Rtete, Msg::SetTheme(ThemeChoice::Rtete)),
        ]
        .spacing(8),
        text("Con il tema Omarchy i colori seguono `omarchy theme set` mentre RTete è aperta.")
            .size(12)
            .color(p.muted),
        row![
            text(format!("Dimensione  {:.0}%", s.scale * 100.0)).width(170),
            iced::widget::slider(0.7..=1.6, s.scale, Msg::Scale).step(0.05_f32),
        ]
        .spacing(12)
        .align_y(Center),
        iced::widget::toggler(s.gpu)
            .label("Accelerazione GPU: scorrimento fluido, ~115 MB invece di ~80 (al prossimo avvio)")
            .on_toggle(Msg::Gpu),
        section("NOTIFICHE"),
        iced::widget::toggler(s.notify)
            .label("Notifiche dei messaggi nuovi (non per le chat silenziate)")
            .on_toggle(Msg::Notify),
        iced::widget::toggler(s.notify_text)
            .label("Mostra il testo del messaggio")
            .on_toggle(Msg::NotifyText),
        section("LINK"),
        iced::widget::toggler(s.links_in_app)
            .label("Apri i link t.me e tg:// dentro RTete")
            .on_toggle(Msg::LinksInApp),
        text("Link tg:// da browser e altre app").color(p.text),
        handler_row,
        section("ACCOUNT"),
        text(format!("Connesso come {}", g.app.me)).color(p.muted),
        text("Numero e codice si cambiano con `rtete` nel terminale.").size(12).color(p.muted),
    ]
    .spacing(14)
    .max_width(620);
    container(scrollable(container(body).padding(28)).height(Fill))
        .width(Fill)
        .height(Fill)
        .style(style::fill(p.bg))
        .into()
}

/// i puntini di «sta scrivendo» che si riempiono (uno ogni 400 ms)
fn dots(g: &Gui) -> &'static str {
    let step = g.now.duration_since(g.epoch).as_millis() / 400;
    ["", ".", "..", "..."][(step % 4) as usize]
}

fn forward_picker(g: &Gui) -> Element<'_, Msg> {
    let p = style::p();
    let app = &g.app;
    let needle = g.fwd_filter.to_lowercase();
    let items = app
        .contacts
        .iter()
        .filter(|d| !d.broadcast || app.open.is_some_and(|o| tg::same_peer(o, d.peer_id)))
        .filter(|d| needle.is_empty() || d.name.to_lowercase().contains(&needle))
        .map(|d| {
            button(
                row![avatar(app, d, 36.0), text(d.name.as_str()).width(Fill)]
                    .spacing(12)
                    .align_y(Center),
            )
            .on_press(Msg::ForwardTo(d.peer_id))
            .padding([6, 10])
            .width(Fill)
            .style(style::flat(false))
            .into()
        });
    container(
        column![
            row![
                text("Inoltra a…").size(20).font(BOLD).width(Fill),
                button(text("✕")).on_press(Msg::Cancel).style(style::flat(false)),
            ]
            .align_y(Center),
            text_input("Cerca chat…", &g.fwd_filter)
                .style(style::input)
                .id(FWD_SEARCH)
                .on_input(Msg::FwdFilter)
                .padding(9),
            scrollable(Column::with_children(items).spacing(2)).height(Fill),
        ]
        .spacing(12)
        .max_width(520),
    )
    .padding(24)
    .center_x(Fill)
    .height(Fill)
    .style(style::fill(p.bg))
    .into()
}
