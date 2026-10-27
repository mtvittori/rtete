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

use rondine::app::{App, TrState};
use rondine::tg::{self, HistoryRow, RowKind};
use rondine::{tr, trf};

use crate::style;
use crate::settings::{ThemeChoice, UiStyle};
use crate::{CacheKind, ChatFilter, Confirm, Gui, Msg, Phase, SettingsPage, COMPOSE, FWD_SEARCH, HISTORY};

const BOLD: Font = Font {
    weight: Weight::Bold,
    ..Font::DEFAULT
};
const BUBBLE_W: f32 = 520.0;
const STICKER_PX: f32 = 170.0;
const GRID_COLS: usize = 6;
/// caratteri della citazione sopra una risposta
const QUOTE_CHARS: usize = 44;
const SIDEBAR_W: f32 = 310.0;
const COMPACT_W: f32 = 76.0;
/// sotto questa larghezza la lista chat mostra solo gli avatar
const COMPACT_BELOW: f32 = 760.0;
/// larghezza della scheda della chat accanto alla cronologia
const INFO_W: f32 = 360.0;
/// sotto questa larghezza la scheda prende il posto della cronologia
const INFO_SIDE_BELOW: f32 = 1050.0;
/// lato delle miniature nella griglia dei media condivisi
const SHARED_PX: f32 = 104.0;

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
        Phase::Connecting(step, s) => loading(g, *step, s),
        Phase::Failed(e) => failed(e),
        Phase::Ready => iced::widget::responsive(move |size| {
            let info = g.show_info && g.app.current_chat().is_some();
            let right = if g.forwarding.is_some() {
                forward_picker(g)
            } else if g.show_settings {
                settings(g)
            } else if g.show_starred {
                starred_view(g)
            } else if info && size.width < INFO_SIDE_BELOW {
                info_panel(g, false)
            } else if info {
                row![chat(g), info_panel(g, true)].into()
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
            let mut layers = iced::widget::Stack::new().push(main);
            if let Some(what) = g.confirm {
                layers = layers.push(confirm_dialog(g, what));
            }
            if let Some(path) = &g.viewer {
                layers = layers.push(photo_viewer(path));
            }
            layers.into()
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
        button(text(tr("Apri fuori")).size(13))
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
            container(text(tr("rotella per lo zoom · trascina per spostare · Esc chiude")).size(12).color(p.muted))
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
        .padding(16)
        .style(style::fill(style::p().bg))
        .into()
}

/// il marchio della schermata di avvio: una «R» sul colore d'accento
fn logo<'a>(c: iced::Color) -> Element<'a, Msg> {
    container(text("R").size(46).font(BOLD).color(iced::Color::WHITE))
        .center(92)
        .style(style::avatar(c))
        .into()
}

/// La rotella: dodici puntini in cerchio, la scia sbiadisce dietro la
/// testa (un giro al secondo, guidato dal Tick dell'avvio).
fn spinner<'a>(g: &Gui, size: f32) -> Element<'a, Msg> {
    const N: usize = 12;
    let p = style::p();
    let head = (g.now.duration_since(g.epoch).as_millis() / 83) as usize % N;
    let dot = size * 0.14;
    let r = (size - dot) / 2.0;
    let mut wheel = iced::widget::Stack::new().width(size).height(size);
    for i in 0..N {
        let a = i as f32 / N as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        let age = (head + N - i) % N;
        let c = iced::Color { a: (1.0 - age as f32 / N as f32).max(0.12), ..p.accent };
        wheel = wheel.push(
            container(container(space()).width(dot).height(dot).style(style::avatar(c))).padding(
                iced::Padding { top: r + r * a.sin(), left: r + r * a.cos(), right: 0.0, bottom: 0.0 },
            ),
        );
    }
    wheel.into()
}

/// Schermata di avvio: marchio, rotella e i tre passi (fatti, in corso,
/// da fare) con la riga di stato che arriva dalla rete.
fn loading<'a>(g: &Gui, step: u8, status: &'a str) -> Element<'a, Msg> {
    let p = style::p();
    let steps_l = [tr("Connessione a Telegram"), tr("Verifica dell'account"), tr("Caricamento delle chat")];
    let mut steps = Column::new().spacing(8);
    for (i, label) in steps_l.iter().enumerate() {
        let (mark, color) = match (i as u8).cmp(&step) {
            std::cmp::Ordering::Less => ("✓", p.link),
            std::cmp::Ordering::Equal => ("●", p.text),
            std::cmp::Ordering::Greater => ("○", p.muted),
        };
        steps = steps.push(
            row![text(mark).size(13).color(color).width(18), text(*label).size(14).color(color)]
                .spacing(8)
                .align_y(Center),
        );
    }
    let card = container(steps).padding([14, 22]).style(style::pill(p.sidebar));
    centered(column![
        logo(p.accent),
        text("Rondine").size(30).font(BOLD),
        text(tr("Telegram leggero per Linux")).size(13).color(p.muted),
        space().height(14),
        spinner(g, 54.0),
        text(status).size(13).color(p.muted),
        space().height(6),
        card,
    ])
}

/// Avvio fallito: cosa è successo e come rimediare.
fn failed(e: &str) -> Element<'_, Msg> {
    let p = style::p();
    centered(column![
        container(text("!").size(46).font(BOLD).color(iced::Color::WHITE))
            .center(92)
            .style(style::avatar(p.danger)),
        text(tr("Rondine non riesce a partire")).size(22).font(BOLD),
        container(text(e).color(p.danger).align_x(Center)).max_width(520),
        text(tr("Controlla la connessione; il primo accesso si fa con `rondine` nel terminale."))
            .size(12)
            .color(p.muted),
        space().height(6),
        button(text(tr("Chiudi"))).on_press(Msg::Quit).padding([8, 18]).style(style::primary),
    ])
}

// ---------------------------------------------------------------- sidebar

