//! Rondine GUI — la stessa Rondine (backend, worker, cache delle anteprime,
//! stato `App`) con un'interfaccia iced al posto della TUI. L'accesso
//! (numero, codice) resta alla TUI: qui si riusa la sessione salvata.

mod net;
mod notify;
mod settings;
mod style;
mod ui;

use iced::widget::{operation, scrollable};
use iced::{keyboard, Subscription, Task};

use rondine::app::App;
use rondine::clipboard;
use rondine::model::ChatId;
use rondine::send::{Request, SendJob, SendKind};
use rondine::sync::{handle_live, handle_response};
use rondine::{links, model, paths};
use rondine::{i18n, tr, trf, translate};

use crate::net::{Carrier, Env, Link, NetEvent, Pairing};
use crate::settings::{Settings, ThemeChoice, UiStyle};

pub const COMPOSE: &str = "compose";
pub const HISTORY: &str = "history";
pub const FWD_SEARCH: &str = "fwd-search";
pub const LOGIN_FIELD: &str = "login";

#[derive(Debug, Clone)]
pub enum Msg {
    Net(Carrier),
    Open(ChatId),
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
    /// 📎: selettore dei file del portale
    Attach,
    Picked(Option<std::path::PathBuf>),
    /// file trascinato sulla finestra
    Dropped(std::path::PathBuf),
    /// Ctrl-V nel campo di scrittura (iced ha già incollato il testo)
    Pasted(String),
    /// Ctrl-V fuori dal campo di scrittura
    PasteKey,
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
    ForwardTo(ChatId),
    FwdFilter(String),
    /// un frame mentre un'animazione corre
    Frame(std::time::Instant),
    /// la finestra ha preso o perso il fuoco
    Focus(bool),
    /// apre la foto nel visore
    View(i32),
    Notify(bool),
    NotifyText(bool),
    /// apre o chiude la scheda della chat (info, media, link…)
    ToggleInfo,
    InfoTab(model::SharedTab),
    /// chiede conferma per bloccare (true) o sbloccare l'utente
    AskBlock(bool),
    /// chiede conferma per eliminare la chat (o uscire dal gruppo)
    AskDeleteChat,
    /// «elimina anche per l'altro» nella conferma
    Revoke(bool),
    /// il pulsante di conferma del popup
    ConfirmYes,
    /// una pagina delle impostazioni
    Page(SettingsPage),
    PfFirst(String),
    PfLast(String),
    PfUser(String),
    PfAbout(String),
    PfSave,
    /// selettore della nuova foto profilo
    PfPhoto,
    PfPhotoPicked(Option<std::path::PathBuf>),
    AutoPhotos(bool),
    VideoThumbs(bool),
    Avatars(bool),
    PhotoSize(u16),
    CacheLimit(u32),
    /// sfoltisce subito la cache al tetto scelto
    PruneNow,
    AskClear(CacheKind),
    AskEndSession(Option<i64>),
    /// scarica l'anteprima di una foto (download automatico spento)
    LoadPreview(i32),
    /// lingua dell'interfaccia (None: quella del sistema)
    Lang(Option<i18n::Lang>),
    /// passa a Telegram o a WhatsApp
    SwitchEnv(Env),
    EnvRail(bool),
    Unified(bool),
    ShowOnline(bool),
    ShowTyping(bool),
    /// un messaggio per una chat dell'altro ambiente (feed unico)
    InEnv(Env, Box<Msg>),
    /// collega WhatsApp: col QR (false) o col numero di telefono (true)
    LinkWhatsApp(bool),
    /// il numero per l'abbinamento col codice
    WaPhone(String),
    /// riprova a collegare l'ambiente in vista
    Retry,
    /// i campi dell'accesso a Telegram e l'invio
    LoginA(String),
    LoginB(String),
    LoginSubmit,
    /// mostra o nasconde la traduzione di un messaggio
    Translate(i32),
    /// traduzione automatica della chat aperta
    AutoTranslate(bool),
    TrProvider(translate::Provider),
    TrDeeplKey(String),
    TrUrl(String),
    TrKey(String),
    TrModel(String),
    /// un servizio pronto (Groq, OpenRouter, Ollama)
    TrPreset(usize),
    /// lingua di destinazione (None: quella dell'interfaccia)
    TrTarget(Option<String>),
    /// traduce una frase di prova
    TrTest,
    /// stile dell'interfaccia: Telegram o WhatsApp
    SetStyle(UiStyle),
    /// filtro della lista chat (Tutte, Non lette…)
    ChatFilter(ChatFilter),
    /// mostra le chat archiviate (o torna alla lista)
    ShowArchived(bool),
    /// menu di una chat della lista (tasto destro); None lo chiude
    ChatMenu(Option<ChatId>),
    /// archivia, fissa, silenzia, segna da leggere, timer effimero
    ChatDo(ChatId, model::ChatAction),
    ToggleFavorite(ChatId),
    /// elimina la chat dal menu della lista (passa dalla conferma)
    MenuDelete(ChatId),
    /// stella sul messaggio
    Star(i32),
    /// la lista dei messaggi importanti
    ShowStarred(bool),
    /// apre la chat sul messaggio importante (chat Bot API, id)
    OpenStarred(i64, i32),
    Unstar(i64, i32),
    Quit,
}

/// Le pagine delle impostazioni, come in Telegram.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SettingsPage {
    Main,
    Account,
    Appearance,
    Notifications,
    Media,
    Storage,
    Devices,
    Links,
    Translation,
}

/// Le parti della cache che si possono svuotare.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CacheKind {
    /// foto, sticker e miniature
    Media,
    Avatars,
    Temp,
}

impl CacheKind {
    pub fn dir(self) -> std::path::PathBuf {
        match self {
            CacheKind::Media => paths::media_dir(),
            CacheKind::Avatars => paths::media_dir().join("avatars"),
            CacheKind::Temp => paths::tmp_dir(),
        }
    }
}

/// I filtri sopra la lista chat, come in WhatsApp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChatFilter {
    All,
    Unread,
    Favorites,
    Groups,
}

/// L'azione in attesa di conferma nel popup.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Confirm {
    Block(bool),
    DeleteChat,
    /// chiude una sessione, o tutte le altre con `None`
    EndSession(Option<i64>),
    ClearCache(CacheKind),
}

pub enum Phase {
    /// passo dell'avvio (0..=2) e cosa sta succedendo
    Connecting(u8, String),
    Failed(String),
    Ready,
    /// WhatsApp non ancora collegato: la pagina che spiega e il pulsante
    Linking,
    /// accesso a Telegram: credenziali, numero, codice o password 2FA
    Login(net::Login),
    /// WhatsApp in abbinamento: il QR (o il codice) da usare col telefono
    Pairing { qr: Option<iced::widget::qr_code::Data>, code: Option<String> },
}

/// L'ambiente che non è in vista, con tutto il suo stato: le sue chat,
/// il collegamento alla rete e a che punto è.
pub struct Parked {
    pub app: App,
    link: Option<Link>,
    pub phase: Phase,
}

/// Uno stato dell'app nuovo, con le preferenze e le liste condivise.
fn new_app(settings: &Settings) -> App {
    App {
        media_prefs: settings.media_prefs(),
        translate: translate::Config::load(),
        starred: rondine::starred::load(),
        ..App::default()
    }
}

