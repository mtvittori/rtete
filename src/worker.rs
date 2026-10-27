//! I pezzi dei worker uguali per ogni backend: salvataggi, player audio e
//! traduzioni. Il dispatcher vero (un task per job di rete) sta nel
//! backend (`backend::tg::worker`), che passa qui il proprio download.

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::model::ChatId;
use crate::paths;
use crate::send::Response;

/// generazione progressiva del player audio (niente pid esterni)
pub static NEXT_PLAY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Individua un comando nel PATH senza dipendenze esterne.
pub fn which(cmd: &str) -> bool {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|d| std::path::Path::new(d).join(cmd).exists())
}

/// Un salvataggio: scarica in .part e sposta col nome libero (mai
/// sovrascritture, mai parziali col nome finale).
pub async fn save_job<F, Fut>(name: String, download: F) -> Result<String, String>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let Some(sane) = paths::sanitize_file_name(&name) else {
        return Err(crate::tr("nome file vuoto o inutilizzabile").into());
    };
    let dir = paths::downloads_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let tmp = dir.join(format!(".rondine-{ts}.part"));
    let res = match download(tmp.clone()).await {
        Ok(()) => {
            // scaricato per intero: prendi un nome libero e sposta
            let target = paths::unique_save_path(&dir, &sane);
            match std::fs::rename(&tmp, &target) {
                Ok(()) => Ok(target.display().to_string()),
                Err(e) => Err(crate::trf!("spostamento: {}", e)),
            }
        }
        Err(e) => Err(e),
    };
    // il .part avviato non merita di restare a sporcare la cartella
    let _ = tokio::fs::remove_file(&tmp).await;
    res
}

/// Il player audio/video: un solo brano alla volta, il nuovo ferma il
/// vecchio.
#[derive(Default)]
pub struct Player {
    /// sveglia del player audio in corsa: generazione sul watch
    alert: Option<tokio::sync::watch::Sender<u64>>,
}

impl Player {
    /// Scarica con `download` (nel temporaneo che riceve) e riproduce.
    pub fn play<F, Fut>(&mut self, fname: String, label: String, video: bool, out: mpsc::UnboundedSender<Response>, download: F)
    where
        F: FnOnce(PathBuf) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), String>> + Send,
    {
        // sveglia il player vecchio (se c'è): lui chiude
        // figlio e temporanei e non manda l'Ok postumo
        let gen = NEXT_PLAY.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(prev) = self.alert.take() {
            prev.send_replace(gen);
        }
        let (alert, wait) = tokio::sync::watch::channel(gen);
        self.alert = Some(alert);
        // nel nome del temp la generazione: Ctrl-P vicini
        // non si pestano i propri file
        let target = paths::tmp_dir().join(format!("play-{gen}-{fname}"));
        tokio::spawn(async move {
            play_job(download(target.clone()), target, wait, gen, label, video, out).await;
        });
    }
}