fn sidebar(g: &Gui, compact: bool) -> Element<'_, Msg> {
    let app = &g.app;
    let p = style::p();
    let wa = style::wa();
    let needle = g.filter.to_lowercase();
    let open = app.open;
    let favorite = |d: &tg::DialogInfo| d.peer_id.bot_api_dialog_id().is_some_and(|id| g.settings.favorites.contains(&id));
    // archivio a parte; il filtro scelto vale per la lista principale
    let visible = |d: &&tg::DialogInfo| {
        if d.archived != g.show_archived {
            return false;
        }
        if !needle.is_empty() {
            return d.name.to_lowercase().contains(&needle);
        }
        g.show_archived
            || match g.chat_filter {
                ChatFilter::All => true,
                ChatFilter::Unread => d.unread > 0 || d.unread_mark,
                ChatFilter::Favorites => favorite(d),
                ChatFilter::Groups => d.is_group && !d.broadcast,
            }
    };
    let mut list = Column::new();
    if !compact && !g.show_archived && needle.is_empty() {
        let archived: Vec<&tg::DialogInfo> = app.contacts.iter().filter(|d| d.archived).collect();
        if !archived.is_empty() {
            let unread = archived.iter().filter(|d| d.unread > 0 && !d.muted).count();
            let mut line = row![text("📦").size(18).width(46).align_x(Center), text(tr("Archiviate")).width(Fill)]
                .spacing(10)
                .align_y(Center);
            if unread > 0 {
                line = line.push(text(unread.to_string()).size(12).color(p.accent));
            }
            list = list.push(
                button(line)
                    .on_press(Msg::ShowArchived(true))
                    .padding([9, 10])
                    .width(Fill)
                    .style(style::chat_item(false)),
            );
        }
    }
    for d in app.contacts.iter().filter(visible) {
        let is_open = open.is_some_and(|o| tg::same_peer(o, d.peer_id));
        let badge = |n: String| container(text(n).size(if compact { 10 } else { 12 })).padding([1, 7]).style(style::pill(p.accent));
        if compact {
            let mut face = iced::widget::Stack::new().push(avatar(app, d, 46.0));
            if d.unread > 0 || d.unread_mark {
                let n = if d.unread > 0 { d.unread.min(999).to_string() } else { String::new() };
                face = face.push(container(badge(n)).width(46).align_x(iced::Right));
            }
            list = list.push(
                button(container(face).center_x(Fill))
                    .on_press(Msg::Open(d.peer_id))
                    .padding([6, 4])
                    .width(Fill)
                    .style(style::chat_item(is_open)),
            );
            continue;
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
        let preview_max = ((SIDEBAR_W - 46.0 - 100.0) / 7.2) as usize;
        let unread = d.unread > 0 || d.unread_mark;
        // riga 1: (gruppo) nome … spunte e ora
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
            if last.outgoing && !wa {
                top = top.push(ticks(last.id, d.read_out, 12.0));
            }
            // WhatsApp: l'ora diventa verde se ci sono messaggi da leggere
            let time_color = if wa && unread { p.accent } else { p.muted };
            top = top.push(text(time).size(12).color(time_color));
        }
        // riga 2: (spunte) anteprima o «sta scrivendo…» … icone e badge
        let (preview, color) = match app.typing_in(d.peer_id) {
            Some(t) => (format!("{t}{}", dots(g)), p.link),
            None => (
                d.last.as_ref().and_then(|l| l.text.lines().next()).unwrap_or("").to_string(),
                p.muted,
            ),
        };
        let mut bottom = Row::new().spacing(6).align_y(Center);
        if wa {
            if let Some(last) = d.last.as_ref().filter(|l| l.outgoing) {
                bottom = bottom.push(ticks(last.id, d.read_out, 12.0));
            }
        }
        bottom = bottom.push(
            container(text(ellipsize(&preview, preview_max)).size(13).color(color).wrapping(text::Wrapping::None))
                .width(Fill)
                .clip(true),
        );
        if d.muted {
            bottom = bottom.push(text("🔕").size(11));
        }
        if d.pinned {
            bottom = bottom.push(text("📌").size(11));
        }
        if d.unread > 0 {
            bottom = bottom.push(badge(d.unread.to_string()));
        } else if d.unread_mark {
            bottom = bottom.push(badge(" ".into()));
        }
        let line = row![avatar(app, d, 46.0), column![top, bottom].spacing(3).width(Fill)]
            .spacing(10)
            .align_y(Center);
        // click: apre; tasto destro: il menu della chat
        list = list.push(
            mouse_area(
                button(line)
                    .on_press(Msg::Open(d.peer_id))
                    .padding([7, 10])
                    .width(Fill)
                    .style(style::chat_item(is_open || g.chat_menu == Some(d.peer_id))),
            )
            .on_right_press(Msg::ChatMenu(Some(d.peer_id))),
        );
        if g.chat_menu == Some(d.peer_id) {
            list = list.push(chat_menu(d, favorite(d)));
        }
    }
    let gear = button(text("⚙").size(18))
        .on_press(Msg::ToggleSettings)
        .padding([6, 10])
        .style(style::flat(g.show_settings));
    let star = button(text("⭐").size(16))
        .on_press(Msg::ShowStarred(!g.show_starred))
        .padding([6, 10])
        .style(style::flat(g.show_starred));
    let search = text_input(tr("Cerca chat…"), &g.filter)
        .style(style::input)
        .on_input(Msg::Filter)
        .padding(9);
    let head: Element<'_, Msg> = if compact {
        container(column![gear, star].spacing(4).align_x(Center)).center_x(Fill).padding(10).into()
    } else if g.show_archived {
        container(
            row![
                button(text("←").size(18)).on_press(Msg::ShowArchived(false)).padding([2, 10]).style(style::flat(false)),
                text(tr("Archiviate")).size(18).font(BOLD).width(Fill),
            ]
            .spacing(8)
            .align_y(Center),
        )
        .padding(12)
        .into()
    } else {
        let chip = |label: &'static str, f: ChatFilter| -> Element<'_, Msg> {
            button(text(label).size(12))
                .on_press(Msg::ChatFilter(f))
                .padding([4, 8])
                .style(style::chip(g.chat_filter == f))
                .into()
        };
        let chips = row![
            chip(tr("Tutte"), ChatFilter::All),
            chip(tr("Non lette"), ChatFilter::Unread),
            chip(tr("Preferite"), ChatFilter::Favorites),
            chip(tr("Gruppi"), ChatFilter::Groups),
        ]
        .spacing(4)
        .wrap();
        let top: Element<'_, Msg> = if wa {
            // WhatsApp: il titolo grande e le icone a destra, la ricerca sotto
            column![
                row![text(tr("Chat")).size(22).font(BOLD).width(Fill), star, gear].spacing(2).align_y(Center),
                search,
            ]
            .spacing(10)
            .into()
        } else {
            row![gear, search, star].spacing(6).align_y(Center).into()
        };
        container(column![top, chips].spacing(10)).padding(10).into()
    };
    let empty = !compact && !app.contacts.iter().any(|d| visible(&d));
    let mut body = Column::new().push(head);
    if empty {
        let what = if g.show_archived { tr("Nessuna chat archiviata") } else { tr("Nessuna chat qui") };
        body = body.push(container(text(what).size(13).color(p.muted)).center_x(Fill).padding(20));
    }
    container(body.push(scrollable(list.padding(iced::Padding::ZERO.right(10))).height(Fill)))
        .width(if compact { COMPACT_W } else { SIDEBAR_W })
        .height(Fill)
        .style(style::fill(p.sidebar))
        .into()
}

/// Il menu di una chat della lista (tasto destro), aperto sotto la voce.
fn chat_menu<'a>(d: &tg::DialogInfo, favorite: bool) -> Element<'a, Msg> {
    let p = style::p();
    let pid = d.peer_id;
    let item = |icon: &'static str, label: &'static str, m: Msg| -> Element<'a, Msg> {
        button(row![text(icon).width(24), text(label).size(14)].spacing(8).align_y(Center))
            .on_press(m)
            .width(Fill)
            .padding([6, 12])
            .style(style::flat(false))
            .into()
    };
    let unread = d.unread > 0 || d.unread_mark;
    let mut col = column![
        item("📦", if d.archived { tr("Togli dall'archivio") } else { tr("Archivia") }, Msg::ChatDo(pid, tg::ChatAction::Archive(!d.archived))),
        item("📌", if d.pinned { tr("Non fissare") } else { tr("Fissa in alto") }, Msg::ChatDo(pid, tg::ChatAction::Pin(!d.pinned))),
        item("🔕", if d.muted { tr("Riattiva le notifiche") } else { tr("Silenzia le notifiche") }, Msg::ChatDo(pid, tg::ChatAction::Mute(!d.muted))),
        item("●", if unread { tr("Segna come letta") } else { tr("Segna come da leggere") }, Msg::ChatDo(pid, tg::ChatAction::MarkUnread(!unread))),
        item("⭐", if favorite { tr("Togli dalle preferite") } else { tr("Aggiungi alle preferite") }, Msg::ToggleFavorite(pid)),
    ]
    .spacing(1);
    col = col.push(
        button(row![text("🗑").width(24), text(if d.is_group { tr("Esci ed elimina") } else { tr("Elimina chat") }).size(14).color(p.danger)].spacing(8))
            .on_press(Msg::MenuDelete(pid))
            .width(Fill)
            .padding([6, 12])
            .style(style::flat(false)),
    );
    container(col)
        .padding(4)
        .width(Fill)
        .style(style::pill(style::mix(p.sidebar, p.text, 0.06)))
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
    let Some(z) = local(g, secs) else {
        return String::new();
    };
    let today = jiff::Zoned::now().date();
    let d = z.date();
    if d == today {
        z.strftime("%H:%M").to_string()
    } else if (today - d).get_days() < 7 && d < today {
        rondine::i18n::weekdays()[d.weekday().to_monday_zero_offset() as usize].to_string()
    } else {
        z.strftime("%d/%m/%y").to_string()
    }
}

