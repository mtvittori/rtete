//! RTete GUI — la stessa RTete (grammers, worker, cache delle anteprime,
//! stato `App`) con un'interfaccia iced al posto della TUI. L'accesso
//! (numero, codice) resta alla TUI: qui si riusa la sessione salvata.

mod net;
mod notify;
mod settings;
mod style;
mod ui;

use grammers_client::session::types::PeerId;
use iced::widget::{operation, scrollable};
use iced::{keyboard, Subscription, Task};

use rtete::app::App;
use rtete::clipboard;
use rtete::send::{Request, SendJob, SendKind};
use rtete::sync::{handle_response, handle_update};
use rtete::tg;

use crate::net::{Carrier, Link, NetEvent};
use crate::settings::{Settings, ThemeChoice};

pub const COMPOSE: &str = "compose";
pub const HISTORY: &str = "history";
pub const FWD_SEARCH: &str = "fwd-search";

#[derive(Debug, Clone)]
pub enum Msg {
    Net(Carrier),
    Open(PeerId),
    Filter(String),
    Compose(String),
    Submit,
    Scrolled(scrollable::Viewport),
    Older,
    Select(i32),
    Reply(i32),
    Edit(i32),
    AskDelete(i32),
    ConfirmDelete,
    Cancel,
    React(i32, &'static str),
    Copy(i32),
    Save(i32),
    Play(i32),
    PasteImage,
    Stickers,
    OpenSet(i64),
    SetsBack,
    SendSticker(usize),
    Link(String),
    Key(keyboard::Event),
    /// controllo periodico del tema Omarchy
    Tick,
    ToggleSettings,
    SetTheme(ThemeChoice),
    Scale(f32),
    LinksInApp(bool),
    TgHandler,
    Join,
    Gpu(bool),
    Forward(i32),
    ForwardTo(PeerId),
    FwdFilter(String),
    /// un frame mentre un'animazione corre
    Frame(std::time::Instant),
    /// la finestra ha preso o perso il fuoco
    Focus(bool),
    Notify(bool),
    NotifyText(bool),
}

pub enum Phase {
    Connecting(String),
    Failed(String),
    Ready,
}

pub struct Gui {
    pub app: App,
    pub phase: Phase,
    link: Option<Link>,
    pub filter: String,
    pub tz: jiff::tz::TimeZone,
    /// la vista è in fondo alla cronologia: i nuovi messaggi la seguono
    at_bottom: bool,
    pub settings: Settings,
    pub show_settings: bool,
    /// chi apre i link tg:// (letto quando si aprono le impostazioni)
    pub tg_handler: String,
    /// ultima modifica vista del colors.toml di Omarchy
    theme_stamp: Option<std::time::SystemTime>,
    /// link arrivati prima della connessione
    pending_links: Vec<String>,
    /// messaggio da inoltrare: (chat di partenza, id)
    pub forwarding: Option<(tg::ChatRef, i32)>,
    pub fwd_filter: String,
    /// animazioni: pannello sticker, barra azioni del messaggio scelto
    pub panel: iced::Animation<bool>,
    pub bar: iced::Animation<bool>,
    /// l'istante del frame che si sta disegnando
    pub now: std::time::Instant,
    pub epoch: std::time::Instant,
    /// finestra a fuoco: la chat aperta non notifica
    focused: bool,
}

fn main() -> iced::Result {
    // GPU (wgpu) per uno scorrimento fluido; dalle impostazioni si passa
    // al disegno software (tiny-skia, ~35 MB in meno). ICED_BACKEND vince.
    if std::env::var_os("ICED_BACKEND").is_none() && !Settings::load().gpu {
        std::env::set_var("ICED_BACKEND", "tiny-skia");
    }
    let link = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    // istanza unica: la finestra già aperta riceve il link e viene a fuoco
    if net::hand_off(link.as_deref()) {
        focus_existing();
        return Ok(());
    }
    net::start(link);
    iced::application(boot, Gui::update, ui::view)
        .title("RTete")
        .theme(|_: &Gui| style::theme())
        .scale_factor(|g: &Gui| g.settings.scale)
        .subscription(Gui::subscription)
        .window(iced::window::Settings {
            size: iced::Size::new(1100.0, 760.0),
            // la classe che Hyprland vede (regole finestra, scorciatoie)
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "rtete-gui".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .run()
}

/// Porta a fuoco la finestra di RTete già aperta (Hyprland).
pub fn focus_existing() {
    let lua = r#"hl.dsp.focus({ window = "class:rtete-gui" })"#;
    let ok = std::process::Command::new("hyprctl")
        .args(["dispatch", lua])
        .output()
        .is_ok_and(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).contains("rror"));
    if !ok {
        let _ = std::process::Command::new("hyprctl")
            .args(["dispatch", "focuswindow", "class:rtete-gui"])
            .output();
    }
}

fn boot() -> Gui {
    let settings = Settings::load();
    apply_theme(settings.theme);
    Gui {
        settings,
        show_settings: false,
        tg_handler: String::new(),
        theme_stamp: style::omarchy_stamp(),
        pending_links: Vec::new(),
        forwarding: None,
        fwd_filter: String::new(),
        panel: iced::Animation::new(false).quick(),
        bar: iced::Animation::new(false).quick(),
        now: std::time::Instant::now(),
        epoch: std::time::Instant::now(),
        focused: true,
        app: App::default(),
        phase: Phase::Connecting("avvio…".into()),
        link: None,
        filter: String::new(),
        tz: jiff::tz::TimeZone::system(),
        at_bottom: true,
    }
}

fn apply_theme(choice: ThemeChoice) {
    let pal = match choice {
        ThemeChoice::Omarchy => style::omarchy().unwrap_or(style::RTETE),
        ThemeChoice::Rtete => style::RTETE,
    };
    style::set(pal);
}

impl Gui {
    fn subscription(&self) -> Subscription<Msg> {
        let mut subs = vec![
            Subscription::run(net::events).map(Msg::Net),
            keyboard::listen().map(Msg::Key),
            iced::event::listen_with(|e, _, _| match e {
                iced::Event::Window(iced::window::Event::Focused) => Some(Msg::Focus(true)),
                iced::Event::Window(iced::window::Event::Unfocused) => Some(Msg::Focus(false)),
                _ => None,
            }),
        ];
        // frame a ogni ridisegno solo mentre un'animazione corre: ferma
        // la finestra non consuma niente
        let now = std::time::Instant::now();
        if self.panel.is_animating(now) || self.bar.is_animating(now) {
            subs.push(iced::window::frames().map(Msg::Frame));
        }
        // il tema Omarchy si ricontrolla ogni 2s; con qualcuno che scrive
        // ogni 400 ms, per i puntini e perché il «sta scrivendo» scada
        let every = if !self.app.typing.is_empty() {
            Some(400)
        } else if self.settings.theme == ThemeChoice::Omarchy {
            Some(2000)
        } else {
            None
        };
        if let Some(ms) = every {
            subs.push(iced::time::every(std::time::Duration::from_millis(ms)).map(|_| Msg::Tick));
        }
        Subscription::batch(subs)
    }

    fn req(&self) -> Option<tokio::sync::mpsc::UnboundedSender<Request>> {
        self.link.as_ref().map(|l| l.req.clone())
    }

    fn update(&mut self, msg: Msg) -> Task<Msg> {
        self.now = std::time::Instant::now();
        let (was_sticker, was_sel) = (self.app.show_sticker, self.app.msg_cursor);
        let task = self.handle(msg);
        // le animazioni partono dai cambi di stato, da qualunque messaggio
        if self.app.show_sticker != was_sticker {
            self.panel.go_mut(self.app.show_sticker, self.now);
        }
        if self.app.msg_cursor != was_sel && self.app.msg_cursor.is_some() {
            self.bar = iced::Animation::new(false).quick().go(true, self.now);
        }
        task
    }

    fn handle(&mut self, msg: Msg) -> Task<Msg> {
        let task = match msg {
            Msg::Frame(at) => {
                self.now = at;
                return Task::none();
            }
            Msg::Focus(on) => {
                self.focused = on;
                if let (true, Some(p)) = (on, self.app.open) {
                    notify::dismiss(p);
                }
                return Task::none();
            }
            Msg::Notify(on) => {
                self.settings.notify = on;
                self.settings.save();
                Task::none()
            }
            Msg::NotifyText(on) => {
                self.settings.notify_text = on;
                self.settings.save();
                Task::none()
            }
            Msg::Forward(id) => {
                if let Some(chat) = self.app.current_chat() {
                    self.forwarding = Some((chat.peer_ref, id));
                    self.fwd_filter.clear();
                    self.app.msg_cursor = None;
                    return operation::focus(FWD_SEARCH);
                }
                Task::none()
            }
            Msg::FwdFilter(s) => {
                self.fwd_filter = s;
                Task::none()
            }
            Msg::ForwardTo(pid) => {
                let dest = self
                    .app
                    .contacts
                    .iter()
                    .find(|c| tg::same_peer(c.peer_id, pid))
                    .map(|c| (c.peer_ref, c.name.clone()));
                if let (Some((from, id)), Some((to, to_name)), Some(req)) =
                    (self.forwarding.take(), dest, self.req())
                {
                    let _ = req.send(Request::Forward { from, id, to, to_name: to_name.clone() });
                    self.app.status = format!("inoltro a {to_name}…");
                }
                Task::none()
            }
            Msg::Net(c) => match c.take() {
                Some(ev) => self.on_net(ev),
                None => Task::none(),
            },
            Msg::Open(pid) => self.open_chat(pid),
            Msg::Filter(s) => {
                self.filter = s;
                Task::none()
            }
            Msg::Compose(s) => {
                let cap = self.app.compose_cap();
                self.app.compose = s.chars().take(cap).collect();
                Task::none()
            }
            Msg::Submit => self.submit(),
            Msg::Scrolled(v) => {
                // ancorata in fondo: offset() = distanza dal fondo,
                // reversed() = distanza dalla cima
                self.at_bottom = v.absolute_offset().y < 8.0;
                if v.absolute_offset_reversed().y < 120.0 {
                    self.app.want_older = true;
                    if let Some(req) = self.req() {
                        self.app.request_older(&req);
                    }
                }
                // arrivano a raffica: niente giro completo di after_change
                return Task::none();
            }
            Msg::Older => {
                self.app.want_older = true;
                Task::none()
            }
            Msg::Select(id) => {
                self.app.msg_cursor = if self.app.msg_cursor == Some(id) { None } else { Some(id) };
                self.app.disarm_delete();
                Task::none()
            }
            Msg::Reply(id) => {
                self.app.replying = Some(id);
                self.app.editing = None;
                self.app.msg_cursor = None;
                self.app.disarm_delete();
                operation::focus(COMPOSE)
            }
            Msg::Edit(id) => {
                let text = self.app.open_history().and_then(|ring| {
                    ring.iter().find(|r| r.id == id).and_then(|r| match &r.kind {
                        tg::RowKind::Text(t) => Some(t.clone()),
                        _ => None,
                    })
                });
                match text {
                    Some(t) => {
                        self.app.discard_pending_photo();
                        self.app.editing = Some(id);
                        self.app.replying = None;
                        self.app.msg_cursor = None;
                        self.app.compose = t;
                        Task::batch([operation::focus(COMPOSE), operation::move_cursor_to_end(COMPOSE)])
                    }
                    None => Task::none(),
                }
            }
            Msg::AskDelete(id) => {
                self.app.del_arm = true;
                self.app.del_id = Some(id);
                Task::none()
            }
            Msg::ConfirmDelete => {
                let target = self.app.del_id.take();
                self.app.del_arm = false;
                if let (Some(id), Some(chat), Some(req)) =
                    (target, self.app.current_chat(), self.req())
                {
                    let _ = req.send(Request::Del { peer_ref: chat.peer_ref, id });
                    self.app.status = "cancello il messaggio…".into();
                    if self.app.msg_cursor == Some(id) {
                        self.app.msg_cursor = None;
                    }
                }
                Task::none()
            }
            Msg::Cancel => {
                self.cancel();
                Task::none()
            }
            Msg::React(id, emoticon) => {
                let mine_already = self
                    .app
                    .open_history()
                    .and_then(|ring| ring.iter().find(|r| r.id == id))
                    .is_some_and(|r| r.reactions.iter().any(|i| i.mine && i.emoticon == emoticon));
                if let (Some(chat), Some(req)) = (self.app.current_chat(), self.req()) {
                    let _ = req.send(Request::React {
                        peer_ref: chat.peer_ref,
                        msg_id: id,
                        emoticon: (!mine_already).then(|| emoticon.to_string()),
                    });
                    // optimistic_react lavora sul cursore di selezione
                    self.app.msg_cursor = Some(id);
                    self.app.optimistic_react(emoticon);
                }
                Task::none()
            }
            Msg::Copy(id) => {
                let text = self
                    .app
                    .open_history()
                    .and_then(|ring| ring.iter().find(|r| r.id == id))
                    .and_then(clipboard::copy_text);
                match text {
                    Some(t) => {
                        self.app.status = "copiato negli appunti".into();
                        iced::clipboard::write(t)
                    }
                    None => Task::none(),
                }
            }
            Msg::Save(id) => {
                let job = self.row_media(id, |k| match k {
                    tg::RowKind::Doc { media, name, .. } => Some((media.clone(), name.clone())),
                    _ => None,
                });
                if let (Some((media, name)), Some(req)) = (job, self.req()) {
                    self.app.status = format!("salvo «{name}» in ~/Downloads…");
                    let _ = req.send(Request::Save { media, name });
                }
                Task::none()
            }
            Msg::Play(id) => {
                let job = self.row_media(id, |k| match k {
                    tg::RowKind::Audio { media, name, voice, .. } => {
                        let label = if *voice { "voce".to_string() } else { name.clone() };
                        let fname = if *voice {
                            format!("voce-{}.ogg", tg::media_key(media))
                        } else {
                            tg::sanitize_file_name(name).unwrap_or_else(|| "brano".into())
                        };
                        Some((media.clone(), fname, label, false))
                    }
                    tg::RowKind::Video { media, .. } => Some((
                        media.clone(),
                        format!("video-{}.mp4", tg::media_key(media)),
                        "video".to_string(),
                        true,
                    )),
                    _ => None,
                });
                if let (Some((media, fname, label, video)), Some(req)) = (job, self.req()) {
                    self.app.status = format!("scarico «{label}» e la riproduco…");
                    let _ = req.send(Request::Play { media, fname, label, video });
                }
                Task::none()
            }
            Msg::PasteImage => {
                if let Some(link) = &self.link {
                    clipboard::ctrl_paste(&mut self.app, &link.res);
                }
                operation::focus(COMPOSE)
            }
            Msg::Stickers => {
                self.app.show_sticker = !self.app.show_sticker;
                if self.app.show_sticker && self.app.sets.is_none() {
                    if let Some(req) = self.req() {
                        let _ = req.send(Request::Sets);
                    }
                    self.app.status = "carico i set di sticker…".into();
                }
                Task::none()
            }
            Msg::OpenSet(set_id) => {
                self.app.open_set = Some(set_id);
                self.app.set_level = true;
                let short = self
                    .app
                    .sets
                    .as_ref()
                    .and_then(|s| s.iter().find(|s| s.set_id == set_id))
                    .map(|s| s.short_name.clone());
                if !self.app.set_docs.contains_key(&set_id) && !self.app.docs_cooling(set_id) {
                    if let (Some(short), Some(req)) = (short, self.req()) {
                        let _ = req.send(Request::SetDocs { set_id, short });
                        self.app.status = "scarico il set…".into();
                    }
                }
                Task::none()
            }
            Msg::SetsBack => {
                self.app.set_level = false;
                Task::none()
            }
            Msg::SendSticker(i) => {
                let media = self
                    .app
                    .open_set
                    .and_then(|id| self.app.set_docs.get(&id))
                    .and_then(|docs| docs.get(i))
                    .cloned();
                if let (Some(media), Some(chat), Some(req)) =
                    (media, self.app.current_chat(), self.req())
                {
                    let _ = req.send(Request::Send(SendJob {
                        peer: chat.peer_ref,
                        peer_id: chat.peer_id,
                        kind: SendKind::Sticker(media),
                        reply_to: self.app.replying,
                    }));
                    self.app.replying = None;
                    self.app.show_sticker = false;
                    self.app.status = "invio sticker…".into();
                    return self.snap_bottom();
                }
                Task::none()
            }
            Msg::Link(url) => {
                match tg::parse_link(&url).filter(|_| self.settings.links_in_app) {
                    Some(link) => self.follow(link),
                    None => {
                        let _ = std::process::Command::new("xdg-open")
                            .arg(&url)
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn();
                    }
                }
                Task::none()
            }
            Msg::Tick => {
                let now = std::time::Instant::now();
                self.app.typing.retain(|_, (until, _)| *until > now);
                let stamp = style::omarchy_stamp();
                if stamp != self.theme_stamp {
                    self.theme_stamp = stamp;
                    apply_theme(self.settings.theme);
                }
                Task::none()
            }
            Msg::ToggleSettings => {
                self.show_settings = !self.show_settings;
                if self.show_settings {
                    self.tg_handler = settings::tg_handler();
                }
                Task::none()
            }
            Msg::SetTheme(choice) => {
                self.settings.theme = choice;
                apply_theme(choice);
                self.theme_stamp = style::omarchy_stamp();
                self.settings.save();
                Task::none()
            }
            Msg::Scale(f) => {
                self.settings.scale = (f * 20.0).round() / 20.0;
                self.settings.save();
                Task::none()
            }
            Msg::Gpu(on) => {
                self.settings.gpu = on;
                self.settings.save();
                self.app.status = "vale dal prossimo avvio".into();
                Task::none()
            }
            Msg::LinksInApp(on) => {
                self.settings.links_in_app = on;
                self.settings.save();
                Task::none()
            }
            Msg::TgHandler => {
                if !settings::set_tg_handler() {
                    self.app.status = "xdg-mime non ha funzionato".into();
                }
                self.tg_handler = settings::tg_handler();
                Task::none()
            }
            Msg::Join => {
                if let (Some((hash, title, _)), Some(req)) = (self.app.invite.take(), self.req()) {
                    let _ = req.send(Request::Join(hash));
                    self.app.status = format!("entro in «{title}»…");
                }
                Task::none()
            }
            Msg::Key(keyboard::Event::KeyPressed { key, .. }) => {
                if key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                    self.cancel();
                }
                Task::none()
            }
            Msg::Key(_) => Task::none(),
        };
        self.after_change();
        // un link risolto: la chat si apre appena è nella lista
        if let Some((pid, post)) = self.app.link_open {
            if self.app.contacts.iter().any(|c| tg::same_peer(c.peer_id, pid)) {
                self.app.link_open = None;
                self.show_settings = false;
                let open = self.open_chat(pid);
                self.app.msg_cursor = post;
                return Task::batch([task, open]);
            }
        }
        task
    }

    /// Un link Telegram: le chat private si cercano fra le proprie, il
    /// resto lo risolve il worker (username, inviti).
    fn follow(&mut self, link: tg::TgLink) {
        if let tg::TgLink::Private { channel, post } = link {
            let pid = PeerId::channel(channel);
            match pid.filter(|p| self.app.contacts.iter().any(|c| tg::same_peer(c.peer_id, *p))) {
                Some(p) => self.app.link_open = Some((p, post)),
                None => self.app.status = "link a una chat privata che non è fra le tue".into(),
            }
            return;
        }
        if let Some(req) = self.req() {
            let _ = req.send(Request::Link(link));
            self.app.status = "apro il link…".into();
        }
    }

    /// Link da fuori (argomento o altra istanza): come un click in chat,
    /// ma sempre dentro RTete.
    fn external_link(&mut self, url: String) {
        if self.link.is_none() {
            self.pending_links.push(url);
            return;
        }
        match tg::parse_link(&url) {
            Some(link) => self.follow(link),
            None => self.app.status = format!("link non riconosciuto: {url}"),
        }
    }

    /// Esc o ✕: chiude il primo pannello o annulla l'azione in corso.
    fn cancel(&mut self) {
        if self.forwarding.take().is_some() {
            return;
        }
        if self.show_settings {
            self.show_settings = false;
            return;
        }
        let a = &mut self.app;
        if a.invite.take().is_some() {
        } else if a.show_sticker {
            a.show_sticker = false;
        } else if a.del_arm {
            a.disarm_delete();
        } else if a.editing.take().is_some() {
            a.compose.clear();
        } else if a.replying.take().is_some() {
        } else if a.pending_photo.is_some() {
            a.discard_pending_photo();
        } else {
            a.msg_cursor = None;
        }
        a.status.clear();
    }

    fn row_media<T>(&self, id: i32, f: impl Fn(&tg::RowKind) -> Option<T>) -> Option<T> {
        self.app
            .open_history()
            .and_then(|ring| ring.iter().find(|r| r.id == id))
            .and_then(|r| f(&r.kind))
    }

    /// dopo ogni messaggio: pagine vecchie, citazioni, anteprime da convertire
    fn after_change(&mut self) {
        let Some(link) = &self.link else {
            return;
        };
        let req = link.req.clone();
        self.app.request_older(&req);
        self.app.request_quoted(&req);
        self.app.request_avatars(&req);
        self.app.refresh_preview_queue(&req);
        if self.app.show_sticker && self.app.set_level {
            if let Some(docs) = self.app.open_set.and_then(|id| self.app.set_docs.get(&id)).cloned() {
                self.app.ensure_previews(&req, docs.iter());
            }
        }
    }

    /// la cronologia è ancorata in fondo: l'offset 0 (START) è l'ultimo
    /// messaggio, END sarebbe la cima
    fn snap_bottom(&mut self) -> Task<Msg> {
        self.at_bottom = true;
        operation::snap_to(HISTORY, scrollable::RelativeOffset::START)
    }

    fn on_net(&mut self, ev: NetEvent) -> Task<Msg> {
        match ev {
            NetEvent::Status(s) => {
                self.phase = Phase::Connecting(s);
                Task::none()
            }
            NetEvent::Failed(e) => {
                self.phase = Phase::Failed(e);
                Task::none()
            }
            NetEvent::Ready { link, contacts, me } => {
                self.app.contacts = contacts;
                self.app.me = me;
                self.link = Some(link);
                self.phase = Phase::Ready;
                for url in std::mem::take(&mut self.pending_links) {
                    self.external_link(url);
                }
                Task::none()
            }
            NetEvent::Link(url) => {
                self.external_link(url);
                Task::none()
            }
            NetEvent::OpenPeer(pid) => self.open_chat(pid),
            NetEvent::UpdateErr(e) => {
                self.app.status = format!("rete: {e}");
                Task::none()
            }
            NetEvent::Update(u) => {
                let Some(link) = &self.link else {
                    return Task::none();
                };
                let client = link.client.clone();
                // messaggio in arrivo: chi e da dove, prima che l'update sparisca
                let incoming = match &u {
                    grammers_client::update::Update::NewMessage(m) if !m.outgoing() => {
                        Some((m.peer_id(), tg::sender_name(m)))
                    }
                    _ => None,
                };
                // handle_update aspetta la rete solo per una chat mai vista
                // (dialog_from_message); il contesto tokio serve agli spawn
                let _rt = net::RT.get().map(|rt| rt.enter());
                iced::futures::executor::block_on(handle_update(&mut self.app, &client, u));
                if let Some((peer, from)) = incoming {
                    self.notify(peer, from);
                }
                Task::none()
            }
            NetEvent::Response(r) => {
                let Some(link) = &self.link else {
                    return Task::none();
                };
                let (client, req) = (link.client.clone(), link.req.clone());
                let first_page = matches!(&r, rtete::send::Response::Hist { .. });
                let _rt = net::RT.get().map(|rt| rt.enter());
                iced::futures::executor::block_on(handle_response(&mut self.app, r, &client, &req));
                if first_page || self.at_bottom {
                    return self.snap_bottom();
                }
                Task::none()
            }
        }
    }

    /// Notifica di un messaggio nuovo: non per le chat silenziate né per
    /// quella aperta mentre la finestra è a fuoco.
    fn notify(&self, peer: PeerId, from: String) {
        if !self.settings.notify || (self.focused && self.app.open == Some(peer)) {
            return;
        }
        let Some(d) = self.app.contacts.iter().find(|d| tg::same_peer(d.peer_id, peer)) else {
            return;
        };
        if d.muted {
            return;
        }
        let text = match (&d.last, self.settings.notify_text) {
            (Some(l), true) => l.text.clone(),
            _ => "nuovo messaggio".to_string(),
        };
        let body = if d.is_group && !d.broadcast && !from.is_empty() {
            format!("{from}: {text}")
        } else {
            text
        };
        let icon = d.photo_id.and_then(|id| self.app.avatars.get(&id));
        notify::show(peer, &d.name, &body, icon.map(|p| p.as_path()));
    }

    fn open_chat(&mut self, pid: PeerId) -> Task<Msg> {
        notify::dismiss(pid);
        let Some(i) = self.app.contacts.iter().position(|c| tg::same_peer(c.peer_id, pid)) else {
            return Task::none();
        };
        self.app.contact_cursor = i;
        self.show_settings = false;
        if self.app.open == Some(pid) {
            return operation::focus(COMPOSE);
        }
        let chat = &self.app.contacts[i];
        let (peer_ref, peer) = (chat.peer_ref, chat.peer_id);
        self.app.open = Some(pid);
        if let Some(req) = self.req() {
            let _ = req.send(Request::Hist { peer_ref, peer });
        }
        let a = &mut self.app;
        a.status = "carico la cronologia…".into();
        a.editing = None;
        a.replying = None;
        a.disarm_delete();
        a.msg_cursor = None;
        a.discard_pending_photo();
        a.compose.clear();
        a.show_sticker = false;
        Task::batch([self.snap_bottom(), operation::focus(COMPOSE)])
    }

    /// Invio nel campo di scrittura: foto in attesa, modifica o testo nuovo.
    fn submit(&mut self) -> Task<Msg> {
        let Some(req) = self.req() else {
            return Task::none();
        };
        let Some((peer, peer_id)) = self.app.current_chat().map(|c| (c.peer_ref, c.peer_id)) else {
            return Task::none();
        };
        let a = &mut self.app;
        let kind = if let Some(path) = a.pending_photo.take() {
            a.status = "invio la foto…".into();
            SendKind::Photo { path, caption: std::mem::take(&mut a.compose) }
        } else if let Some(id) = a.editing {
            if a.compose.trim().is_empty() {
                a.status = "testo vuoto: niente da modificare".into();
                return Task::none();
            }
            a.editing = None;
            a.status = "salvo la modifica…".into();
            SendKind::EditText { id, text: std::mem::take(&mut a.compose) }
        } else if !a.compose.trim().is_empty() {
            a.status = "invio…".into();
            SendKind::Text(std::mem::take(&mut a.compose))
        } else {
            return Task::none();
        };
        let reply_to = match kind {
            SendKind::EditText { .. } => None,
            _ => a.replying.take(),
        };
        let _ = req.send(Request::Send(SendJob { peer, peer_id, kind, reply_to }));
        self.snap_bottom()
    }
}
