//! Impostazioni della GUI, salvate in `~/.config/rtete/gui.conf` come
//! righe `chiave = valore`.

use std::path::PathBuf;

use rondine::i18n::{self, Lang};

/// La lingua dell'interfaccia: quella del sistema o una scelta.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LangChoice(pub Option<Lang>);

impl LangChoice {
    pub fn resolve(self) -> Lang {
        self.0.unwrap_or_else(i18n::detect)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ThemeChoice {
    /// i colori del tema Omarchy corrente, aggiornati al cambio tema
    Omarchy,
    /// la palette scura di Rondine
    Rondine,
    /// il verde di WhatsApp
    WhatsApp,
}

impl ThemeChoice {
    fn code(self) -> &'static str {
        match self {
            ThemeChoice::Omarchy => "omarchy",
            ThemeChoice::Rondine => "rondine",
            ThemeChoice::WhatsApp => "whatsapp",
        }
    }

    fn from_code(v: &str) -> Option<Self> {
        match v {
            "omarchy" => Some(ThemeChoice::Omarchy),
            "rondine" | "rtete" => Some(ThemeChoice::Rondine),
            "whatsapp" => Some(ThemeChoice::WhatsApp),
            _ => None,
        }
    }
}

/// L'ambiente (Telegram o WhatsApp) e con esso la disposizione
/// dell'interfaccia; ognuno ricorda il suo tema.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum UiStyle {
    Telegram,
    WhatsApp,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub style: UiStyle,
    /// il tema dello stile Telegram
    pub theme: ThemeChoice,
    /// il tema dello stile WhatsApp
    pub theme_wa: ThemeChoice,
    /// chat preferite (id Bot API), per il filtro «Preferite»
    pub favorites: Vec<i64>,
    /// scala dell'interfaccia (testo e spazi)
    pub scale: f32,
    /// i link t.me / tg:// si aprono in Rondine invece che nel browser
    pub links_in_app: bool,
    /// disegno con la GPU (wgpu) invece che in software (tiny-skia)
    pub gpu: bool,
    /// notifiche desktop dei messaggi nuovi
    pub notify: bool,
    /// il testo del messaggio nella notifica (altrimenti solo «nuovo messaggio»)
    pub notify_text: bool,
    /// anteprime delle foto scaricate da sole (altrimenti al click)
    pub auto_photos: bool,
    /// miniature dei video
    pub video_thumbs: bool,
    /// foto profilo nella lista chat
    pub avatars: bool,
    /// lato delle foto in chat, in punti
    pub photo_px: u16,
    /// tetto della cache di foto e sticker, in MB (sfoltita all'avvio)
    pub cache_mb: u32,
    pub lang: LangChoice,
}

/// le misure delle foto in chat proposte nelle impostazioni
pub const PHOTO_SIZES: [(u16, &str); 3] = [(220, "Piccole"), (320, "Medie"), (440, "Grandi")];
/// i tetti della cache proposti nelle impostazioni (MB)
pub const CACHE_LIMITS: [u32; 4] = [32, 128, 512, 2048];

impl Default for Settings {
    fn default() -> Self {
        Self {
            style: UiStyle::Telegram,
            theme: ThemeChoice::Omarchy,
            theme_wa: ThemeChoice::WhatsApp,
            favorites: Vec::new(),
            scale: 1.0,
            links_in_app: true,
            gpu: true,
            notify: true,
            notify_text: true,
            auto_photos: true,
            video_thumbs: true,
            avatars: true,
            photo_px: 320,
            cache_mb: 32,
            lang: LangChoice(None),
        }
    }
}

fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("rondine/gui.conf")
}

impl Settings {
    /// il tema dello stile in uso
    pub fn active_theme(&self) -> ThemeChoice {
        match self.style {
            UiStyle::Telegram => self.theme,
            UiStyle::WhatsApp => self.theme_wa,
        }
    }

    /// il tema dello stile in uso, da cambiare
    pub fn active_theme_mut(&mut self) -> &mut ThemeChoice {
        match self.style {
            UiStyle::Telegram => &mut self.theme,
            UiStyle::WhatsApp => &mut self.theme_wa,
        }
    }

    /// cosa scaricare da solo, per lo stato condiviso
    pub fn media_prefs(&self) -> rondine::app::MediaPrefs {
        rondine::app::MediaPrefs {
            photos: self.auto_photos,
            video_thumbs: self.video_thumbs,
            avatars: self.avatars,
        }
    }