/// Foto profilo tonda (o iniziale su colore) con il pallino «online».
fn avatar<'a>(app: &App, d: &tg::DialogInfo, size: f32) -> Element<'a, Msg> {
    let photo = d.photo_id.filter(|_| app.media_prefs.avatars);
    let face: Element<'a, Msg> = match photo.and_then(|id| app.avatars.get(&id)) {
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
        return container(text(tr("Scegli una chat a sinistra")).color(style::p().muted))
            .center(Fill)
            .style(style::fill(style::p().bg))
            .into();
    };
    // click sul nome: la scheda con info, media, link, blocco
    let who = mouse_area(
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
    .on_press(Msg::ToggleInfo)
    .interaction(iced::mouse::Interaction::Pointer);
    let header = container(
        row![
            container(who).width(Fill),
            button(text("ⓘ").size(18))
                .on_press(Msg::ToggleInfo)
                .padding([4, 10])
                .style(style::flat(g.show_info)),
        ]
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
    let members = g.app.peer_info.get(&chat.peer_id).and_then(|i| i.members);
    let line = match chat.seen {
        _ if chat.broadcast => match members {
            Some(n) => trf!("canale · {} iscritti", n),
            None => tr("canale").to_string(),
        },
        _ if chat.is_group => match members {
            Some(n) => trf!("gruppo · {} membri", n),
            None => tr("gruppo").to_string(),
        },
        s if s.online(now) => return text(tr("online")).size(12).color(p.link).into(),
        tg::Seen::Online(t) | tg::Seen::Offline(t) => match local(g, t) {
            Some(z) if z.date() == jiff::Zoned::now().date() => {
                trf!("ultimo accesso alle {}", z.strftime("%H:%M"))
            }
            Some(z) => trf!("ultimo accesso il {} alle {}", z.strftime("%d/%m"), z.strftime("%H:%M")),
            None => tr("chat privata").into(),
        },
        tg::Seen::Recently => tr("ultimo accesso di recente").into(),
        tg::Seen::LastWeek => tr("ultimo accesso questa settimana").into(),
        tg::Seen::LastMonth => tr("ultimo accesso questo mese").into(),
        tg::Seen::Unknown => tr("chat privata").into(),
    };
    text(line).size(12).color(p.muted).into()
}

fn history(g: &Gui) -> Element<'_, Msg> {
    let app = &g.app;
    let Some(ring) = app.open_history() else {
        return container(text(tr("carico la cronologia…")).color(style::p().muted))
            .center(Fill)
            .into();
    };
    let peer = app.open.expect("chat aperta");
    let mut col = Column::new().padding([12, 18]);
    if !app.older_done.contains(&peer) {
        let label = if app.older_busy.contains(&peer) {
            tr("carico…")
        } else {
            tr("messaggi precedenti")
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
        label.push_str(tr("modificato "));
    }
    if let Some(z) = local(g, r.date) {
        label.push_str(&z.strftime("%H:%M").to_string());
    }
    let mut m = Row::new().spacing(3).align_y(Center);
    if g.app.open.is_some_and(|peer| g.app.is_starred(peer, r.id)) {
        m = m.push(text("⭐").size(10));
    }
    m = m.push(text(label).size(11).color(p.muted));
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
        // autore nascosto: solo «inoltrato»
        let label = if f.is_empty() { tr("↪ inoltrato").to_string() } else { trf!("↪ inoltrato da {}", f) };
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
    // la traduzione sotto il testo, con la lingua d'origine
    if let (RowKind::Text(_), Some(peer)) = (&r.kind, app.open) {
        match app.translations.get(&(peer, r.id)) {
            Some(TrState::Shown(t)) => {
                let label = match &t.source {
                    Some(src) => trf!("🌐 tradotto da {}", src.to_uppercase()),
                    None => tr("🌐 tradotto").to_string(),
                };
                let line = container(space()).width(Fill).height(1).style(style::fill(iced::Color { a: 0.25, ..p.text }));
                content = content.push(
                    column![line, text(label).size(11).color(p.link), container(linked(&t.text)).max_width(BUBBLE_W - 30.0)]
                        .spacing(4),
                );
            }
            Some(TrState::Busy) => content = content.push(text(tr("🌐 traduco…")).size(11).color(p.muted)),
            _ => {}
        }
    }
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
            .style(style::bubble(r.outgoing, selected, first))
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
            container(actions(g, r)).max_height(h).clip(true).padding(iced::Padding::ZERO.top(4)),
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
        Some(None) => (String::new(), tr("messaggio eliminato").into()),
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
            let px = if *round { 220.0 } else { g.settings.photo_px as f32 };
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
                text(tr("sticker animato")).size(11).color(style::p().muted)
            ]
            .into(),
            _ => text(emoji.as_str()).size(64).into(),
        },
        RowKind::Photo { media, caption, gif } => {
            let mut c = Column::new().spacing(6);
            // click sulla foto: visore a tutta finestra con zoom
            let pic = preview(app, media, g.settings.photo_px as f32).map(|img| -> Element<'a, Msg> {
                mouse_area(img)
                    .on_press(Msg::View(r.id))
                    .interaction(iced::mouse::Interaction::ZoomIn)
                    .into()
            });
            c = c.push(pic.unwrap_or_else(|| {
                // download automatico spento: la foto arriva con un click
                if !app.media_prefs.photos && !app.pending.contains(&tg::media_key(media)) {
                    return button(text(if *gif { tr("⬇ Mostra la GIF") } else { tr("⬇ Mostra la foto") }).size(13))
                        .on_press(Msg::LoadPreview(r.id))
                        .padding([10, 16])
                        .style(style::flat(true))
                        .into();
                }
                text(if *gif { tr("GIF in caricamento…") } else { tr("🖼 foto in caricamento…") })
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
                button(text(tr("Salva")).size(13)).on_press(Msg::Save(r.id)).style(style::primary),
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
            let label = if *voice { tr("Messaggio vocale").to_string() } else { name.clone() };
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
fn actions<'a>(g: &Gui, r: &'a HistoryRow) -> Element<'a, Msg> {
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
    bar = bar.push(space().width(8)).push(act(tr("↱ Rispondi"), Msg::Reply(r.id)));
    if r.outgoing && matches!(r.kind, RowKind::Text(_)) {
        bar = bar.push(act(tr("✎ Modifica"), Msg::Edit(r.id)));
    }
    if matches!(&r.kind, RowKind::Text(t) if !t.trim().is_empty()) {
        let shown = g
            .app
            .open
            .and_then(|p| g.app.translations.get(&(p, r.id)))
            .is_some_and(|s| matches!(s, TrState::Shown(_)));
        bar = bar.push(act(if shown { tr("🌐 Originale") } else { tr("🌐 Traduci") }, Msg::Translate(r.id)));
    }
    let starred = g.app.open.is_some_and(|peer| g.app.is_starred(peer, r.id));
    bar = bar.push(act(if starred { tr("☆ Non importante") } else { tr("⭐ Importante") }, Msg::Star(r.id)));
    bar = bar.push(act(tr("↪ Inoltra"), Msg::Forward(r.id)));
    bar = bar.push(act(tr("⧉ Copia"), Msg::Copy(r.id)));
    if r.outgoing {
        bar = bar.push(
            button(text(tr("🗑 Elimina")).size(13))
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
            tr("Eliminare il messaggio per tutti?").into(),
            Some(
                button(text(tr("Elimina")).size(13))
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
            .unwrap_or_else(|| tr("messaggio").into());
        let what: String = what.chars().take(100).collect();
        col = col.push(bar(trf!("↱ Rispondi a {}", what), None));
    }
    if app.editing.is_some() {
        col = col.push(bar(tr("✎ Modifica del messaggio — Invio salva").into(), None));
    }
    if let Some(e) = attachment(app) {
        col = col.push(e);
    }
    if let Some(name) = &app.uploading {
        col = col.push(
            container(
                text(trf!("⬆ Invio di «{}» in corso…", name)).size(13).color(style::p().muted),
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
        (Some(p), _) => (std::path::PathBuf::from(p), tr("Immagine dagli appunti").to_string()),
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
        text(trf!("{} · scrivi una didascalia e premi Invio", size)).size(12).color(p.muted),
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
    let placeholder = if attached { tr("Didascalia…") } else { tr("Scrivi un messaggio…") };
    if style::wa() {
        // WhatsApp: emoji e allegato a sinistra, campo tondo, invio verde tondo
        return container(
            row![
                button(text("😊").size(20)).on_press(Msg::Stickers).style(style::flat(app.show_sticker)),
                button(text("📎").size(20)).on_press(Msg::Attach).style(style::flat(false)),
                text_input(placeholder, &app.compose)
                    .style(style::input)
                    .id(COMPOSE)
                    .on_input(Msg::Compose)
                    .on_paste(Msg::Pasted)
                    .on_submit(Msg::Submit)
                    .padding([10, 16])
                    .width(Fill),
                button(container(text("➤").size(18)).center(24))
                    .on_press(Msg::Submit)
                    .padding(8)
                    .style(style::send_round),
            ]
            .spacing(8)
            .align_y(Center),
        )
        .padding([8, 12])
        .style(style::fill(style::p().header))
        .into();
    }
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
            button(text(tr("Invia"))).on_press(Msg::Submit).padding([8, 16]).style(style::primary),
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
        tab(tr("⭐ Preferiti"), here == Some(tg::FAVED_SET), Msg::OpenSet(tg::FAVED_SET)),
        tab(tr("🕘 Recenti"), here == Some(tg::RECENT_SET), Msg::OpenSet(tg::RECENT_SET)),
        tab(tr("☰ Set"), here.is_none(), Msg::SetsBack),
        text(title).font(BOLD).wrapping(text::Wrapping::None),
    ]
    .spacing(6)
    .align_y(Center);
    let inner: Element<'_, Msg> = match here {
        None => match &app.sets {
            None => text(tr("carico i set…")).color(style::p().muted).into(),
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
                None => text(tr("scarico gli sticker…")).color(style::p().muted).into(),
                Some(docs) if docs.is_empty() => text(tr("niente qui: aggiungi sticker ai preferiti da Telegram"))
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
    let today = jiff::Zoned::now().date();
    if d == today {
        return tr("Oggi").into();
    }
    if today.yesterday().ok() == Some(d) {
        return tr("Ieri").into();
    }
    let mese = rondine::i18n::months()[(d.month() - 1) as usize];
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
            text(trf!("Invito a «{}» · {} membri", title, members)).color(p.link).width(Fill),
            button(text(tr("Unisciti")).size(13)).on_press(Msg::Join).style(style::primary),
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
    let (title, body) = match g.settings_page {
        SettingsPage::Main => (tr("Impostazioni"), settings_main(g)),
        SettingsPage::Account => (tr("Il mio account"), settings_account(g)),
        SettingsPage::Appearance => (tr("Aspetto"), settings_appearance(g)),
        SettingsPage::Notifications => (tr("Notifiche"), settings_notifications(g)),
        SettingsPage::Media => (tr("Immagini e media"), settings_media(g)),
        SettingsPage::Storage => (tr("Dati e archiviazione"), settings_storage(g)),
        SettingsPage::Devices => (tr("Dispositivi"), settings_devices(g)),
        SettingsPage::Links => ("Link", settings_links(g)),
        SettingsPage::Translation => (tr("Traduzione"), settings_translation(g)),
    };
    let mut head = Row::new().spacing(8).align_y(Center);
    if g.settings_page != SettingsPage::Main {
        head = head.push(
            button(text("←").size(18))
                .on_press(Msg::Page(SettingsPage::Main))
                .padding([2, 10])
                .style(style::flat(false)),
        );
    }
    head = head
        .push(text(title).size(22).font(BOLD).width(Fill))
        .push(button(text("✕")).on_press(Msg::ToggleSettings).style(style::flat(false)));
    let mut col = column![head, body].spacing(18).max_width(620);
    if !g.app.status.is_empty() {
        col = col.push(text(g.app.status.as_str()).size(12).color(p.muted));
    }
    container(scrollable(container(col).padding(28)).height(Fill))
        .width(Fill)
        .height(Fill)
        .style(style::fill(p.bg))
        .into()
}

fn section<'a>(title: &'static str) -> Element<'a, Msg> {
    text(title).size(13).font(BOLD).color(style::p().link).into()
}

/// un pulsante di scelta fra alternative (tema, misura, tetto)
fn choice<'a>(label: String, on: bool, m: Msg) -> Element<'a, Msg> {
    button(text(label)).on_press(m).padding([8, 14]).style(style::flat(on)).into()
}

/// la mia foto profilo (o l'iniziale)
fn my_photo<'a>(g: &Gui, size: f32) -> Element<'a, Msg> {
    match g.app.profile.as_ref().and_then(|p| p.photo.as_ref()) {
        Some(path) => image(image::Handle::from_path(path))
            .width(size)
            .height(size)
            .content_fit(iced::ContentFit::Cover)
            .border_radius(size / 2.0)
            .into(),
        None => initial(&g.app.me, size),
    }
}

/// La prima pagina: il mio profilo in alto e l'elenco delle sezioni.
fn settings_main(g: &Gui) -> Element<'_, Msg> {
    let p = style::p();
    let prof = g.app.profile.as_ref();
    let mut who = column![text(g.app.me.as_str()).size(18).font(BOLD)].spacing(2);
    if let Some(pr) = prof {
        if !pr.phone.is_empty() {
            who = who.push(text(pr.phone.as_str()).size(13).color(p.muted));
        }
        if !pr.username.is_empty() {
            who = who.push(text(format!("@{}", pr.username)).size(13).color(p.link));
        }
    }
    let card = button(row![my_photo(g, 64.0), who.width(Fill), text("›").size(20).color(p.muted)].spacing(14).align_y(Center))
        .on_press(Msg::Page(SettingsPage::Account))
        .width(Fill)
        .padding(12)
        .style(style::flat(false));
    let entry = |icon: &'static str, label: &'static str, sub: &'static str, page: SettingsPage| -> Element<'_, Msg> {
        button(
            row![
                text(icon).size(20).width(30),
                column![text(label), text(sub).size(12).color(p.muted)].spacing(1).width(Fill),
                text("›").size(18).color(p.muted),
            ]
            .spacing(10)
            .align_y(Center),
        )
        .on_press(Msg::Page(page))
        .width(Fill)
        .padding([9, 12])
        .style(style::flat(false))
        .into()
    };
    let list = column![
        entry("👤", tr("Il mio account"), tr("nome, username, bio e foto"), SettingsPage::Account),
        entry("🎨", tr("Aspetto"), tr("lingua, tema, dimensione, accelerazione GPU"), SettingsPage::Appearance),
        entry("🔔", tr("Notifiche"), tr("avvisi dei messaggi nuovi"), SettingsPage::Notifications),
        entry("🖼", tr("Immagini e media"), tr("download automatico, miniature, misura delle foto"), SettingsPage::Media),
        entry("💾", tr("Dati e archiviazione"), tr("cache e spazio su disco"), SettingsPage::Storage),
        entry("💻", tr("Dispositivi"), tr("sessioni attive dell'account"), SettingsPage::Devices),
        entry("🌐", tr("Traduzione"), tr("traduzione dei messaggi con DeepL, Groq e altri"), SettingsPage::Translation),
        entry("🔗", "Link", tr("t.me e tg:// dentro Rondine"), SettingsPage::Links),
    ]
    .spacing(2);
    column![
        container(card).style(style::pill(p.sidebar)),
        container(list).padding(4).style(style::pill(p.sidebar)),
        text(format!("Rondine {}", env!("CARGO_PKG_VERSION"))).size(12).color(p.muted),
    ]
    .spacing(16)
    .into()
}

fn form_field<'a>(
    label: &'static str,
    placeholder: &'static str,
    value: &'a str,
    on: fn(String) -> Msg,
) -> Element<'a, Msg> {
    column![
        text(label).size(12).color(style::p().muted),
        text_input(placeholder, value).on_input(on).on_submit(Msg::PfSave).style(style::input).padding(9),
    ]
    .spacing(4)
    .into()
}

/// Profilo: foto, nome, cognome, username e bio; il numero solo in lettura.
fn settings_account(g: &Gui) -> Element<'_, Msg> {
    let p = style::p();
    let Some(saved) = g.app.profile.as_ref() else {
        return text(tr("carico il profilo…")).color(p.muted).into();
    };
    let pf = &g.pf;
    let changed = saved.first != pf.first
        || saved.last != pf.last
        || saved.username != pf.username
        || saved.about != pf.about;
    let photo = column![
        my_photo(g, 96.0),
        button(text(tr("Cambia foto")).size(13)).on_press(Msg::PfPhoto).style(style::flat(true)),
    ]
    .spacing(8)
    .align_x(Center)
    .width(Fill);
    let mut bio = column![form_field(tr("Bio"), tr("Qualche parola su di te"), &pf.about, Msg::PfAbout)].spacing(2);
    bio = bio.push(
        text(trf!("{}/70 · la vedono tutti nel tuo profilo", pf.about.chars().count()))
            .size(11)
            .color(p.muted),
    );
    let mut user = column![form_field(tr("Username"), tr("username"), &pf.username, Msg::PfUser)].spacing(2);
    user = user.push(
        text(if pf.username.is_empty() {
            tr("senza username ti trova solo chi ha il tuo numero").to_string()
        } else {
            trf!("link: t.me/{}", pf.username)
        })
        .size(11)
        .color(p.muted),
    );
    let phone = if saved.phone.is_empty() { tr("nascosto").to_string() } else { saved.phone.clone() };
    column![
        photo,
        row![
            container(form_field(tr("Nome"), tr("Nome (obbligatorio)"), &pf.first, Msg::PfFirst)).width(Fill),
            container(form_field(tr("Cognome"), tr("Cognome"), &pf.last, Msg::PfLast)).width(Fill),
        ]
        .spacing(12),
        user,
        bio,
        info_field(text(phone).into(), tr("telefono · si cambia dall'app ufficiale")),
        row![
            space().width(Fill),
            button(text(tr("Salva")))
                .on_press_maybe(changed.then_some(Msg::PfSave))
                .padding([8, 18])
                .style(style::primary),
        ],
    ]
    .spacing(16)
    .into()
}