pub struct Gui {
    pub app: App,
    pub phase: Phase,
    link: Option<Link>,
    /// l'ambiente in vista (Telegram o WhatsApp)
    pub env: Env,
    /// l'altro ambiente, fermo dietro le quinte (None senza WhatsApp)
    pub parked: Option<Parked>,
    /// sto aggiornando l'ambiente nascosto: niente scorrimenti né fuoco
    background: bool,
    /// da quando WhatsApp prova a collegarsi senza risposta (per l'avviso)
    pub wa_since: Option<std::time::Instant>,
    /// i campi dell'accesso (API ID e hash, oppure il dato chiesto)
    pub login_a: String,
    pub login_b: String,
    /// il numero per collegare WhatsApp col codice invece del QR
    pub wa_phone: String,
    pub filter: String,
    pub tz: jiff::tz::TimeZone,
    /// la vista è in fondo alla cronologia: i nuovi messaggi la seguono
    at_bottom: bool,
    /// «sta scrivendo» mandato: a chi e quando (si ripete ogni 4 s)
    typing_sent: Option<(model::ChatRef, std::time::Instant)>,
    /// l'ultimo tasto nel campo di scrittura
    last_key: std::time::Instant,
    pub settings: Settings,
    pub show_settings: bool,
    /// chi apre i link tg:// (letto quando si aprono le impostazioni)
    pub tg_handler: String,
    /// ultima modifica vista del colors.toml di Omarchy
    theme_stamp: Option<std::time::SystemTime>,
    /// link arrivati prima della connessione
    pending_links: Vec<String>,
    /// messaggio da inoltrare: (chat di partenza, id)
    pub forwarding: Option<(model::ChatRef, i32)>,
    pub fwd_filter: String,
    /// animazioni: pannello sticker, barra azioni del messaggio scelto
    pub panel: iced::Animation<bool>,
    pub bar: iced::Animation<bool>,
    /// l'istante del frame che si sta disegnando
    pub now: std::time::Instant,
    pub epoch: std::time::Instant,
    /// finestra a fuoco: la chat aperta non notifica
    focused: bool,
    /// foto aperta nel visore
    pub viewer: Option<std::path::PathBuf>,
    /// il compose prima dell'ultimo Ctrl-V nel campo: se gli appunti erano
    /// un'immagine o dei file, il testo incollato da iced (un percorso, o
    /// niente) si annulla
    paste_undo: Option<String>,
    /// scheda della chat aperta accanto (o al posto) della cronologia
    pub show_info: bool,
    pub info_tab: model::SharedTab,
    /// popup di conferma aperto
    pub confirm: Option<Confirm>,
    /// eliminazione della chat privata anche per l'altro
    pub revoke: bool,
    pub settings_page: SettingsPage,
    /// il profilo in modifica (copiato da quello letto da Telegram)
    pub pf: model::MyProfile,
    /// spazio della cache: media, foto profilo, temporanei (file, byte)
    pub cache_usage: [(usize, u64); 3],
    pub chat_filter: ChatFilter,
    pub show_archived: bool,
    /// la chat della lista col menu aperto
    pub chat_menu: Option<ChatId>,
    /// la lista dei messaggi importanti al posto della chat
    pub show_starred: bool,
}