    pub fn load() -> Self {
        let mut s = Self::default();
        let Ok(raw) = std::fs::read_to_string(path()) else {
            return s;
        };
        for (k, v) in raw.lines().filter_map(|l| l.split_once('=')) {
            match (k.trim(), v.trim()) {
                ("tema", v) => s.theme = ThemeChoice::from_code(v).unwrap_or(s.theme),
                ("tema_whatsapp", v) => s.theme_wa = ThemeChoice::from_code(v).unwrap_or(s.theme_wa),
                ("stile", v) => s.style = if v == "whatsapp" { UiStyle::WhatsApp } else { UiStyle::Telegram },
                ("preferite", v) => s.favorites = v.split(',').filter_map(|x| x.trim().parse().ok()).collect(),
                ("scala", v) => {
                    if let Ok(f) = v.parse::<f32>() {
                        s.scale = f.clamp(0.7, 1.6);
                    }
                }
                ("link_in_app", v) => s.links_in_app = v == "true",
                ("gpu", v) => s.gpu = v == "true",
                ("notifiche", v) => s.notify = v == "true",
                ("notifiche_testo", v) => s.notify_text = v == "true",
                ("foto_auto", v) => s.auto_photos = v == "true",
                ("miniature_video", v) => s.video_thumbs = v == "true",
                ("foto_profilo", v) => s.avatars = v == "true",
                ("misura_foto", v) => {
                    if let Ok(n) = v.parse::<u16>() {
                        s.photo_px = n.clamp(160, 600);
                    }
                }
                ("lingua", v) => s.lang = LangChoice(Lang::from_code(v)),
                ("cache_mb", v) => {
                    if let Ok(n) = v.parse::<u32>() {
                        s.cache_mb = n.clamp(16, 8192);
                    }
                }
                _ => {}
            }
        }
        s
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let theme = self.theme.code();
        let style = match self.style {
            UiStyle::Telegram => "telegram",
            UiStyle::WhatsApp => "whatsapp",
        };
        let favorites: Vec<String> = self.favorites.iter().map(|i| i.to_string()).collect();
        let _ = std::fs::write(
            p,
            format!(
                "tema = {theme}\nscala = {:.2}\nlink_in_app = {}\ngpu = {}\nnotifiche = {}\nnotifiche_testo = {}\n\
                 foto_auto = {}\nminiature_video = {}\nfoto_profilo = {}\nmisura_foto = {}\ncache_mb = {}\nlingua = {}\n\
                 stile = {style}\ntema_whatsapp = {}\npreferite = {}\n",
                self.scale,
                self.links_in_app,
                self.gpu,
                self.notify,
                self.notify_text,
                self.auto_photos,
                self.video_thumbs,
                self.avatars,
                self.photo_px,
                self.cache_mb,
                self.lang.0.map_or("auto", |l| l.code()),
                self.theme_wa.code(),
                favorites.join(",")
            ),
        );
    }
}

/// Chi apre oggi i link `tg://` (il .desktop registrato in xdg-mime).
pub fn tg_handler() -> String {
    std::process::Command::new("xdg-mime")
        .args(["query", "default", "x-scheme-handler/tg"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

pub fn set_tg_handler() -> bool {
    std::process::Command::new("xdg-mime")
        .args(["default", "rondine-gui.desktop", "x-scheme-handler/tg"])
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ogni_stile_tiene_il_suo_tema() {
        let mut s = Settings::default();
        assert_eq!(s.active_theme(), ThemeChoice::Omarchy);
        s.style = UiStyle::WhatsApp;
        assert_eq!(s.active_theme(), ThemeChoice::WhatsApp);
        // il tema cambiato nello stile WhatsApp non tocca Telegram
        *s.active_theme_mut() = ThemeChoice::Rondine;
        s.style = UiStyle::Telegram;
        assert_eq!(s.active_theme(), ThemeChoice::Omarchy);
        *s.active_theme_mut() = ThemeChoice::WhatsApp;
        s.style = UiStyle::WhatsApp;
        assert_eq!(s.active_theme(), ThemeChoice::Rondine);
    }

    #[test]
    fn salva_e_rilegge_stile_temi_e_preferite() {
        let dir = std::env::temp_dir().join(format!("rondine-conf-{}", std::process::id()));
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let mut s = Settings::default();
        s.style = UiStyle::WhatsApp;
        s.theme = ThemeChoice::Rondine;
        s.theme_wa = ThemeChoice::Omarchy;
        s.favorites = vec![42, -100123];
        s.save();
        let back = Settings::load();
        assert_eq!(back.style, UiStyle::WhatsApp);
        assert_eq!(back.theme, ThemeChoice::Rondine);
        assert_eq!(back.theme_wa, ThemeChoice::Omarchy);
        assert_eq!(back.favorites, vec![42, -100123]);
        // le impostazioni di prima del cambio di nome restano valide
        std::fs::write(path(), "tema = rtete\n").unwrap();
        let old = Settings::load();
        assert_eq!((old.style, old.theme, old.theme_wa), (UiStyle::Telegram, ThemeChoice::Rondine, ThemeChoice::WhatsApp));
        std::fs::remove_dir_all(&dir).ok();
    }
}