fn settings_appearance(g: &Gui) -> Element<'_, Msg> {
    let p = style::p();
    let s = &g.settings;
    let omarchy_label = match style::omarchy_name() {
        Some(n) => trf!("Tema Omarchy · {}", n),
        None => tr("Tema Omarchy").into(),
    };
    column![
        section(tr("LINGUA")),
        Row::with_children(
            std::iter::once(choice(
                tr("Automatica (sistema)").to_string(),
                s.lang.0.is_none(),
                Msg::Lang(None),
            ))
            .chain(rondine::i18n::Lang::ALL.into_iter().map(|l| {
                choice(l.name().to_string(), s.lang.0 == Some(l), Msg::Lang(Some(l)))
            })),
        )
        .spacing(8)
        .wrap(),
        section(tr("STILE")),
        row![
            choice("Telegram".into(), s.style == UiStyle::Telegram, Msg::SetStyle(UiStyle::Telegram)),
            choice("WhatsApp".into(), s.style == UiStyle::WhatsApp, Msg::SetStyle(UiStyle::WhatsApp)),
        ]
        .spacing(8),
        section(tr("TEMA")),
        row![
            choice(omarchy_label, s.active_theme() == ThemeChoice::Omarchy, Msg::SetTheme(ThemeChoice::Omarchy)),
            choice(tr("Rondine scuro").into(), s.active_theme() == ThemeChoice::Rondine, Msg::SetTheme(ThemeChoice::Rondine)),
            choice(tr("Verde WhatsApp").into(), s.active_theme() == ThemeChoice::WhatsApp, Msg::SetTheme(ThemeChoice::WhatsApp)),
        ]
        .spacing(8)
        .wrap(),
        text(match s.style {
            UiStyle::Telegram => tr("Il tema scelto vale solo per lo stile Telegram."),
            UiStyle::WhatsApp => tr("Il tema scelto vale solo per lo stile WhatsApp."),
        })
        .size(12)
        .color(p.muted),
        text(tr("Con il tema Omarchy i colori seguono `omarchy theme set` mentre Rondine è aperta."))
            .size(12)
            .color(p.muted),
        section(tr("DIMENSIONE")),
        row![
            text(trf!("Interfaccia  {}%", format!("{:.0}", s.scale * 100.0))).width(170),
            iced::widget::slider(0.7..=1.6, s.scale, Msg::Scale).step(0.05_f32),
        ]
        .spacing(12)
        .align_y(Center),
        section(tr("DISEGNO")),
        iced::widget::toggler(s.gpu)
            .label(tr("Accelerazione GPU: scorrimento fluido, ~115 MB invece di ~80 (al prossimo avvio)"))
            .on_toggle(Msg::Gpu),
    ]
    .spacing(14)
    .into()
}

