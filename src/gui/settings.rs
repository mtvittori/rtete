//! Impostazioni della GUI, salvate in `~/.config/rtete/gui.conf` come
//! righe `chiave = valore`.

use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ThemeChoice {
    /// i colori del tema Omarchy corrente, aggiornati al cambio tema
    Omarchy,
    /// la palette scura di Rondine
    Rondine,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub theme: ThemeChoice,
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
}

/// le misure delle foto in chat proposte nelle impostazioni
pub const PHOTO_SIZES: [(u16, &str); 3] = [(220, "Piccole"), (320, "Medie"), (440, "Grandi")];
/// i tetti della cache proposti nelle impostazioni (MB)
pub const CACHE_LIMITS: [u32; 4] = [32, 128, 512, 2048];

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Omarchy,
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
                ("tema", "omarchy") => s.theme = ThemeChoice::Omarchy,
                ("tema", "rondine" | "rtete") => s.theme = ThemeChoice::Rondine,
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
        let theme = match self.theme {
            ThemeChoice::Omarchy => "omarchy",
            ThemeChoice::Rondine => "rondine",
        };
        let _ = std::fs::write(
            p,
            format!(
                "tema = {theme}\nscala = {:.2}\nlink_in_app = {}\ngpu = {}\nnotifiche = {}\nnotifiche_testo = {}\n\
                 foto_auto = {}\nminiature_video = {}\nfoto_profilo = {}\nmisura_foto = {}\ncache_mb = {}\n",
                self.scale,
                self.links_in_app,
                self.gpu,
                self.notify,
                self.notify_text,
                self.auto_photos,
                self.video_thumbs,
                self.avatars,
                self.photo_px,
                self.cache_mb
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