/// Riproduzione audio: niente kill esterni col pid (si riusano: uccideva
/// un processo qualsiasi). La generazione gira su un watch: un nuovo
/// Ctrl-P avanza il valore, il task vecchio chiude figlio e temporanei
/// e non manda l'Ok postumo che sovrascriveva lo stato sbagliato.
#[allow(clippy::too_many_arguments)]
pub async fn play_job(
    download: impl Future<Output = Result<(), String>>,
    target: PathBuf,
    mut stop_rx: tokio::sync::watch::Receiver<u64>,
    gen: u64,
    label: String,
    video: bool,
    out: mpsc::UnboundedSender<Response>,
) {
    let _ = out.send(Response::Played(Ok(format!("▶ {label}"))));
    if *stop_rx.borrow() != gen {
        // già sostituito: via il temporaneo e chiudi
        let _ = tokio::fs::remove_file(&target).await;
        return;
    }
    if let Err(e) = download.await {
        let _ = tokio::fs::remove_file(&target).await;
        let _ = out.send(Response::Played(Err(e)));
        return;
    }
    if *stop_rx.borrow() != gen {
        let _ = tokio::fs::remove_file(&target).await;
        return;
    }
    let player = crate::env_var("PLAYER")
        .or_else(|| ["mpv", "ffplay"].iter().find(|p| which(p)).map(|p| p.to_string()));
    let Some(p) = player else {
        let _ = out.send(Response::Played(Err(crate::tr("nessun player: installa mpv o ffplay").into())));
        return;
    };
    let is_mpv = std::path::Path::new(&p)
        .file_name()
        .map(|f| f == std::ffi::OsStr::new("mpv"))
        .unwrap_or(false);
    let mut cmd = tokio::process::Command::new(&p);
    match (is_mpv, video) {
        (true, true) => cmd.args(["--no-terminal", "--really-quiet", "--title=Rondine"]),
        (true, false) => cmd.args(["--no-video", "--no-terminal", "--really-quiet"]),
        (false, true) => cmd.args(["-autoexit", "-loglevel", "quiet"]),
        (false, false) => cmd.args(["-nodisp", "-autoexit", "-loglevel", "quiet"]),
    };
    cmd.arg(&target);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    // cintura di sicurezza: allo drop del figlio si chiude da sé
    cmd.kill_on_drop(true);
    let Ok(mut child) = cmd.spawn() else {
        let _ = tokio::fs::remove_file(&target).await;
        let _ = out.send(Response::Played(Err(crate::tr("player: non parte").into())));
        return;
    };
    tokio::select! {
        st = child.wait() => {
            let _ = tokio::fs::remove_file(&target).await;
            // l'exit naturale conta solo senza player più recente
            if st.is_ok() && *stop_rx.borrow() == gen {
                let _ = out.send(Response::Played(Ok(label)));
            }
        }
        _ = stop_rx.changed() => {
            // sostituito da un Ctrl-P nuovo: chiudi senza Ok (il verso
            // arriva dal nuovo player)
            let _ = child.kill().await;
            let _ = tokio::fs::remove_file(&target).await;
        }
    }
}

/// Traduce il testo di un messaggio (`auto`: chat tradotta da sola);
/// `permits` tiene poche traduzioni alla volta, per non sbattere sui
/// limiti del servizio.
pub fn translate_job(
    permits: &Arc<tokio::sync::Semaphore>,
    peer: ChatId,
    id: i32,
    text: String,
    cfg: Box<crate::translate::Config>,
    auto: bool,
    out: mpsc::UnboundedSender<Response>,
) {
    let gate = Arc::clone(permits);
    tokio::spawn(async move {
        let _gate = gate.acquire_owned().await;
        let target = cfg.target_lang();
        let to = target.clone();
        let res = tokio::task::spawn_blocking(move || crate::translate::translate(&cfg, &text, &to))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        let _ = out.send(Response::Translated { peer, id, auto, target, res });
    });
}

/// La frase di prova delle impostazioni (Impostazioni → Traduzione).
pub fn translate_test_job(cfg: Box<crate::translate::Config>, out: mpsc::UnboundedSender<Response>) {
    tokio::spawn(async move {
        let res = tokio::task::spawn_blocking(move || {
            let sample = "Hello! This is a test message from Rondine 🙂";
            crate::translate::translate(&cfg, sample, &cfg.target_lang())
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        let _ = out.send(Response::TranslateTest(res));
    });
}

/// Scarica (con `fetch`, che restituisce l'originale in cache) e converte
/// in PNG/APNG; la risposta porta la chiave della cache.
pub async fn conv_job<Fut>(key: String, fetch: Fut, out: &mpsc::UnboundedSender<Response>)
where
    Fut: Future<Output = Result<PathBuf, String>>,
{
    let res = crate::loader::convert_to_png(&key, fetch).await;
    let _ = out.send(Response::Converted((key, res)));
}