fn settings_notifications(g: &Gui) -> Element<'_, Msg> {
    let s = &g.settings;
    column![
        iced::widget::toggler(s.notify)
            .label(tr("Notifiche dei messaggi nuovi (non per le chat silenziate)"))
            .on_toggle(Msg::Notify),
        iced::widget::toggler(s.notify_text)
            .label(tr("Mostra il testo del messaggio"))
            .on_toggle(Msg::NotifyText),
        text(tr("Una chat letta da un altro dispositivo toglie da sola la sua notifica."))
            .size(12)
            .color(style::p().muted),
    ]
    .spacing(14)
    .into()
}

/// Download automatico, miniature e misura delle foto in chat.
fn settings_media(g: &Gui) -> Element<'_, Msg> {
    let p = style::p();
    let s = &g.settings;
    let sizes = Row::with_children(crate::settings::PHOTO_SIZES.iter().map(|&(px, label)| {
        choice(format!("{label} · {px}px"), s.photo_px == px, Msg::PhotoSize(px))
    }))
    .spacing(8)
    .wrap();
    column![
        section(tr("DOWNLOAD AUTOMATICO")),
        iced::widget::toggler(s.auto_photos)
            .label(tr("Scarica da solo le foto e le GIF delle chat"))
            .on_toggle(Msg::AutoPhotos),
        text(tr("Spento: al posto della foto compare «Mostra la foto», e si scarica con un click."))
            .size(12)
            .color(p.muted),
        iced::widget::toggler(s.video_thumbs)
            .label(tr("Miniature dei video"))
            .on_toggle(Msg::VideoThumbs),
        iced::widget::toggler(s.avatars)
            .label(tr("Foto profilo nella lista chat"))
            .on_toggle(Msg::Avatars),
        section(tr("MISURA DELLE FOTO IN CHAT")),
        sizes,
        text(tr("Gli sticker restano sempre visibili; i media condivisi nella scheda della chat si scaricano quando la apri."))
            .size(12)
            .color(p.muted),
    ]
    .spacing(14)
    .into()
}

/// Spazio occupato dalla cache, svuotamento e tetto.
fn settings_storage(g: &Gui) -> Element<'_, Msg> {
    let p = style::p();
    let kinds = [
        (CacheKind::Media, tr("Foto, sticker e miniature")),
        (CacheKind::Avatars, tr("Foto profilo")),
        (CacheKind::Temp, tr("File temporanei (audio, video, appunti)")),
    ];
    let mut usage = Column::new().spacing(10);
    let mut total = 0u64;
    for (i, (kind, label)) in kinds.into_iter().enumerate() {
        let (n, bytes) = g.cache_usage[i];
        total += bytes;
        usage = usage.push(
            row![
                column![text(label), text(trf!("{} file · {}", n, tg::fmt_size(bytes as f64))).size(12).color(p.muted)]
                    .spacing(1)
                    .width(Fill),
                button(text(tr("Svuota")).size(13))
                    .on_press_maybe((n > 0).then_some(Msg::AskClear(kind)))
                    .padding([5, 12])
                    .style(style::flat(true)),
            ]
            .align_y(Center),
        );
    }
    let limits = Row::with_children(crate::settings::CACHE_LIMITS.iter().map(|&mb| {
        let label = if mb >= 1024 { format!("{} GB", mb / 1024) } else { format!("{mb} MB") };
        choice(label, g.settings.cache_mb == mb, Msg::CacheLimit(mb))
    }))
    .spacing(8)
    .wrap();
    column![
        section(tr("SPAZIO USATO")),
        container(usage).padding(14).style(style::pill(p.sidebar)),
        text(trf!("Totale: {}", tg::fmt_size(total as f64))).font(BOLD),
        text(tr("La cronologia non resta sul disco: si salvano solo anteprime e foto profilo."))
            .size(12)
            .color(p.muted),
        section(tr("TETTO DELLA CACHE")),
        limits,
        row![
            text(tr("A ogni avvio Rondine toglie i file più vecchi oltre il tetto."))
                .size(12)
                .color(p.muted)
                .width(Fill),
            button(text(tr("Sfoltisci ora")).size(13)).on_press(Msg::PruneNow).style(style::flat(true)),
        ]
        .spacing(8)
        .align_y(Center),
    ]
    .spacing(14)
    .into()
}

