//! Impostazioni della GUI, salvate in `~/.config/rtete/gui.conf` come
//! righe `chiave = valore`.

use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ThemeChoice {
    /// i colori del tema Omarchy corrente, aggiornati al cambio tema
    Omarchy,
    /// la palette scura di RTete
    Rtete,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub theme: ThemeChoice,
    /// scala dell'interfaccia (testo e spazi)
    pub scale: f32,
    /// i link t.me / tg:// si aprono in RTete invece che nel browser
    pub links_in_app: bool,
    /// disegno con la GPU (wgpu) invece che in software (tiny-skia)
    pub gpu: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Omarchy,
            scale: 1.0,
            links_in_app: true,
            gpu: true,
        }
    }
}

fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("rtete/gui.conf")
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Self::default();
        let Ok(raw) = std::fs::read_to_string(path()) else {
            return s;
        };
        for (k, v) in raw.lines().filter_map(|l| l.split_once('=')) {
            match (k.trim(), v.trim()) {
                ("tema", "omarchy") => s.theme = ThemeChoice::Omarchy,
                ("tema", "rtete") => s.theme = ThemeChoice::Rtete,
                ("scala", v) => {
                    if let Ok(f) = v.parse::<f32>() {
                        s.scale = f.clamp(0.7, 1.6);
                    }
                }
                ("link_in_app", v) => s.links_in_app = v == "true",
                ("gpu", v) => s.gpu = v == "true",
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
            ThemeChoice::Rtete => "rtete",
        };
        let _ = std::fs::write(
            p,
            format!(
                "tema = {theme}\nscala = {:.2}\nlink_in_app = {}\ngpu = {}\n",
                self.scale, self.links_in_app, self.gpu
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
        .args(["default", "rtete-gui.desktop", "x-scheme-handler/tg"])
        .status()
        .is_ok_and(|s| s.success())
}
