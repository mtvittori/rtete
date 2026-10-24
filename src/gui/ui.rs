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
use crate::{Gui, Msg, Phase, COMPOSE, HISTORY};

const BOLD: Font = Font {
    weight: Weight::Bold,
    ..Font::DEFAULT
};
const BUBBLE_W: f32 = 520.0;
const STICKER_PX: f32 = 170.0;
const PHOTO_PX: f32 = 320.0;
const GRID_COLS: usize = 6;

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
        Phase::Ready => {
            let right = if g.show_settings { settings(g) } else { chat(g) };
            let mut pane = Column::new();
            if let Some((_, title, members)) = &g.app.invite {
                pane = pane.push(invite(title, *members));
            }
            row![
                sidebar(g),
                container(pane.push(right)).width(Length::FillPortion(5)).height(Fill)
            ]
            .into()
        }
    }
}

fn centered<'a>(c: Column<'a, Msg>) -> Element<'a, Msg> {
    container(c.spacing(12).align_x(Center))
        .center(Fill)
        .style(style::fill(style::p().bg))
        .into()
}

// ---------------------------------------------------------------- sidebar

fn sidebar(g: &Gui) -> Element<'_, Msg> {
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
            // riga 1: (gruppo) nome … spunte e ora, come Telegram
            let mut top = Row::new().spacing(6).align_y(Center);
            if d.broadcast {
                top = top.push(text("📢").size(12));
            } else if d.is_group {
                top = top.push(text("👥").size(12));
            }
            top = top.push(
                container(text(d.name.as_str()).font(BOLD).wrapping(text::Wrapping::None))
                    .width(Fill)
                    .clip(true),
            );
            if let Some(last) = &d.last {
                if last.outgoing {
                    top = top.push(ticks(last.id, d.read_out, 12.0));
                }
                top = top.push(text(list_time(g, last.date)).size(12).color(p.muted));
            }
            // riga 2: anteprima (o «sta scrivendo…») … badge o puntina
            let (preview, color) = match app.typing_in(d.peer_id) {
                Some(t) => (format!("{t}…"), p.link),
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
                text(preview).size(13).color(color).wrapping(text::Wrapping::None)
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
        .on_input(Msg::Filter)
        .padding(9);
    let gear = button(text("⚙").size(18))
        .on_press(Msg::ToggleSettings)
        .padding([6, 10])
        .style(style::flat(g.show_settings));
    container(column![
        container(row![gear, search].spacing(6).align_y(Center)).padding(10),
        scrollable(Column::with_children(items).padding(iced::Padding::ZERO.right(10)))
            .height(Fill),
    ])
    .width(Length::FillPortion(2))
    .max_width(360)
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
    if app.show_sticker {
        body = body.push(stickers(app));
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
        return text(format!("{t}…")).size(12).color(p.link).into();
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
    let mut col = Column::new().spacing(6).padding([12, 18]);
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
    for r in ring {
        let day = local(g, r.date).map(|z| z.date());
        if let Some(d) = day.filter(|_| day != last_day) {
            last_day = day;
            col = col.push(
                container(
                    container(text(day_label(d)).size(12))
                        .padding([3, 10])
                        .style(style::pill(style::p().bubble_in)),
                )
                .center_x(Fill)
                .padding([6, 0]),
            );
        }
        col = col.push(message(g, ring, r));
    }
    scrollable(col)
        .id(HISTORY)
        .anchor_bottom()
        .on_scroll(Msg::Scrolled)
        .height(Fill)
        .into()
}

fn message<'a>(g: &'a Gui, ring: &'a VecDeque<HistoryRow>, r: &'a HistoryRow) -> Element<'a, Msg> {
    let app = &g.app;
    if let RowKind::Service(s) = &r.kind {
        return container(text(s.as_str()).size(12).color(style::p().muted)).center_x(Fill).into();
    }
    let selected = app.msg_cursor == Some(r.id);
    let group = app.current_chat().is_some_and(|c| c.is_group);
    let mut content = Column::new().spacing(4);
    if group && !r.outgoing && !r.from.is_empty() {
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
        content = content.push(text(label).size(12).color(style::p().link));
    }
    if r.reply_to.is_some() {
        content = content.push(quote(app, ring, r));
    }
    let bare = matches!(r.kind, RowKind::Sticker { animated: false, .. });
    content = content.push(body(app, r));
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
        content = content.push(Row::with_children(chips).spacing(4).wrap());
    }
    let mut meta = String::new();
    if r.edited {
        meta.push_str("modificato · ");
    }
    if let Some(z) = local(g, r.date) {
        meta.push_str(&z.strftime("%H:%M").to_string());
    }
    let mut meta = row![text(meta).size(11).color(style::p().muted)].spacing(4).align_y(Center);
    if r.outgoing {
        let read_out = app.current_chat().map_or(0, |c| c.read_out);
        meta = meta.push(ticks(r.id, read_out, 11.0));
    }
    content = content.push(container(meta).align_right(Fill));

    let bubble: Element<'a, Msg> = if bare {
        container(content).max_width(BUBBLE_W).padding(4).into()
    } else {
        container(content)
            .max_width(BUBBLE_W)
            .padding([8, 12])
            .style(style::bubble(r.outgoing, selected))
            .into()
    };
    let bubble = mouse_area(bubble).on_press(Msg::Select(r.id));
    let align = if r.outgoing { Alignment::End } else { Alignment::Start };
    let mut col = column![bubble].align_x(align).width(Fill);
    if selected {
        col = col.push(actions(r));
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
    let what = r.reply_quote.clone().unwrap_or(what);
    let what: String = what.chars().take(90).collect();
    let line = if who.is_empty() {
        format!("↱ {what}")
    } else {
        format!("↱ {who}: {what}")
    };
    container(text(line).size(12).color(style::p().link).wrapping(text::Wrapping::None))
        .padding([2, 8])
        .style(style::pill(iced::Color { a: 0.25, ..style::p().bg }))
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

fn body<'a>(app: &'a App, r: &'a HistoryRow) -> Element<'a, Msg> {
    match &r.kind {
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
            c = c.push(preview(app, media, PHOTO_PX).unwrap_or_else(|| {
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
    bar = bar.push(act("⧉ Copia", Msg::Copy(r.id)));
    if r.outgoing {
        bar = bar.push(
            button(text("🗑 Elimina").size(13))
                .on_press(Msg::AskDelete(r.id))
                .padding([4, 8])
                .style(style::flat(false)),
        );
    }
    container(bar)
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
    if app.pending_photo.is_some() {
        col = col.push(bar(
            "🖼 Immagine dagli appunti pronta — scrivi una didascalia e premi Invio".into(),
            None,
        ));
    }
    col.into()
}

fn composer(app: &App) -> Element<'_, Msg> {
    let placeholder = if app.pending_photo.is_some() { "Didascalia…" } else { "Scrivi un messaggio…" };
    container(
        row![
            button(text("📎").size(18))
                .on_press(Msg::PasteImage)
                .style(style::flat(false)),
            button(text("😊").size(18))
                .on_press(Msg::Stickers)
                .style(style::flat(app.show_sticker)),
            text_input(placeholder, &app.compose)
                .id(COMPOSE)
                .on_input(Msg::Compose)
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
    let inner: Element<'_, Msg> = match (&app.sets, app.set_level, app.open_set) {
        (None, _, _) => text("carico i set…").color(style::p().muted).into(),
        (Some(sets), false, _) | (Some(sets), true, None) => {
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
        (Some(sets), true, Some(set_id)) => {
            let title = sets
                .iter()
                .find(|s| s.set_id == set_id)
                .map(|s| s.title.as_str())
                .unwrap_or("");
            let head = row![
                button(text("←")).on_press(Msg::SetsBack).style(style::flat(false)),
                text(title).font(BOLD),
            ]
            .spacing(8)
            .align_y(Center);
            let grid: Element<'_, Msg> = match app.set_docs.get(&set_id) {
                None => text("scarico gli sticker…").color(style::p().muted).into(),
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
            column![head, grid].spacing(8).into()
        }
    };
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