/// Le sessioni attive: questa in cima, le altre chiudibili una per una.
fn settings_devices(g: &Gui) -> Element<'_, Msg> {
    let p = style::p();
    let Some(list) = &g.app.sessions else {
        return text(tr("carico i dispositivi…")).color(p.muted).into();
    };
    let mut col = Column::new().spacing(10);
    for (i, s) in list.iter().enumerate() {
        if i == 1 {
            col = col.push(section(tr("ALTRE SESSIONI")));
        } else if i == 0 && s.current {
            col = col.push(section(tr("QUESTO DISPOSITIVO")));
        }
        let mut top = row![text(if s.device.is_empty() { tr("Dispositivo") } else { s.device.as_str() }).font(BOLD).width(Fill)]
            .spacing(8)
            .align_y(Center);
        if !s.current {
            top = top.push(
                button(text(tr("Disconnetti")).size(12))
                    .on_press(Msg::AskEndSession(Some(s.hash)))
                    .padding([4, 10])
                    .style(style::flat(true)),
            );
        }
        let place = [s.ip.as_str(), s.place.as_str()]
            .into_iter()
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let when = if s.current {
            tr("online").to_string()
        } else {
            trf!("ultima attività: {}", list_time(g, s.active))
        };
        col = col.push(
            container(
                column![
                    top,
                    text(s.app.as_str()).size(13),
                    text(place).size(12).color(p.muted),
                    text(when).size(12).color(if s.current { p.link } else { p.muted }),
                ]
                .spacing(2),
            )
            .padding(12)
            .width(Fill)
            .style(style::pill(p.sidebar)),
        );
    }
    if list.iter().filter(|s| !s.current).count() > 0 {
        col = col.push(
            button(text(tr("Chiudi tutte le altre sessioni")))
                .on_press(Msg::AskEndSession(None))
                .padding([8, 16])
                .style(style::danger),
        );
    }
    col.into()
}

/// Servizio di traduzione: DeepL o compatibile OpenAI, chiavi, lingua, prova.
fn settings_translation<'a>(g: &'a Gui) -> Element<'a, Msg> {
    use rondine::translate::{Provider, PRESETS, TARGETS};
    let p = style::p();
    let c = &g.app.translate;
    let mut col = column![
        section(tr("SERVIZIO")),
        row![
            choice("DeepL".into(), c.provider == Provider::DeepL, Msg::TrProvider(Provider::DeepL)),
            choice(tr("Compatibile OpenAI (Groq…)").into(), c.provider == Provider::OpenAi, Msg::TrProvider(Provider::OpenAi)),
        ]
        .spacing(8),
    ]
    .spacing(14);
    let field = |label: &'static str, ph: &'static str, value: &'a str, secret: bool, on: fn(String) -> Msg| -> Element<'a, Msg> {
        column![
            text(label).size(12).color(p.muted),
            text_input(ph, value).secure(secret).on_input(on).style(style::input).padding(9),
        ]
        .spacing(4)
        .into()
    };
    match c.provider {
        Provider::DeepL => {
            col = col
                .push(field(tr("Chiave API DeepL"), "xxxxxxxx-xxxx-…:fx", &c.deepl_key, true, Msg::TrDeeplKey))
                .push(
                    text(tr("La chiave si crea gratis su deepl.com (piano «DeepL API Free»); quelle gratuite finiscono con «:fx»."))
                        .size(12)
                        .color(p.muted),
                );
        }
        Provider::OpenAi => {
            let presets = Row::with_children(PRESETS.iter().enumerate().map(|(i, (name, url, _))| {
                choice(name.to_string(), c.oa_url == *url, Msg::TrPreset(i))
            }))
            .spacing(8);
            col = col
                .push(presets)
                .push(field(tr("Indirizzo"), "https://api.groq.com/openai/v1", &c.oa_url, false, Msg::TrUrl))
                .push(field(tr("Chiave API"), "gsk_…", &c.oa_key, true, Msg::TrKey))
                .push(field(tr("Modello"), "llama-3.3-70b-versatile", &c.oa_model, false, Msg::TrModel))
                .push(
                    text(tr("La chiave di Groq si crea su console.groq.com; con Ollama in locale non serve."))
                        .size(12)
                        .color(p.muted),
                );
        }
    }
    let ui_lang = rondine::i18n::lang();
    let targets = Row::with_children(
        std::iter::once(choice(
            trf!("Come l'interfaccia ({})", ui_lang.name()),
            c.target.is_none(),
            Msg::TrTarget(None),
        ))
        .chain(TARGETS.iter().map(|(code, name)| {
            choice(name.to_string(), c.target.as_deref() == Some(*code), Msg::TrTarget(Some(code.to_string())))
        })),
    )
    .spacing(8)
    .wrap();
    let mut test = row![
        button(text(tr("Prova")))
            .on_press_maybe(c.ready().then_some(Msg::TrTest))
            .padding([8, 16])
            .style(style::primary),
    ]
    .spacing(12)
    .align_y(Center);
    match &g.app.tr_test {
        Some(Ok(t)) => test = test.push(text(format!("✓ {t}")).color(p.link).width(Fill)),
        Some(Err(e)) => test = test.push(text(format!("✕ {e}")).color(p.danger).width(Fill)),
        None if !c.ready() => test = test.push(text(tr("inserisci prima la chiave")).size(12).color(p.muted)),
        None => {}
    }
    col.push(section(tr("TRADURRE IN")))
        .push(targets)
        .push(test)
        .push(section(tr("PRIVACY")))
        .push(
            text(tr("I messaggi da tradurre vengono inviati al servizio scelto, solo quando premi «🌐 Traduci» o nelle chat a traduzione automatica (si attiva dalla scheda della chat). La chiave resta in ~/.config/rondine/translate.conf, leggibile solo da te."))
                .size(12)
                .color(p.muted),
        )
        .into()
}

fn settings_links(g: &Gui) -> Element<'_, Msg> {
    let p = style::p();
    let handler_is_us = g.tg_handler == "rondine-gui.desktop";
    let handler_label = if g.tg_handler.is_empty() {
        tr("nessuna app").to_string()
    } else {
        g.tg_handler.trim_end_matches(".desktop").to_string()
    };
    let mut handler_row = row![text(trf!("Oggi li apre: {}", handler_label)).color(p.muted).width(Fill)]
        .spacing(8)
        .align_y(Center);
    if !handler_is_us {
        handler_row = handler_row.push(
            button(text(tr("Usa Rondine")).size(13))
                .on_press(Msg::TgHandler)
                .style(style::primary),
        );
    }
    column![
        iced::widget::toggler(g.settings.links_in_app)
            .label(tr("Apri i link t.me e tg:// dentro Rondine"))
            .on_toggle(Msg::LinksInApp),
        text(tr("Link tg:// da browser e altre app")).color(p.text),
        handler_row,
    ]
    .spacing(14)
    .into()
}

// ------------------------------------------------- messaggi importanti

/// La lista dei messaggi con la stella, dal più recente; click: apre la
/// chat su quel messaggio.
fn starred_view(g: &Gui) -> Element<'_, Msg> {
    let p = style::p();
    let mut list: Vec<&rondine::starred::Star> = g.app.starred.iter().collect();
    list.sort_by_key(|s| std::cmp::Reverse(s.date));
    let head = row![
        text(tr("⭐ Messaggi importanti")).size(20).font(BOLD).width(Fill),
        button(text("✕")).on_press(Msg::ShowStarred(false)).style(style::flat(false)),
    ]
    .align_y(Center);
    let body: Element<'_, Msg> = if list.is_empty() {
        text(tr("Nessun messaggio importante: aprine uno e premi «⭐ Importante» nelle sue azioni."))
            .color(p.muted)
            .into()
    } else {
        let items = list.into_iter().map(|s| {
            let when = local(g, s.date).map(|z| z.strftime("%d/%m/%y %H:%M").to_string()).unwrap_or_default();
            let card = column![
                row![
                    text(if s.from.is_empty() || s.from == s.chat_name { s.chat_name.clone() } else { format!("{} · {}", s.chat_name, s.from) }).size(12).font(BOLD).color(style::name_color(&s.chat_name)).width(Fill),
                    text(when).size(11).color(p.muted),
                ]
                .spacing(8),
                text(ellipsize(&s.text, 300)).wrapping(text::Wrapping::WordOrGlyph),
            ]
            .spacing(4)
            .width(Fill);
            row![
                button(container(card).padding([8, 12]).style(style::bubble(false, false, true)))
                    .on_press(Msg::OpenStarred(s.chat, s.id))
                    .padding(0)
                    .width(Fill)
                    .style(|_: &iced::Theme, _| button::Style::default()),
                button(text("✕").size(12)).on_press(Msg::Unstar(s.chat, s.id)).style(style::flat(false)),
            ]
            .spacing(6)
            .align_y(Center)
            .into()
        });
        scrollable(Column::with_children(items).spacing(8).padding(iced::Padding::ZERO.right(12))).height(Fill).into()
    };
    container(column![head, body].spacing(16).max_width(720))
        .padding(24)
        .center_x(Fill)
        .height(Fill)
        .style(style::fill(p.bg))
        .into()
}