fn main() -> iced::Result {
    // prima di leggere le impostazioni: le cartelle di RTete diventano di Rondine
    paths::migrate_old_dirs();
    i18n::set(Settings::load().lang.resolve());
    // GPU (wgpu) per uno scorrimento fluido; dalle impostazioni si passa
    // al disegno software (tiny-skia, ~35 MB in meno). ICED_BACKEND vince.
    if std::env::var_os("ICED_BACKEND").is_none() && !Settings::load().gpu {
        std::env::set_var("ICED_BACKEND", "tiny-skia");
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--telegram` / `--whatsapp`: un ambiente solo, come due app separate
    if args.iter().any(|a| a == "--telegram" || a == "-t") {
        settings::set_solo(Env::Telegram);
    } else if cfg!(feature = "whatsapp") && args.iter().any(|a| a == "--whatsapp" || a == "-w") {
        settings::set_solo(Env::WhatsApp);
    }
    let solo = settings::solo();
    let app_id = app_id();
    // i link tg:// sono di Telegram: all'istanza con Telegram che è aperta
    let link = args.into_iter().find(|a| !a.starts_with('-')).filter(|_| solo != Some(Env::WhatsApp));
    // istanza unica: la finestra già aperta (questa o quella coi due
    // ambienti, che userebbe le stesse sessioni) riceve il link e viene a fuoco
    let others: &[&str] = match solo {
        None => &["rondine-gui", "rondine-telegram", "rondine-whatsapp"],
        Some(_) => &[app_id, "rondine-gui"],
    };
    let mut first = None;
    for id in others {
        let l = link.as_deref().filter(|_| *id != "rondine-whatsapp");
        if net::hand_off(id, l) && first.is_none() {
            first = Some(*id);
        }
    }
    if let Some(id) = first {
        focus_existing(id);
        return Ok(());
    }
    net::start(app_id, solo != Some(Env::WhatsApp), link, Settings::load().cache_mb as u64 * 1024 * 1024);
    // WhatsApp già collegato: si connette insieme a Telegram
    #[cfg(feature = "whatsapp")]
    if solo != Some(Env::Telegram) && rondine::backend::wa::has_session() {
        net::start_whatsapp(None);
    }
    iced::application(boot, Gui::update, ui::view)
        .title(|_: &Gui| match settings::solo() {
            Some(Env::Telegram) => "Rondine · Telegram".to_string(),
            Some(Env::WhatsApp) => "Rondine · WhatsApp".to_string(),
            None => "Rondine".to_string(),
        })
        .theme(|_: &Gui| style::theme())
        .scale_factor(|g: &Gui| g.settings.scale)
        .subscription(Gui::subscription)
        .window(iced::window::Settings {
            size: iced::Size::new(1100.0, 760.0),
            // la classe che Hyprland vede (regole finestra, scorciatoie)
            #[cfg(target_os = "linux")]
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: app_id.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .run()
}

/// La classe della finestra (e il nome del socket): `rondine-gui`, o
/// `rondine-telegram` / `rondine-whatsapp` per le istanze a sé.
pub fn app_id() -> &'static str {
    match settings::solo() {
        Some(Env::Telegram) => "rondine-telegram",
        Some(Env::WhatsApp) => "rondine-whatsapp",
        None => "rondine-gui",
    }
}

/// Porta a fuoco la finestra di Rondine già aperta (Hyprland).
pub fn focus_existing(app_id: &str) {
    let lua = format!(r#"hl.dsp.focus({{ window = "class:{app_id}" }})"#);
    let ok = std::process::Command::new("hyprctl")
        .args(["dispatch", &lua])
        .output()
        .is_ok_and(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).contains("rror"));
    if !ok {
        let _ = std::process::Command::new("hyprctl")
            .args(["dispatch", "focuswindow", &format!("class:{app_id}")])
            .output();
    }
}

fn boot() -> Gui {
    let mut settings = Settings::load();
    // da sola l'ambiente scelto all'avvio; con WhatsApp quello dell'ultima
    // volta; senza, lo stile scelto
    if let Some(env) = settings::solo() {
        settings.style = env;
    }
    let env = if cfg!(feature = "whatsapp") { settings.style } else { Env::Telegram };
    style::set_wa(settings.layout() == UiStyle::WhatsApp);
    apply_theme(settings.active_theme());
    let telegram = Parked { app: new_app(&settings), link: None, phase: Phase::Connecting(0, tr("avvio…").into()) };
    let whatsapp = whatsapp_slot(&settings);
    let (active, parked) = match (env, whatsapp) {
        (Env::WhatsApp, Some(wa)) if settings::solo().is_some() => (wa, None),
        (Env::WhatsApp, Some(wa)) => (wa, Some(telegram)),
        (_, _) if settings::solo().is_some() => (telegram, None),
        (_, wa) => (telegram, wa),
    };
    let env = if (parked.is_some() || settings::solo() == Some(Env::WhatsApp)) && env == Env::WhatsApp {
        Env::WhatsApp
    } else {
        Env::Telegram
    };
    Gui {
        env,
        parked,
        background: false,
        wa_since: None,
        login_a: String::new(),
        login_b: String::new(),
        wa_phone: String::new(),
        settings: settings.clone(),
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
        viewer: None,
        paste_undo: None,
        show_info: false,
        info_tab: model::SharedTab::Media,
        confirm: None,
        revoke: false,
        settings_page: SettingsPage::Main,
        pf: model::MyProfile::default(),
        cache_usage: [(0, 0); 3],
        chat_filter: ChatFilter::All,
        show_archived: false,
        chat_menu: None,
        show_starred: false,
        app: active.app,
        phase: active.phase,
        link: active.link,
        filter: String::new(),
        tz: jiff::tz::TimeZone::system(),
        at_bottom: true,
        typing_sent: None,
        last_key: std::time::Instant::now(),
    }
}

/// L'ambiente WhatsApp all'avvio: già collegato (si connette subito) o
/// da collegare. Senza la feature non c'è.
#[cfg(feature = "whatsapp")]
fn whatsapp_slot(settings: &Settings) -> Option<Parked> {
    let phase = if rondine::backend::wa::has_session() {
        Phase::Connecting(0, tr("mi collego a WhatsApp…").into())
    } else {
        Phase::Linking
    };
    Some(Parked { app: new_app(settings), link: None, phase })
}

#[cfg(not(feature = "whatsapp"))]
fn whatsapp_slot(_: &Settings) -> Option<Parked> {
    None
}

fn apply_theme(choice: ThemeChoice) {
    let pal = match choice {
        ThemeChoice::Omarchy => style::omarchy().unwrap_or(style::RONDINE),
        ThemeChoice::Rondine => style::RONDINE,
        ThemeChoice::WhatsApp => style::WHATSAPP,
    };
    style::set(pal);
}

impl Gui {
    fn subscription(&self) -> Subscription<Msg> {
        let mut subs = vec![
            Subscription::run(net::events).map(Msg::Net),
            keyboard::listen().map(Msg::Key),
            iced::event::listen_with(|e, status, _| match e {
                iced::Event::Window(iced::window::Event::Focused) => Some(Msg::Focus(true)),
                iced::Event::Window(iced::window::Event::Unfocused) => Some(Msg::Focus(false)),
                iced::Event::Window(iced::window::Event::FileDropped(p)) => Some(Msg::Dropped(p)),
                // nel campo di scrittura il Ctrl-V lo cattura text_input (on_paste)
                iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. })
                    if status == iced::event::Status::Ignored
                        && modifiers.command()
                        && key.as_ref() == keyboard::Key::Character("v") =>
                {
                    Some(Msg::PasteKey)
                }
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
        let every = if matches!(self.phase, Phase::Connecting(..)) {
            // la rotella della schermata di avvio
            Some(80)
        } else if !self.app.typing.is_empty() || self.typing_sent.is_some() || self.app.read_mark_pending {
            Some(400)
        } else if self.settings.active_theme() == ThemeChoice::Omarchy {
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
            Msg::View(id) => {
                // l'originale a piena risoluzione se c'è, sennò l'anteprima
                self.viewer = self.row_media(id, |k| match k {
                    model::RowKind::Photo { media, .. } => rondine::loader::original(media)
                        .or_else(|| self.app.previews.get(media.key()).cloned()),
                    _ => None,
                });
                return Task::none();
            }
            Msg::Focus(on) => {
                self.focused = on;
                self.send_presence();
                if !on {
                    self.stop_typing();
                }
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
                    .find(|c| model::same_peer(c.peer_id, pid))
                    .map(|c| (c.peer_ref, c.name.clone()));
                if let (Some((from, id)), Some((to, to_name)), Some(req)) =
                    (self.forwarding.take(), dest, self.req())
                {
                    let _ = req.send(Request::Forward { from, id, to, to_name: to_name.clone() });
                    self.app.status = trf!("inoltro a {}…", to_name);
                }
                Task::none()
            }
            Msg::Net(c) => match c.take() {
                // click su una notifica dell'altro ambiente: ci si sposta lì
                Some((env, NetEvent::OpenPeer(pid))) => {
                    let switch = self.switch_env(env);
                    Task::batch([switch, self.open_chat(pid)])
                }
                Some((env, ev)) => self.in_env(env, |g| g.on_net(ev)),
                None => Task::none(),
            },
            Msg::SwitchEnv(env) => self.switch_env(env),
            Msg::LoginA(s) => {
                self.login_a = s;
                Task::none()
            }
            Msg::LoginB(s) => {
                self.login_b = s;
                Task::none()
            }
            Msg::LoginSubmit => {
                let a = std::mem::take(&mut self.login_a);
                let b = std::mem::take(&mut self.login_b);
                let text = match &self.phase {
                    Phase::Login(net::Login::Credentials(_)) => format!("{} {}", a.trim(), b.trim()),
                    _ => a,
                };
                if !text.trim().is_empty() {
                    net::answer(text);
                    self.phase = Phase::Connecting(1, tr("verifico l'account…").into());
                }
                Task::none()
            }
            Msg::LinkWhatsApp(by_phone) => {
                #[cfg(feature = "whatsapp")]
                {
                    let phone = by_phone.then(|| self.wa_phone.clone());
                    self.phase = Phase::Connecting(0, tr("mi collego a WhatsApp…").into());
                    self.wa_since = Some(std::time::Instant::now());
                    net::start_whatsapp(phone);
                }
                #[cfg(not(feature = "whatsapp"))]
                let _ = by_phone;
                Task::none()
            }
            Msg::WaPhone(s) => {
                self.wa_phone = s.chars().filter(|c| c.is_ascii_digit() || *c == '+' || *c == ' ').take(20).collect();
                Task::none()
            }
            Msg::Retry => {
                self.phase = Phase::Connecting(0, tr("avvio…").into());
                match self.env {
                    Env::Telegram => net::start_telegram(),
                    #[cfg(feature = "whatsapp")]
                    Env::WhatsApp => net::start_whatsapp(None),
                    #[cfg(not(feature = "whatsapp"))]
                    Env::WhatsApp => {}
                }
                Task::none()
            }
            Msg::Open(pid) => self.open_chat(pid),
            Msg::Filter(s) => {
                self.filter = s;
                Task::none()
            }
            Msg::Compose(s) => {
                let cap = self.app.compose_cap();
                self.app.compose = s.chars().take(cap).collect();
                self.typed();
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
                        model::RowKind::Text(t) => Some(t.clone()),
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
                    self.app.status = tr("cancello il messaggio…").into();
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
                        self.app.status = tr("copiato negli appunti").into();
                        iced::clipboard::write(t)
                    }
                    None => Task::none(),
                }
            }
            Msg::Save(id) => {
                let job = self.row_media(id, |k| match k {
                    model::RowKind::Doc { media, name, .. } => Some((media.clone(), name.clone())),
                    _ => None,
                });
                if let (Some((media, name)), Some(req)) = (job, self.req()) {
                    self.app.status = trf!("salvo «{}» in ~/Downloads…", name);
                    let _ = req.send(Request::Save { media, name });
                }
                Task::none()
            }
            Msg::Play(id) => {
                let job = self.row_media(id, |k| match k {
                    model::RowKind::Audio { media, name, voice, .. } => {
                        let label = if *voice { "voce".to_string() } else { name.clone() };
                        let fname = if *voice {
                            format!("voce-{}.ogg", media.key())
                        } else {
                            paths::sanitize_file_name(name).unwrap_or_else(|| "brano".into())
                        };
                        Some((media.clone(), fname, label, false))
                    }
                    model::RowKind::Video { media, .. } => Some((
                        media.clone(),
                        format!("video-{}.mp4", media.key()),
                        "video".to_string(),
                        true,
                    )),
                    _ => None,
                });
                if let (Some((media, fname, label, video)), Some(req)) = (job, self.req()) {
                    self.app.status = trf!("scarico «{}» e la riproduco…", label);
                    let _ = req.send(Request::Play { media, fname, label, video });
                }
                Task::none()
            }
            Msg::Attach => {
                if self.app.open.is_none() || self.app.editing.is_some() {
                    return Task::none();
                }
                // portale xdg (FileChooser di GTK): niente librerie GTK nel processo
                let pick = rfd::AsyncFileDialog::new().set_title(tr("Allega un file")).pick_file();
                Task::perform(pick, |f| Msg::Picked(f.map(|f| f.path().to_path_buf())))
            }
            Msg::Picked(Some(path)) | Msg::Dropped(path) => {
                if self.app.open.is_some() && self.app.editing.is_none() {
                    clipboard::attach_file(&mut self.app, path);
                }
                operation::focus(COMPOSE)
            }
            Msg::Picked(None) => operation::focus(COMPOSE),
            Msg::Pasted(new) => {
                if self.app.open.is_none() || self.app.editing.is_some() {
                    self.app.compose = new;
                    return Task::none();
                }
                self.paste_undo = Some(std::mem::replace(&mut self.app.compose, new));
                if let Some(link) = &self.link {
                    clipboard::ctrl_paste(&mut self.app, &link.res);
                }
                Task::none()
            }
            Msg::PasteKey => {
                self.paste_undo = None;
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
                }
                // come Telegram: la prima volta si apre sui preferiti;
                // WhatsApp non ha set né preferiti, solo i recenti
                if self.app.show_sticker && self.app.open_set.is_none() {
                    let first = if self.env == Env::WhatsApp { model::RECENT_SET } else { model::FAVED_SET };
                    return self.handle(Msg::OpenSet(first));
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
                    .map(|s| s.short_name.clone())
                    .or_else(|| (set_id < 0).then(String::new));
                if !self.app.set_docs.contains_key(&set_id) && !self.app.docs_cooling(set_id) {
                    if let (Some(short), Some(req)) = (short, self.req()) {
                        let _ = req.send(Request::SetDocs { set_id, short });
                        self.app.status = tr("scarico il set…").into();
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
                    self.app.status = tr("invio sticker…").into();
                    return self.snap_bottom();
                }
                Task::none()
            }
            Msg::Link(url) => {
                match links::parse_link(&url).filter(|_| self.settings.links_in_app) {
                    Some(link) => self.follow(link),
                    None => links::open_external(&url),
                }
                Task::none()
            }
            Msg::Tick => {
                let now = std::time::Instant::now();
                // fermo da 5 s: non sto più scrivendo
                if self.typing_sent.is_some() && now.duration_since(self.last_key).as_secs() >= 5 {
                    self.stop_typing();
                }
                self.app.typing.retain(|_, (until, _)| *until > now);
                if let Some(req) = self.req() {
                    self.app.flush_read_mark(&req);
                }
                let stamp = style::omarchy_stamp();
                if stamp != self.theme_stamp {
                    self.theme_stamp = stamp;
                    apply_theme(self.settings.active_theme());
                }
                Task::none()
            }
            Msg::ToggleSettings => {
                self.show_settings = !self.show_settings;
                if self.show_settings {
                    self.tg_handler = settings::tg_handler();
                    self.settings_page = SettingsPage::Main;
                    self.show_info = false;
                    if let Some(req) = self.req() {
                        let _ = req.send(Request::Profile);
                    }
                }
                Task::none()
            }
            Msg::Page(page) => {
                self.settings_page = page;
                match page {
                    SettingsPage::Account => {
                        if let Some(p) = &self.app.profile {
                            self.pf = p.clone();
                        }
                    }
                    SettingsPage::Devices => {
                        if let Some(req) = self.req() {
                            let _ = req.send(Request::Sessions);
                        }
                    }
                    SettingsPage::Storage => self.measure_cache(),
                    _ => {}
                }
                Task::none()
            }
            Msg::PfFirst(v) => {
                self.pf.first = v.chars().take(64).collect();
                Task::none()
            }
            Msg::PfLast(v) => {
                self.pf.last = v.chars().take(64).collect();
                Task::none()
            }
            Msg::PfUser(v) => {
                self.pf.username = v.trim_start_matches('@').chars().take(32).collect();
                Task::none()
            }
            Msg::PfAbout(v) => {
                self.pf.about = v.chars().take(70).collect();
                Task::none()
            }
            Msg::PfSave => {
                if self.pf.first.trim().is_empty() {
                    self.app.status = tr("il nome non può essere vuoto").into();
                    return Task::none();
                }
                let old_username = self.app.profile.as_ref().map(|p| p.username.clone()).unwrap_or_default();
                if let Some(req) = self.req() {
                    let _ = req.send(Request::SaveProfile { profile: self.pf.clone(), old_username });
                    self.app.status = tr("salvo il profilo…").into();
                }
                Task::none()
            }
            Msg::PfPhoto => {
                let pick = rfd::AsyncFileDialog::new()
                    .set_title(tr("Nuova foto profilo"))
                    .add_filter(tr("Immagini"), &["jpg", "jpeg", "png"])
                    .pick_file();
                return Task::perform(pick, |f| Msg::PfPhotoPicked(f.map(|f| f.path().to_path_buf())));
            }
            Msg::PfPhotoPicked(Some(path)) => {
                if let Some(req) = self.req() {
                    let _ = req.send(Request::ProfilePhoto(path));
                    self.app.status = tr("carico la foto profilo…").into();
                }
                Task::none()
            }
            Msg::PfPhotoPicked(None) => Task::none(),
            Msg::AutoPhotos(on) => {
                self.settings.auto_photos = on;
                self.media_changed()
            }
            Msg::VideoThumbs(on) => {
                self.settings.video_thumbs = on;
                self.media_changed()
            }
            Msg::Avatars(on) => {
                self.settings.avatars = on;
                self.media_changed()
            }
            Msg::PhotoSize(px) => {
                self.settings.photo_px = px;
                self.settings.save();
                Task::none()
            }
            Msg::CacheLimit(mb) => {
                self.settings.cache_mb = mb;
                self.settings.save();
                Task::none()
            }
            Msg::PruneNow => {
                rondine::loader::prune_cache_to(self.settings.cache_mb as u64 * 1024 * 1024);
                // le anteprime tolte dal disco si riscaricano quando servono
                self.app.previews.retain(|_, p| p.exists());
                self.measure_cache();
                self.app.status = tr("cache sfoltita").into();
                Task::none()
            }
            Msg::AskClear(kind) => {
                self.confirm = Some(Confirm::ClearCache(kind));
                Task::none()
            }
            Msg::AskEndSession(hash) => {
                self.confirm = Some(Confirm::EndSession(hash));
                Task::none()
            }
            Msg::LoadPreview(id) => {
                let media = self.row_media(id, |k| rondine::app::preview_media(k).cloned());
                if let (Some(media), Some(req)) = (media, self.req()) {
                    self.app.ensure_previews(&req, [&media]);
                }
                Task::none()
            }
            Msg::Lang(l) => {
                self.settings.lang = settings::LangChoice(l);
                self.settings.save();
                i18n::set(self.settings.lang.resolve());
                Task::none()
            }
            Msg::Translate(id) => {
                if let Some(peer) = self.app.open {
                    self.app.toggle_translation(peer, id);
                    self.app.msg_cursor = None;
                }
                Task::none()
            }
            Msg::AutoTranslate(on) => {
                if let Some(peer) = self.app.open {
                    self.app.translate.set_auto(peer, on);
                    self.save_translate();
                    if on {
                        self.app.auto_translate(peer, rondine::app::AUTO_TRANSLATE_LAST);
                    }
                }
                Task::none()
            }
            Msg::TrProvider(p) => {
                self.app.translate.provider = p;
                self.save_translate();
                Task::none()
            }
            Msg::TrDeeplKey(k) => {
                self.app.translate.deepl_key = k.trim().to_string();
                self.save_translate();
                Task::none()
            }
            Msg::TrUrl(u) => {
                self.app.translate.oa_url = u.trim().to_string();
                self.save_translate();
                Task::none()
            }
            Msg::TrKey(k) => {
                self.app.translate.oa_key = k.trim().to_string();
                self.save_translate();
                Task::none()
            }
            Msg::TrModel(m) => {
                self.app.translate.oa_model = m.trim().to_string();
                self.save_translate();
                Task::none()
            }
            Msg::TrPreset(i) => {
                if let Some((_, url, model)) = translate::PRESETS.get(i) {
                    self.app.translate.oa_url = url.to_string();
                    self.app.translate.oa_model = model.to_string();
                    self.save_translate();
                }
                Task::none()
            }
            Msg::TrTarget(t) => {
                self.app.translate.target = t;
                self.save_translate();
                Task::none()
            }
            Msg::TrTest => {
                if let Some(req) = self.req() {
                    self.app.tr_test = None;
                    let _ = req.send(Request::TranslateTest(Box::new(self.app.translate.clone())));
                    self.app.status = tr("provo la traduzione…").into();
                }
                Task::none()
            }
            Msg::SetStyle(s) => {
                if self.settings.mixed() {
                    self.settings.style_mix = s;
                } else {
                    self.settings.style = s;
                }
                style::set_wa(s == UiStyle::WhatsApp);
                apply_theme(self.settings.active_theme());
                self.theme_stamp = style::omarchy_stamp();
                self.settings.save();
                Task::none()
            }
            Msg::ChatFilter(f) => {
                self.chat_filter = f;
                self.show_archived = false;
                Task::none()
            }
            Msg::ShowArchived(on) => {
                self.show_archived = on;
                self.chat_menu = None;
                Task::none()
            }
            Msg::ChatMenu(pid) => {
                self.chat_menu = if self.chat_menu == pid { None } else { pid };
                Task::none()
            }
            Msg::ChatDo(pid, action) => {
                self.chat_menu = None;
                let target = self.app.contacts.iter().find(|c| model::same_peer(c.peer_id, pid)).map(|c| c.peer_ref);
                let target = target.or_else(|| self.app.current_chat().filter(|c| c.peer_id == pid).map(|c| c.peer_ref));
                if let (Some(peer_ref), Some(req)) = (target, self.req()) {
                    let _ = req.send(Request::ChatAction { peer_ref, peer: pid, action });
                }
                Task::none()
            }
            Msg::ToggleFavorite(pid) => {
                self.chat_menu = None;
                {
                    let id = pid.0;
                    let favs = &mut self.settings.favorites;
                    if favs.contains(&id) {
                        favs.retain(|x| *x != id);
                    } else {
                        favs.push(id);
                    }
                    self.settings.save();
                }
                Task::none()
            }
            Msg::MenuDelete(pid) => {
                self.chat_menu = None;
                let open = self.open_chat(pid);
                self.revoke = false;
                self.confirm = Some(Confirm::DeleteChat);
                return open;
            }
            Msg::Star(id) => {
                if let Some(chat) = self.app.current_chat() {
                    let (peer, peer_ref) = (chat.peer_id, chat.peer_ref);
                    self.app.toggle_star(peer, id);
                    self.app.msg_cursor = None;
                    // su WhatsApp la stella arriva anche al telefono
                    let on = self.app.is_starred(peer, id);
                    if let Some(req) = self.req() {
                        let _ = req.send(Request::Star { peer_ref, id, on });
                    }
                }
                Task::none()
            }
            Msg::ShowStarred(on) => {
                self.show_starred = on;
                if on {
                    self.show_settings = false;
                    self.show_info = false;
                }
                Task::none()
            }
            Msg::OpenStarred(chat, id) => {
                // il messaggio può stare nell'altro ambiente: prima ci si va
                let p = ChatId(chat);
                let elsewhere = !self.app.contacts.iter().any(|c| model::same_peer(c.peer_id, p))
                    && self.parked.as_ref().is_some_and(|k| k.app.contacts.iter().any(|c| model::same_peer(c.peer_id, p)));
                let mut switch = Task::none();
                if elsewhere {
                    let other = if self.env == Env::Telegram { Env::WhatsApp } else { Env::Telegram };
                    switch = self.switch_env(other);
                }
                match Some(ChatId(chat))
                    .filter(|p| self.app.contacts.iter().any(|c| model::same_peer(c.peer_id, *p)))
                {
                    Some(p) => {
                        self.show_starred = false;
                        self.show_archived = self.app.contacts.iter().any(|c| c.peer_id == p && c.archived);
                        self.app.link_open = Some((p, Some(id)));
                    }
                    None => self.app.status = tr("la chat non è più fra le tue").into(),
                }
                // niente return: più sotto il link_open apre la chat
                switch
            }
            Msg::Unstar(chat, id) => {
                self.app.starred.retain(|s| !(s.chat == chat && s.id == id));
                rondine::starred::save(&self.app.starred);
                Task::none()
            }
            Msg::Quit => return iced::exit(),
            Msg::SetTheme(choice) => {
                // il tema vale solo per lo stile in uso
                *self.settings.active_theme_mut() = choice;
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
            Msg::EnvRail(on) => {
                self.settings.env_rail = on;
                self.settings.save();
                Task::none()
            }
            Msg::ShowOnline(on) => {
                self.settings.show_online = on;
                self.settings.save();
                self.send_presence();
                Task::none()
            }
            Msg::ShowTyping(on) => {
                self.settings.show_typing = on;
                self.settings.save();
                if !on {
                    self.stop_typing();
                }
                Task::none()
            }
            Msg::Unified(on) => {
                // si parte dallo stile e dal tema che si stanno guardando
                if on {
                    self.settings.style_mix = self.settings.layout();
                    self.settings.theme_mix = self.settings.active_theme();
                }
                self.settings.unified = on;
                self.settings.save();
                style::set_wa(self.settings.layout() == UiStyle::WhatsApp);
                apply_theme(self.settings.active_theme());
                self.theme_stamp = style::omarchy_stamp();
                Task::none()
            }
            Msg::InEnv(env, m) => match *m {
                // aprire una chat (o eliminarla, con la conferma) porta
                // nel suo ambiente; il resto si fa dietro le quinte
                m @ (Msg::Open(_) | Msg::MenuDelete(_)) => {
                    let switch = self.switch_env(env);
                    let then = self.update(m);
                    Task::batch([switch, then])
                }
                m => self.in_env(env, |g| g.update(m)),
            },
            Msg::Gpu(on) => {
                self.settings.gpu = on;
                self.settings.save();
                self.app.status = tr("vale dal prossimo avvio").into();
                Task::none()
            }
            Msg::LinksInApp(on) => {
                self.settings.links_in_app = on;
                self.settings.save();
                Task::none()
            }
            Msg::TgHandler => {
                if !settings::set_tg_handler() {
                    self.app.status = tr("xdg-mime non ha funzionato").into();
                }
                self.tg_handler = settings::tg_handler();
                Task::none()
            }
            Msg::Join => {
                if let (Some((hash, title, _)), Some(req)) = (self.app.invite.take(), self.req()) {
                    let _ = req.send(Request::Join(hash));
                    self.app.status = trf!("entro in «{}»…", title);
                }
                Task::none()
            }
            Msg::ToggleInfo => {
                self.show_info = !self.show_info && self.app.open.is_some();
                if self.show_info {
                    self.show_settings = false;
                    self.load_info(true);
                }
                Task::none()
            }
            Msg::InfoTab(tab) => {
                self.info_tab = tab;
                self.load_info(false);
                Task::none()
            }
            Msg::AskBlock(on) => {
                self.confirm = Some(Confirm::Block(on));
                Task::none()
            }
            Msg::AskDeleteChat => {
                self.revoke = false;
                self.confirm = Some(Confirm::DeleteChat);
                Task::none()
            }
            Msg::Revoke(on) => {
                self.revoke = on;
                Task::none()
            }
            Msg::ConfirmYes if matches!(self.confirm, Some(Confirm::EndSession(_) | Confirm::ClearCache(_))) => {
                match self.confirm.take() {
                    Some(Confirm::EndSession(hash)) => {
                        if let Some(req) = self.req() {
                            let _ = req.send(Request::EndSession(hash));
                            self.app.status = tr("chiudo la sessione…").into();
                        }
                    }
                    Some(Confirm::ClearCache(kind)) => self.clear_cache(kind),
                    _ => {}
                }
                Task::none()
            }
            Msg::ConfirmYes => {
                let what = self.confirm.take();
                let chat = self.app.current_chat().map(|c| (c.peer_ref, c.peer_id));
                if let (Some(what), Some((peer_ref, peer)), Some(req)) = (what, chat, self.req()) {
                    match what {
                        Confirm::Block(blocked) => {
                            let _ = req.send(Request::Block { peer_ref, peer, blocked });
                            self.app.status = if blocked { tr("blocco l'utente…") } else { tr("sblocco l'utente…") }.into();
                        }
                        Confirm::DeleteChat => {
                            let _ = req.send(Request::DeleteChat { peer_ref, peer, revoke: self.revoke });
                            self.app.status = tr("elimino la chat…").into();
                            self.show_info = false;
                            notify::dismiss(peer);
                        }
                        Confirm::EndSession(_) | Confirm::ClearCache(_) => {}
                    }
                }
                Task::none()
            }
            Msg::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                if key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                    self.cancel();
                }
                // Ctrl+1 / Ctrl+2: Telegram o WhatsApp, anche senza la barra
                match key.as_ref() {
                    keyboard::Key::Character("1") if modifiers.command() => self.switch_env(Env::Telegram),
                    keyboard::Key::Character("2") if modifiers.command() => self.switch_env(Env::WhatsApp),
                    _ => Task::none(),
                }
            }
            Msg::Key(_) => Task::none(),
        };
        self.after_change();
        // un link risolto: la chat si apre appena è nella lista
        if let Some((pid, post)) = self.app.link_open {
            if self.app.contacts.iter().any(|c| model::same_peer(c.peer_id, pid)) {
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
    fn follow(&mut self, link: links::TgLink) {
        if let links::TgLink::Private { channel, post } = link {
            let pid = links::channel_chat(channel);
            match pid.filter(|p| self.app.contacts.iter().any(|c| model::same_peer(c.peer_id, *p))) {
                Some(p) => self.app.link_open = Some((p, post)),
                None => self.app.status = tr("link a una chat privata che non è fra le tue").into(),
            }
            return;
        }
        if let Some(req) = self.req() {
            let _ = req.send(Request::Link(link));
            self.app.status = tr("apro il link…").into();
        }
    }

    /// Link da fuori (argomento o altra istanza): come un click in chat,
    /// ma sempre dentro Rondine.
    fn external_link(&mut self, url: String) {
        if self.link.is_none() {
            self.pending_links.push(url);
            return;
        }
        match links::parse_link(&url) {
            Some(link) => self.follow(link),
            None => self.app.status = trf!("link non riconosciuto: {}", url),
        }
    }

    /// Esc o ✕: chiude il primo pannello o annulla l'azione in corso.
    fn cancel(&mut self) {
        if self.viewer.take().is_some() {
            return;
        }
        if self.confirm.take().is_some() {
            return;
        }
        if self.chat_menu.take().is_some() {
            return;
        }
        if std::mem::take(&mut self.show_starred) {
            return;
        }
        if self.forwarding.take().is_some() {
            return;
        }
        if self.show_settings {
            self.show_settings = false;
            return;
        }
        if self.show_info {
            self.show_info = false;
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
        } else if a.pending_photo.is_some() || a.pending_file.is_some() {
            a.discard_pending_photo();
        } else {
            a.msg_cursor = None;
        }
        a.status.clear();
    }

    /// La riga `id` della chat aperta: dalla cronologia o dai contenuti
    /// condivisi della scheda (media vecchi non ancora caricati).
    fn row_media<T>(&self, id: i32, f: impl Fn(&model::RowKind) -> Option<T>) -> Option<T> {
        let peer = self.app.open?;
        self.app
            .open_history()
            .and_then(|ring| ring.iter().find(|r| r.id == id))
            .or_else(|| {
                self.app
                    .shared
                    .iter()
                    .filter(|((p, _), _)| *p == peer)
                    .flat_map(|(_, items)| items.iter().map(|it| &it.row))
                    .find(|r| r.id == id)
            })
            .and_then(|r| f(&r.kind))
    }

    /// Salva la configurazione della traduzione (file 0600, con le chiavi).
    fn save_translate(&mut self) {
        self.app.tr_test = None;
        if let Err(e) = self.app.translate.save() {
            self.app.status = trf!("traduzione: {}", e);
        }
    }

    /// Le preferenze dei media cambiate: salvate e passate allo stato.
    fn media_changed(&mut self) -> Task<Msg> {
        self.settings.save();
        self.app.media_prefs = self.settings.media_prefs();
        Task::none()
    }

    fn measure_cache(&mut self) {
        for (i, kind) in [CacheKind::Media, CacheKind::Avatars, CacheKind::Temp].into_iter().enumerate() {
            self.cache_usage[i] = rondine::loader::dir_usage(&kind.dir());
        }
    }

    /// Svuota una parte della cache; quello che serve si riscarica.
    fn clear_cache(&mut self, kind: CacheKind) {
        let a = &mut self.app;
        match kind {
            CacheKind::Media => {
                rondine::loader::clear_dir(&kind.dir());
                a.previews.clear();
                a.failed.clear();
            }
            CacheKind::Avatars => {
                rondine::loader::clear_dir(&kind.dir());
                a.avatars.clear();
                a.avatar_asked.clear();
            }
            CacheKind::Temp => {
                // un invio o un allegato in attesa usano i temporanei
                if a.uploading.is_some() || a.pending_photo.is_some() {
                    a.status = tr("c'è un invio in corso: riprova dopo").into();
                    return;
                }
                rondine::loader::clear_dir(&kind.dir());
            }
        }
        a.status = tr("cache svuotata").into();
        self.measure_cache();
    }

    /// Chiede la scheda della chat aperta e i contenuti della scheda scelta;
    /// `fresh`: all'apertura si rileggono anche quelli già in memoria.
    fn load_info(&mut self, fresh: bool) {
        let (Some(chat), Some(req)) = (self.app.current_chat(), self.req()) else {
            return;
        };
        let (peer_ref, peer) = (chat.peer_ref, chat.peer_id);
        if fresh {
            let _ = req.send(Request::Info { peer_ref, peer });
            self.app.shared_asked.retain(|(p, _)| *p != peer);
        }
        if self.app.shared_asked.insert((peer, self.info_tab)) {
            let _ = req.send(Request::Shared { peer_ref, peer, tab: self.info_tab });
        }
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
        self.app.flush_translations(&req);
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
            NetEvent::Status(step, s) => {
                self.phase = Phase::Connecting(step, s);
                Task::none()
            }
            NetEvent::Failed(e) => {
                self.phase = Phase::Failed(e);
                if self.env == Env::WhatsApp {
                    self.wa_since = None;
                }
                Task::none()
            }
            NetEvent::Login(l) => {
                self.phase = Phase::Login(l);
                operation::focus(LOGIN_FIELD)
            }
            NetEvent::Pair(p) => {
                // WhatsApp ha risposto: niente avviso di attesa
                self.wa_since = None;
                match p {
                    Pairing::Qr(qr) => {
                        self.phase = Phase::Pairing { qr: iced::widget::qr_code::Data::new(qr).ok(), code: None }
                    }
                    Pairing::Code(code) => self.phase = Phase::Pairing { qr: None, code: Some(code) },
                    Pairing::Paired => {
                        self.phase = Phase::Connecting(1, tr("Abbinato! Scarico le chat dal telefono…").into())
                    }
                    // durante l'abbinamento resta il QR; prima, la riga di stato
                    Pairing::Status(s) => {
                        if !matches!(self.phase, Phase::Pairing { .. }) {
                            self.phase = Phase::Connecting(0, s);
                        }
                    }
                }
                Task::none()
            }
            NetEvent::Ready { link, contacts, me } => {
                if self.env == Env::WhatsApp {
                    self.wa_since = None;
                }
                self.app.contacts = contacts;
                self.app.me = me;
                // online come la finestra (e come vuole l'impostazione)
                let _ = link.req.send(Request::Presence(self.focused && self.settings.show_online));
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
            NetEvent::Response(rondine::send::Response::Live(u)) => {
                let Some(link) = &self.link else {
                    return Task::none();
                };
                let req = link.req.clone();
                // messaggio in arrivo: chi e da dove, prima che l'evento sparisca
                let incoming = match &u {
                    model::Live::NewMessage { chat, row, .. } if !row.outgoing => Some((*chat, row.from.clone())),
                    _ => None,
                };
                // letta da un'altra sessione: la notifica non serve più
                let read_elsewhere = match &u {
                    model::Live::ReadInbox { chat, .. } => Some(*chat),
                    _ => None,
                };
                handle_live(&mut self.app, u, &req);
                if let Some((peer, from)) = incoming {
                    self.notify(peer, from);
                }
                if let Some(peer) = read_elsewhere {
                    let read = self
                        .app
                        .contacts
                        .iter()
                        .find(|d| model::same_peer(d.peer_id, peer))
                        .is_some_and(|d| d.unread == 0);
                    if read {
                        notify::dismiss(peer);
                    }
                }
                Task::none()
            }
            NetEvent::Response(r) => {
                let Some(link) = &self.link else {
                    return Task::none();
                };
                let req = link.req.clone();
                if let rondine::send::Response::Clipboard(hit) = &r {
                    match (hit, self.paste_undo.take()) {
                        // il testo l'ha già messo iced nel campo
                        (clipboard::ClipboardHit::Text(_) | clipboard::ClipboardHit::Empty, Some(_)) => {
                            self.app.status.clear();
                            return Task::none();
                        }
                        (_, Some(before)) => self.app.compose = before,
                        (_, None) => {}
                    }
                }
                let first_page = matches!(&r, rondine::send::Response::Hist { .. });
                let profile = matches!(&r, rondine::send::Response::Profile(Ok(_)));
                // WhatsApp scollegato da qui: si torna alla pagina per collegarlo
                if self.env == Env::WhatsApp && matches!(&r, rondine::send::Response::SessionEnded(Ok(_))) {
                    self.app = new_app(&self.settings);
                    self.link = None;
                    self.phase = Phase::Linking;
                    self.show_settings = false;
                    return Task::none();
                }
                handle_response(&mut self.app, r, &req);
                if profile {
                    if let Some(p) = &self.app.profile {
                        self.pf = p.clone();
                    }
                }
                if first_page || self.at_bottom {
                    return self.snap_bottom();
                }
                Task::none()
            }
        }
    }

    /// Due ambienti, insieme o in due istanze: il marchio e i temi dicono
    /// di quale si tratta.
    /// Il feed unico è attivo: una lista sola con le chat dei due ambienti.
    pub fn feed(&self) -> bool {
        self.settings.mixed() && self.parked.is_some()
    }

    pub fn other_env(&self) -> Env {
        if self.env == Env::Telegram { Env::WhatsApp } else { Env::Telegram }
    }

    pub fn two_envs(&self) -> bool {
        self.parked.is_some() || settings::solo().is_some()
    }

    /// Passa all'altro ambiente: ognuno tiene le sue chat, il suo stile e
    /// il suo tema; la lista dei messaggi importanti è una sola e viene via.
    fn switch_env(&mut self, to: Env) -> Task<Msg> {
        if to == self.env {
            return Task::none();
        }
        if self.parked.is_none() {
            return Task::none();
        }
        // il «sta scrivendo» va annullato dal servizio a cui è stato mandato
        self.stop_typing();
        let Some(mut p) = self.parked.take() else {
            return Task::none();
        };
        std::mem::swap(&mut self.app, &mut p.app);
        std::mem::swap(&mut self.link, &mut p.link);
        std::mem::swap(&mut self.phase, &mut p.phase);
        self.app.starred = std::mem::take(&mut p.app.starred);
        // la chat aperta di là non si vede più: i nuovi arrivati contano
        p.app.away = true;
        self.app.away = false;
        self.parked = Some(p);
        self.env = to;
        // e quella di qua torna in vista: quanto arrivato intanto è letto
        let back = self.app.current_chat().filter(|d| d.unread > 0).map(|d| d.peer_ref);
        if let (Some(chat), Some(req)) = (back, self.req()) {
            rondine::sync::mark_read(&req, chat);
            if let Some(d) = self.app.contacts.iter_mut().find(|d| model::same_peer(d.peer_id, chat.id)) {
                d.unread = 0;
            }
        }
        self.settings.style = to;
        self.settings.save();
        style::set_wa(self.settings.layout() == Env::WhatsApp);
        apply_theme(self.settings.active_theme());
        self.theme_stamp = style::omarchy_stamp();
        let mixed = self.settings.mixed();
        // quello che era aperto riguardava l'altro ambiente
        self.show_info = false;
        self.show_settings = false;
        self.show_starred = false;
        self.chat_menu = None;
        self.confirm = None;
        self.forwarding = None;
        self.viewer = None;
        // col feed unico la lista (ricerca, archivio) è la stessa
        if !mixed {
            self.show_archived = false;
            self.filter.clear();
        }
        self.snap_bottom()
    }

    /// Esegue `f` sull'ambiente `env`: quello in vista direttamente, l'altro
    /// scambiandolo per un attimo (i task di scorrimento e fuoco si buttano).
    fn in_env(&mut self, env: Env, f: impl FnOnce(&mut Self) -> Task<Msg>) -> Task<Msg> {
        if env == self.env {
            return f(self);
        }
        let Some(mut p) = self.parked.take() else {
            return Task::none();
        };
        std::mem::swap(&mut self.app, &mut p.app);
        std::mem::swap(&mut self.link, &mut p.link);
        std::mem::swap(&mut self.phase, &mut p.phase);
        let (env_was, at_bottom) = (self.env, self.at_bottom);
        self.env = env;
        self.background = true;
        let _ = f(self);
        self.background = false;
        self.env = env_was;
        self.at_bottom = at_bottom;
        std::mem::swap(&mut self.app, &mut p.app);
        std::mem::swap(&mut self.link, &mut p.link);
        std::mem::swap(&mut self.phase, &mut p.phase);
        self.parked = Some(p);
        Task::none()
    }

    /// Notifica di un messaggio nuovo: non per le chat silenziate né per
    /// quella aperta mentre la finestra è a fuoco.
    fn notify(&self, peer: ChatId, from: String) {
        // gli stati non suonano, come nell'app
        if !self.settings.notify || peer == model::WA_STATUS || (self.focused && !self.background && self.app.open == Some(peer)) {
            return;
        }
        let Some(d) = self.app.contacts.iter().find(|d| model::same_peer(d.peer_id, peer)) else {
            return;
        };
        if d.muted {
            return;
        }
        let text = match (&d.last, self.settings.notify_text) {
            (Some(l), true) => l.text.clone(),
            _ => tr("nuovo messaggio").to_string(),
        };
        let body = if d.is_group && !d.broadcast && !from.is_empty() {
            format!("{from}: {text}")
        } else {
            text
        };
        let icon = d.photo_id.and_then(|id| self.app.avatars.get(&id));
        notify::show(self.env, peer, &d.name, &body, icon.map(|p| p.as_path()));
    }

    fn open_chat(&mut self, pid: ChatId) -> Task<Msg> {
        notify::dismiss(pid);
        let Some(i) = self.app.contacts.iter().position(|c| model::same_peer(c.peer_id, pid)) else {
            return Task::none();
        };
        self.app.contact_cursor = i;
        self.show_settings = false;
        self.show_starred = false;
        self.chat_menu = None;
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
        a.status = tr("carico la cronologia…").into();
        a.editing = None;
        a.replying = None;
        a.disarm_delete();
        a.msg_cursor = None;
        a.discard_pending_photo();
        a.compose.clear();
        a.show_sticker = false;
        self.confirm = None;
        if self.show_info {
            self.load_info(true);
        }
        Task::batch([self.snap_bottom(), operation::focus(COMPOSE)])
    }

    /// Invio nel campo di scrittura: foto in attesa, modifica o testo nuovo.
    /// Online sui due servizi: con la finestra davanti, se si vuole farsi
    /// vedere. (Su WhatsApp, da «non disponibile», non arriva nemmeno
    /// l'online degli altri.)
    fn send_presence(&self) {
        let on = self.focused && self.settings.show_online;
        for l in self.link.iter().chain(self.parked.as_ref().and_then(|p| p.link.as_ref())) {
            let _ = l.req.send(Request::Presence(on));
        }
    }

    /// Un tasto nel campo: «sta scrivendo» alla chat aperta, ripetuto
    /// ogni 4 s (Telegram lo fa scadere dopo 6); campo vuoto: smesso.
    fn typed(&mut self) {
        self.last_key = std::time::Instant::now();
        if self.app.compose.trim().is_empty() {
            self.stop_typing();
            return;
        }
        let Some(peer) = self.app.current_chat().map(|c| c.peer_ref) else { return };
        if !self.settings.show_typing || self.app.editing.is_some() {
            return;
        }
        let fresh = self
            .typing_sent
            .is_some_and(|(p, at)| p.id == peer.id && at.elapsed().as_secs() < 4);
        if fresh {
            return;
        }
        if self.typing_sent.is_some_and(|(p, _)| p.id != peer.id) {
            self.stop_typing();
        }
        if let Some(req) = self.req() {
            let _ = req.send(Request::Typing { peer_ref: peer, on: true });
            self.typing_sent = Some((peer, std::time::Instant::now()));
        }
    }

    fn stop_typing(&mut self) {
        let Some((peer, _)) = self.typing_sent.take() else { return };
        if let Some(req) = self.req() {
            let _ = req.send(Request::Typing { peer_ref: peer, on: false });
        }
    }

    fn submit(&mut self) -> Task<Msg> {
        self.stop_typing();
        let Some(req) = self.req() else {
            return Task::none();
        };
        let Some((peer, peer_id)) = self.app.current_chat().map(|c| (c.peer_ref, c.peer_id)) else {
            return Task::none();
        };
        let a = &mut self.app;
        let kind = if let Some(path) = a.pending_photo.take() {
            a.status.clear();
            a.uploading = Some(tr("immagine dagli appunti").into());
            SendKind::Photo { path, caption: std::mem::take(&mut a.compose) }
        } else if let Some(path) = a.pending_file.take() {
            a.status.clear();
            a.uploading = path.file_name().map(|n| n.to_string_lossy().into_owned());
            SendKind::File { path: path.display().to_string(), caption: std::mem::take(&mut a.compose) }
        } else if let Some(id) = a.editing {
            if a.compose.trim().is_empty() {
                a.status = tr("testo vuoto: niente da modificare").into();
                return Task::none();
            }
            a.editing = None;
            a.status = tr("salvo la modifica…").into();
            SendKind::EditText { id, text: std::mem::take(&mut a.compose) }
        } else if !a.compose.trim().is_empty() {
            a.status = tr("invio…").into();
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
