//! Cartelle e file dell'app (XDG su Linux/macOS, %APPDATA%/%LOCALAPPDATA% su
//! Windows), uguali per ogni backend: config, dati, cache degli asset,
//! temporanei e salvataggi.

use std::path::{Path, PathBuf};

/// La home dell'utente: HOME, su Windows USERPROFILE.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Il valore di una variabile d'ambiente, solo se è un percorso assoluto.
fn abs_env(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).map(PathBuf::from).filter(|p| p.is_absolute())
}

/// Una variabile di Windows (APPDATA, LOCALAPPDATA); altrove None.
fn windows_env(key: &str) -> Option<PathBuf> {
    if cfg!(windows) { abs_env(key) } else { None }
}

fn config_base() -> PathBuf {
    // XDG (solo se assoluta), su Windows %APPDATA%, altrimenti ~/.config
    abs_env("XDG_CONFIG_HOME")
        .or_else(|| windows_env("APPDATA"))
        .or_else(|| home().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_dir() -> PathBuf {
    config_base().join(APP_DIR)
}

/// il nome delle cartelle dell'app (config e dati)
const APP_DIR: &str = "rondine";
/// le cartelle e la sessione di quando l'app si chiamava RTete
const OLD_DIR: &str = "rtete";
/// la sessione Telegram (sqlite)
pub const DB_FILE: &str = "rondine.db";
const OLD_DB_FILE: &str = "rtete.db";

/// Prima del cambio di nome tutto stava in `rtete/`: al primo avvio di
/// Rondine le cartelle (credenziali, sessione, cache, impostazioni) si
/// spostano, così non serve rifare l'accesso. Da chiamare prima di tutto.
pub fn migrate_old_dirs() {
    for base in [config_base(), data_base()] {
        let (old, new) = (base.join(OLD_DIR), base.join(APP_DIR));
        if old.is_dir() && !new.exists() {
            let _ = std::fs::rename(&old, &new);
        }
    }
    // la sessione sqlite con i suoi file di journal
    let dir = data_dir();
    for ext in ["", "-wal", "-shm", "-journal"] {
        let old = dir.join(format!("{OLD_DB_FILE}{ext}"));
        let new = dir.join(format!("{DB_FILE}{ext}"));
        if old.exists() && !new.exists() {
            let _ = std::fs::rename(&old, &new);
        }
    }
}

fn data_base() -> PathBuf {
    // XDG (solo se assoluta), su Windows %LOCALAPPDATA%, altrimenti ~/.local/share
    abs_env("XDG_DATA_HOME")
        .or_else(|| windows_env("LOCALAPPDATA"))
        .or_else(|| home().map(|h| h.join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn data_dir() -> PathBuf {
    data_base().join(APP_DIR)
}

pub fn media_dir() -> PathBuf {
    data_dir().join("assets")
}

/// Cartella temporanea (foto dagli appunti, audio in riproduzione).
pub fn tmp_dir() -> PathBuf {
    data_dir().join("tmp")
}

/// Dove sta in cache la foto profilo piccola.
pub fn avatar_path(photo_id: i64) -> PathBuf {
    media_dir().join("avatars").join(format!("{photo_id}.jpg"))
}

/// Cartella dei salvataggi: XDG_DOWNLOAD_DIR (shell-quoted in user-dirs.dirs,
/// tipicamente "$HOME/Scaricati") altrimenti ~/Downloads (su Windows %USERPROFILE%\Downloads).
pub fn downloads_dir() -> PathBuf {
    let home = home().unwrap_or_else(|| PathBuf::from("."));
    if let Ok(v) = std::env::var("XDG_DOWNLOAD_DIR") {
        let v = v.trim().replace(['"', '\''], "");
        if !v.is_empty() {
            let v = v.replace("$HOME", &home.to_string_lossy());
            if std::path::Path::new(&v).is_absolute() {
                return PathBuf::from(v);
            }
        }
    }
    home.join("Downloads")
}

/// Nome file sanity per i salvataggi: via slash, punti elusivi e vuoto.
/// None se il nome resta privo di senso (es. era solo ".", " ", "…").
pub fn sanitize_file_name(name: &str) -> Option<String> {
    let safe: String = name.chars().filter(|c| !c.is_control()).collect();
    let safe = safe.replace(['/', '\\'], "_").replace("..", "_");
    let safe = safe.trim().trim_end_matches('.').trim().to_string();
    if safe.is_empty() || safe == "." {
        None
    } else {
        Some(safe)
    }
}

/// "report.pdf" già presente? prova "report (1).pdf", poi (2)… Non
/// sovrascrive mai un file esistente; oltre 999 tentativi, usa il tempo
/// corrente nel nome.
pub fn unique_save_path(dir: &Path, name: &str) -> PathBuf {
    if !dir.join(name).exists() {
        return dir.join(name);
    }
    let (stem, ext) = match name.rfind('.') {
        Some(0) | None => (name.to_string(), String::new()),
        Some(i) => (name[..i].to_string(), name[i..].to_string()),
    };
    for n in 1..1000u32 {
        let cand = dir.join(format!("{stem} ({n}){ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    dir.join(format!("{stem}-{ts}{ext}"))
}

/// Crea (o ripara) una cartella dati a 0700. Anche su cartelle già esistenti
/// la chmod va risistemata: le installazioni vecchie l'hanno creata con la
/// umask corrente (755), e i file dentro restavano leggibili dagli altri.
pub fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// `OpenOptions` per i file privati: su Unix nascono 0600 (mai scritti con
/// la umask e poi sistemati); su Windows valgono i permessi ereditati dalla
/// cartella del profilo utente.
pub fn private_file() -> std::fs::OpenOptions {
    #[cfg_attr(not(unix), allow(unused_mut))] // mut serve solo per la mode su Unix
    let mut opts = std::fs::OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- sanitize_file_name ----------
    #[test]
    fn sanitize_bins_paths_and_dots() {
        assert_eq!(sanitize_file_name("a/b.pdf"), Some("a_b.pdf".into()));
        assert_eq!(sanitize_file_name("a\\b.txt"), Some("a_b.txt".into()));
        assert_eq!(sanitize_file_name("..bad"), Some("_bad".into()));
    }

    #[test]
    fn sanitize_rejects_empty_or_dot() {
        assert!(sanitize_file_name("").is_none());
        assert!(sanitize_file_name("   ").is_none());
        assert!(sanitize_file_name(".").is_none());
    }

    // ---------- unique_save_path ----------
    #[test]
    fn unique_avoids_collisions() {
        let dir = std::env::temp_dir().join(format!("rondine-uniq-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("report.pdf"), b"x").unwrap();
        assert_eq!(
            unique_save_path(&dir, "report.pdf"),
            dir.join("report (1).pdf")
        );
        assert_eq!(unique_save_path(&dir, "altro.txt"), dir.join("altro.txt"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