// ------------------------------------------------------ scheda della chat

/// La scheda della chat aperta: foto, nome, bio/descrizione, contatti,
/// blocco ed eliminazione, poi media, file, link e audio condivisi.
/// `side`: accanto alla cronologia (larghezza fissa) o al suo posto.
fn info_panel(g: &Gui, side: bool) -> Element<'_, Msg> {
    let app = &g.app;
    let p = style::p();
    let Some(chat) = app.current_chat() else {
        return space().into();
    };
    let info = app.peer_info.get(&chat.peer_id);
    let user = !chat.is_group;
    let title = if user {
        tr("Info utente")
    } else if chat.broadcast {
        tr("Info canale")
    } else {
        tr("Info gruppo")
    };
    let top = row![
        text(title).size(16).font(BOLD).width(Fill),
        button(text("✕")).on_press(Msg::ToggleInfo).style(style::flat(false)),
    ]
    .align_y(Center);
    let face = column![
        avatar(app, chat, 96.0),
        text(chat.name.as_str()).size(20).font(BOLD).align_x(Center),
        subtitle(g, chat),
    ]
    .spacing(6)
    .align_x(Center)
    .width(Fill);

    let mut details = Column::new().spacing(12);
    match info {
        None => {
            details = details.push(text(tr("carico le informazioni…")).size(12).color(p.muted));
        }
        Some(i) => {
            if let Some(phone) = &i.phone {
                details = details.push(info_field(text(phone.as_str()).into(), tr("telefono")));
            }
            if let Some(u) = &i.username {
                let url = format!("https://t.me/{u}");
                details = details.push(info_field(
                    button(text(format!("@{u}")).color(p.link))
                        .on_press(Msg::Link(url))
                        .padding(0)
                        .style(|_: &iced::Theme, _| button::Style::default())
                        .into(),
                    tr("username"),
                ));
            }
            if !i.about.is_empty() {
                let label = if user { tr("bio") } else { tr("descrizione") };
                details = details.push(info_field(linked(&i.about), label));
            }
            if i.blocked {
                details = details.push(text(tr("🚫 Hai bloccato questo utente")).size(13).color(p.danger));
            }
        }
    }

    // azioni: blocco (solo utenti veri, non i Messaggi salvati) ed eliminazione
    let act = |label: String, color: iced::Color, m: Msg| {
        button(text(label).color(color))
            .on_press(m)
            .width(Fill)
            .padding([8, 12])
            .style(style::flat(false))
    };
    let mut acts = Column::new().spacing(2);
    if let Some(i) = info.filter(|i| user && !i.me) {
        let (label, m) = if i.blocked {
            (tr("✅ Sblocca utente"), Msg::AskBlock(false))
        } else {
            (tr("🚫 Blocca utente"), Msg::AskBlock(true))
        };
        acts = acts.push(act(label.into(), p.text, m));
    }
    let del_label = if user {
        tr("🗑 Elimina chat")
    } else if chat.broadcast {
        tr("🚪 Abbandona canale")
    } else {
        tr("🚪 Esci dal gruppo")
    };
    acts = acts.push(act(del_label.into(), p.danger, Msg::AskDeleteChat));
    let auto_tr = iced::widget::toggler(app.translate.is_auto(chat.peer_id))
        .label(tr("🌐 Traduci da solo i messaggi di questa chat"))
        .on_toggle(Msg::AutoTranslate);
    // messaggi effimeri: come i «messaggi temporanei» di WhatsApp
    let ttl_now = info.map_or(0, |i| i.ttl);
    let ttl_choice = |label: &'static str, secs: i32| -> Element<'_, Msg> {
        button(text(label).size(12))
            .on_press(Msg::ChatDo(chat.peer_id, tg::ChatAction::Ttl(secs)))
            .padding([4, 10])
            .style(style::chip(ttl_now == secs))
            .into()
    };
    let ephemeral = column![
        text(tr("⏱ Messaggi effimeri")).size(14),
        row![
            ttl_choice(tr("Spenti"), 0),
            ttl_choice(tr("24 ore"), 86_400),
            ttl_choice(tr("7 giorni"), 604_800),
            ttl_choice(tr("90 giorni"), 7_776_000),
        ]
        .spacing(6)
        .wrap(),
        text(tr("I nuovi messaggi spariscono per tutti dopo il tempo scelto.")).size(11).color(p.muted),
    ]
    .spacing(6);

    let tabs = Row::with_children(tg::SharedTab::ALL.iter().map(|&t| {
        button(text(t.label()).size(13))
            .on_press(Msg::InfoTab(t))
            .padding([4, 10])
            .style(style::flat(g.info_tab == t))
            .into()
    }))
    .spacing(4)
    .wrap();
    let shared: Element<'_, Msg> = match app.shared.get(&(chat.peer_id, g.info_tab)) {
        None => text(tr("carico…")).size(12).color(p.muted).into(),
        Some(items) if items.is_empty() => {
            let what = match g.info_tab {
                tg::SharedTab::Media => tr("Nessuna foto o video"),
                tg::SharedTab::Files => tr("Nessun file"),
                tg::SharedTab::Links => tr("Nessun link"),
                tg::SharedTab::Audio => tr("Nessun brano"),
                tg::SharedTab::Voice => tr("Nessun messaggio vocale"),
            };
            text(what).size(12).color(p.muted).into()
        }
        Some(items) => match g.info_tab {
            tg::SharedTab::Media => shared_grid(app, items),
            tg::SharedTab::Links => shared_links(g, items),
            _ => Column::with_children(items.iter().map(|it| shared_row(g, &it.row)))
                .spacing(4)
                .into(),
        },
    };

    let sep = || container(space()).width(Fill).height(1).style(style::fill(p.selected));
    let body = column![top, face, details, sep(), acts, auto_tr, ephemeral, sep(), tabs, shared]
        .spacing(14)
        .padding(iced::Padding::new(18.0).right(24.0));
    container(scrollable(body).height(Fill))
        .width(if side { Length::Fixed(INFO_W) } else { Length::Fill })
        .height(Fill)
        .style(style::fill(p.sidebar))
        .into()
}

/// un dato della scheda con la sua etichetta sotto, come Telegram
fn info_field<'a>(value: Element<'a, Msg>, label: &'static str) -> Element<'a, Msg> {
    column![value, text(label).size(11).color(style::p().muted)]
        .spacing(2)
        .into()
}

/// data breve sotto file e link condivisi
fn short_date(g: &Gui, secs: i64) -> String {
    local(g, secs)
        .map(|z| z.strftime("%d/%m/%y").to_string())
        .unwrap_or_default()
}

/// foto e video in una griglia di miniature quadrate (click: visore o player)
fn shared_grid<'a>(app: &'a App, items: &'a [tg::SharedItem]) -> Element<'a, Msg> {
    let p = style::p();
    let mut rows = Column::new().spacing(3);
    for chunk in items.chunks(3) {
        let mut line = Row::new().spacing(3);
        for it in chunk {
            let r = &it.row;
            let (media, video) = match &r.kind {
                RowKind::Photo { media, .. } => (media, false),
                RowKind::Video { media, .. } => (media, true),
                _ => continue,
            };
            let thumb: Element<'a, Msg> = match app.previews.get(&tg::media_key(media)) {
                Some(path) => image(image::Handle::from_path(path))
                    .width(SHARED_PX)
                    .height(SHARED_PX)
                    .content_fit(iced::ContentFit::Cover)
                    .border_radius(4)
                    .into(),
                None => container(text(if video { "🎥" } else { "🖼" }).size(28))
                    .center(SHARED_PX)
                    .style(style::pill(p.bubble_in))
                    .into(),
            };
            let cell: Element<'a, Msg> = if video {
                let play = container(text("▶").size(16).color(iced::Color::WHITE))
                    .padding([4, 9])
                    .style(style::pill(iced::Color::from_rgba(0.0, 0.0, 0.0, 0.55)));
                iced::widget::stack![thumb, container(play).center(SHARED_PX)].into()
            } else {
                thumb
            };
            let msg = if video { Msg::Play(r.id) } else { Msg::View(r.id) };
            line = line.push(
                mouse_area(cell)
                    .on_press(msg)
                    .interaction(iced::mouse::Interaction::Pointer),
            );
        }
        rows = rows.push(line);
    }
    rows.into()
}

/// file, brani e vocali: icona, nome, dettagli e il pulsante giusto
fn shared_row<'a>(g: &'a Gui, r: &'a HistoryRow) -> Element<'a, Msg> {
    let p = style::p();
    let when = short_date(g, r.date);
    let (icon, name, extra, action): (&str, String, String, Option<(&str, Msg)>) = match &r.kind {
        RowKind::Doc { name, size, .. } => (
            "📄",
            name.clone(),
            size.map(|s| tg::fmt_size(s as f64)).unwrap_or_default(),
            Some((tr("Salva"), Msg::Save(r.id))),
        ),
        RowKind::Audio { voice: true, seconds, .. } => (
            "🎤",
            author(&g.app, r).to_string(),
            tg::fmt_secs(*seconds),
            Some(("▶", Msg::Play(r.id))),
        ),
        RowKind::Audio { name, seconds, .. } => {
            ("🎵", name.clone(), tg::fmt_secs(*seconds), Some(("▶", Msg::Play(r.id))))
        }
        RowKind::Video { round, seconds, .. } => (
            if *round { "📹" } else { "🎥" },
            if *round { author(&g.app, r).to_string() } else { tr("Video").into() },
            tg::fmt_secs(*seconds),
            Some(("▶", Msg::Play(r.id))),
        ),
        RowKind::Photo { .. } => ("🖼", tr("Foto").into(), String::new(), Some((tr("Apri"), Msg::View(r.id)))),
        other => ("💬", other.summary(), String::new(), None),
    };
    let sub = [extra, when].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
    let mut line = row![
        text(icon).size(22),
        column![
            text(ellipsize(&name, 34)).wrapping(text::Wrapping::None),
            text(sub).size(11).color(p.muted),
        ]
        .spacing(2)
        .width(Fill),
    ]
    .spacing(10)
    .align_y(Center);
    if let Some((label, m)) = action {
        line = line.push(button(text(label).size(12)).on_press(m).padding([4, 10]).style(style::primary));
    }
    container(line).padding([4, 0]).into()
}

/// i link condivisi: l'indirizzo cliccabile e il messaggio che lo contiene
fn shared_links<'a>(g: &'a Gui, items: &'a [tg::SharedItem]) -> Element<'a, Msg> {
    let p = style::p();
    let mut col = Column::new().spacing(10);
    for it in items {
        let mut entry = Column::new().spacing(2);
        for url in &it.links {
            entry = entry.push(
                button(text(ellipsize(url, 46)).size(13).color(p.link).wrapping(text::Wrapping::None))
                    .on_press(Msg::Link(url.clone()))
                    .padding(0)
                    .style(|_: &iced::Theme, _| button::Style::default()),
            );
        }
        let summary = it.row.kind.summary();
        let mut sub = short_date(g, it.row.date);
        if !summary.is_empty() && !it.links.iter().any(|u| summary.trim() == u.as_str()) {
            sub = format!("{sub} · {}", ellipsize(&summary, 60));
        }
        entry = entry.push(text(sub).size(11).color(p.muted));
        col = col.push(entry);
    }
    col.into()
}

/// Il popup di conferma per blocco ed eliminazione: velo scuro sopra
/// tutto, Esc o «Annulla» chiudono senza fare niente.
fn confirm_dialog(g: &Gui, what: Confirm) -> Element<'_, Msg> {
    let p = style::p();
    let chat = g.app.current_chat();
    if chat.is_none() && matches!(what, Confirm::Block(_) | Confirm::DeleteChat) {
        return space().into();
    }
    let name = chat.map_or("", |c| c.name.as_str());
    let user = chat.is_some_and(|c| !c.is_group);
    let broadcast = chat.is_some_and(|c| c.broadcast);
    let me = chat.and_then(|c| g.app.peer_info.get(&c.peer_id)).is_some_and(|i| i.me);
    let (title, body, yes, harmful) = match what {
        Confirm::EndSession(Some(_)) => (
            tr("Chiudere questa sessione?").to_string(),
            tr("Il dispositivo viene disconnesso e per usarlo dovrà accedere di nuovo."),
            tr("Chiudi"),
            true,
        ),
        Confirm::EndSession(None) => (
            tr("Chiudere tutte le altre sessioni?").to_string(),
            tr("Tutti i dispositivi tranne questo vengono disconnessi."),
            tr("Chiudi tutte"),
            true,
        ),
        Confirm::ClearCache(CacheKind::Media) => (
            tr("Svuotare la cache dei media?").to_string(),
            tr("Foto, sticker e miniature salvati sul disco vengono cancellati; si riscaricano quando servono."),
            tr("Svuota"),
            true,
        ),
        Confirm::ClearCache(CacheKind::Avatars) => (
            tr("Svuotare le foto profilo?").to_string(),
            tr("Le foto profilo salvate vengono cancellate; si riscaricano quando servono."),
            tr("Svuota"),
            true,
        ),
        Confirm::ClearCache(CacheKind::Temp) => (
            tr("Cancellare i file temporanei?").to_string(),
            tr("Audio e video già riprodotti e immagini incollate dagli appunti."),
            tr("Cancella"),
            true,
        ),
        Confirm::Block(true) => (
            trf!("Bloccare {}?", name),
            tr("Non potrà più scriverti, chiamarti né vedere il tuo ultimo accesso. Puoi sbloccarlo quando vuoi."),
            tr("Blocca"),
            true,
        ),
        Confirm::Block(false) => (
            trf!("Sbloccare {}?", name),
            tr("Potrà di nuovo scriverti e chiamarti."),
            tr("Sblocca"),
            false,
        ),
        Confirm::DeleteChat if user => (
            trf!("Eliminare la chat con {}?", name),
            tr("La cronologia sparisce da tutti i tuoi dispositivi. Non si può annullare."),
            tr("Elimina"),
            true,
        ),
        Confirm::DeleteChat if broadcast => (
            trf!("Abbandonare «{}»?", name),
            tr("Il canale sparisce dalla lista e non riceverai più i suoi messaggi."),
            tr("Abbandona"),
            true,
        ),
        Confirm::DeleteChat => (
            trf!("Uscire da «{}»?", name),
            tr("Esci dal gruppo e la chat sparisce dalla lista. Per rientrare servirà un nuovo invito."),
            tr("Esci"),
            true,
        ),
    };
    let mut card = column![
        text(title).size(18).font(BOLD),
        text(body).color(p.muted),
    ]
    .spacing(12);
    if what == Confirm::DeleteChat && user && !me {
        card = card.push(
            iced::widget::checkbox(g.revoke)
                .label(trf!("Elimina anche per {}", name))
                .on_toggle(Msg::Revoke),
        );
    }
    let confirm = button(text(yes)).on_press(Msg::ConfirmYes).padding([8, 16]);
    let confirm = if harmful { confirm.style(style::danger) } else { confirm.style(style::primary) };
    card = card.push(
        row![
            space().width(Fill),
            button(text(tr("Annulla"))).on_press(Msg::Cancel).padding([8, 16]).style(style::flat(false)),
            confirm,
        ]
        .spacing(8)
        .align_y(Center),
    );
    let card = container(card).padding(22).max_width(440).style(move |_: &iced::Theme| {
        iced::widget::container::Style {
            background: Some(iced::Background::Color(p.sidebar)),
            text_color: Some(p.text),
            border: iced::border::rounded(14),
            ..Default::default()
        }
    });
    // il velo prende tutti i click: sotto non si preme niente per sbaglio
    iced::widget::opaque(
        mouse_area(
            container(iced::widget::opaque(card))
                .center(Fill)
                .padding(16)
                .style(style::fill(iced::Color::from_rgba(0.0, 0.0, 0.0, 0.55))),
        )
        .on_press(Msg::Cancel),
    )
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
                text(tr("Inoltra a…")).size(20).font(BOLD).width(Fill),
                button(text("✕")).on_press(Msg::Cancel).style(style::flat(false)),
            ]
            .align_y(Center),
            text_input(tr("Cerca chat…"), &g.fwd_filter)
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
