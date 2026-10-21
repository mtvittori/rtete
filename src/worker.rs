//! Il dispatcher: un task per job di rete, conversioni limitate da un semaforo, player audio.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use grammers_client::media::Media;
use grammers_client::session::types::PeerId;
use tokio::sync::mpsc;

use crate::app::{MAX_HISTORY, OLDER_PAGE};
use crate::loader;
use crate::send::{Request, Response, SendDone, SendJob, SendKind};
use crate::tg;

pub(crate) fn row_from_msg(m: grammers_client::message::Message) -> Result<(PeerId, tg::HistoryRow), String> {
    let peer_id = m.peer_id();
    let from = tg::sender_name(&m);
    let mut row = tg::HistoryRow::from_message(&m, from);
    row.outgoing = true;
    Ok((peer_id, row))
}

/// generazione progressiva del player audio (niente pid esterni)
pub(crate) static NEXT_PLAY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Individua un comando nel PATH senza dipendenze esterne.
pub(crate) fn which(cmd: &str) -> bool {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|d| std::path::Path::new(d).join(cmd).exists())
}

/// Un invio testi/foto/sticker o l'edit di un messaggio (era il vecchio
/// worker: solo estratto in funzione, il dispatcher lo esegue per job).
pub(crate) async fn send_job(
    client: &grammers_client::Client,
    job: SendJob,
) -> Result<SendDone, String> {
    match job.kind {
        SendKind::Text(text) => match tg::send_text(client, job.peer, &text).await {
            Ok(m) => row_from_msg(m).map(|(pid, row)| SendDone::New(pid, row)),
            Err(e) => Err(e.to_string()),
        },
        SendKind::Sticker(media) => match tg::send_sticker(client, job.peer, &media).await {
            Ok(m) => row_from_msg(m).map(|(pid, row)| SendDone::New(pid, row)),
            Err(e) => Err(e.to_string()),
        },
        SendKind::Photo { path, caption } => {
            match tg::send_photo(client, job.peer, std::path::Path::new(&path), &caption).await {
                Ok(m) => {
                    // l'upload chiude il job: il temporaneo non serve più
                    let _ = std::fs::remove_file(&path);
                    row_from_msg(m).map(|(pid, row)| SendDone::New(pid, row))
                }
                Err(e) => Err(e.to_string()),
            }
        }
        SendKind::EditText { id, text } => {
            match tg::edit_text(client, job.peer, id, &text).await {
                Ok(()) => Ok(SendDone::Edit(
                    job.peer_id,
                    tg::HistoryRow {
                        id,
                        outgoing: true,
                        from: String::new(),
                        kind: tg::RowKind::Text(text),
                        reactions: Vec::new(),
                        date: 0,
                        reply_to: None,
                        reply_quote: None,
                        fwd: None,
                        edited: true,
                    },
                )),
                Err(e) => Err(e.to_string()),
            }
        }
    }
}

/// Un salvataggio: scarica in .part e sposta col nome libero (mai
/// sovrascritture, mai parziali col nome finale).
pub(crate) async fn save_job(
    client: &grammers_client::Client,
    media: Media,
    name: String,
) -> Result<String, String> {
    let Some(sane) = tg::sanitize_file_name(&name) else {
        return Err("nome file vuoto o inutilizzabile".into());
    };
    let dir = tg::downloads_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let tmp = dir.join(format!(".rtete-{ts}.part"));
    let res = match tg::download_media_to(client, &media, tmp.clone()).await {
        Ok(()) => {
            // scaricato per intero: prendi un nome libero e sposta
            let target = tg::unique_save_path(&dir, &sane);
            match std::fs::rename(&tmp, &target) {
                Ok(()) => Ok(target.display().to_string()),
                Err(e) => Err(format!("spostamento: {e}")),
            }
        }
        Err(e) => Err(e),
    };
    // il .part avviato non merita di restare a sporcare la cartella
    let _ = tokio::fs::remove_file(&tmp).await;
    res
}

/// Riproduzione audio: niente kill esterni col pid (si riusano: uccideva
/// un processo qualsiasi). La generazione gira su un watch: un nuovo
/// Ctrl-P avanza il valore, il task vecchio chiude figlio e temporanei
/// e non manda l'Ok postumo che sovrascriveva lo stato sbagliato.
pub(crate) async fn play_job(
    client: &grammers_client::Client,
    media: &Media,
    target: PathBuf,
    mut stop_rx: tokio::sync::watch::Receiver<u64>,
    gen: u64,
    label: String,
    out: mpsc::UnboundedSender<Response>,
) {
    let _ = out.send(Response::Played(Ok(format!("▶ {label}"))));
    if *stop_rx.borrow() != gen {
        // già sostituito: via il temporaneo e chiudi
        let _ = tokio::fs::remove_file(&target).await;
        return;
    }
    if let Err(e) = tg::download_media_to(client, media, target.clone()).await {
        let _ = tokio::fs::remove_file(&target).await;
        let _ = out.send(Response::Played(Err(e)));
        return;
    }
    if *stop_rx.borrow() != gen {
        let _ = tokio::fs::remove_file(&target).await;
        return;
    }
    let player = std::env::var("RTETE_PLAYER")
        .ok()
        .or_else(|| ["mpv", "ffplay"].iter().find(|p| which(p)).map(|p| p.to_string()));
    let Some(p) = player else {
        let _ = out.send(Response::Played(Err("nessun player: installa mpv o ffplay".into())));
        return;
    };
    let is_mpv = std::path::Path::new(&p)
        .file_name()
        .map(|f| f == std::ffi::OsStr::new("mpv"))
        .unwrap_or(false);
    let mut cmd = tokio::process::Command::new(&p);
    if is_mpv {
        cmd.args(["--no-video", "--no-terminal", "--really-quiet"]);
    } else {
        cmd.args(["-nodisp", "-autoexit", "-loglevel", "quiet"]);
    }
    cmd.arg(&target);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    // cintura di sicurezza: allo drop del figlio si chiude da sé
    cmd.kill_on_drop(true);
    let Ok(mut child) = cmd.spawn() else {
        let _ = tokio::fs::remove_file(&target).await;
        let _ = out.send(Response::Played(Err("player: non parte".into())));
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

/// Dispatcher: un task per job (i lavori di rete vanno in parallelo);
/// la sola conversione resta limitata a due in trasmissione con un
/// semaforo, come i due vecchi worker (RAM piatta).
pub(crate) fn run_worker(
    client: grammers_client::Client,
    mut req_rx: mpsc::UnboundedReceiver<Request>,
    res_tx: mpsc::UnboundedSender<Response>,
) {
    let cl = client;
    let out = res_tx;
    let conv_permits = Arc::new(tokio::sync::Semaphore::new(2));
    tokio::spawn(async move {
        // sveglia del player audio in corsa: generazione sul watch
        let mut play_alert: Option<tokio::sync::watch::Sender<u64>> = None;
        while let Some(req) = req_rx.recv().await {
            let cl = cl.clone();
            let out = out.clone();
            let permits = Arc::clone(&conv_permits);
            match req {
                Request::Conv(media) => tokio::spawn(async move {
                    let _gate = permits.acquire_owned().await;
                    let key = tg::media_key(&media);
                    let res = loader::convert_to_png(cl, &media, key.clone())
                        .await
                        .map_err(|e| e.to_string());
                    let _ = out.send(Response::Converted((key, res)));
                }),
                Request::Hist { peer_ref, peer } => tokio::spawn(async move {
                    // l'errore viaggia col canale: un guasto di rete non
                    // è storia vuota ("chat senza messaggi")
                    let rows = tg::load_history(&cl, peer_ref, MAX_HISTORY)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = out.send(Response::Hist { peer, rows });
                }),
                Request::HistOlder { peer_ref, peer, before } => tokio::spawn(async move {
                    let rows = tg::load_history_before(&cl, peer_ref, before, OLDER_PAGE)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = out.send(Response::HistOlder { peer, rows });
                }),
                Request::Quoted { peer_ref, peer, ids } => tokio::spawn(async move {
                    let rows = tg::load_by_id(&cl, peer_ref, &ids)
                        .await
                        .map(|found| ids.iter().copied().zip(found).collect())
                        .map_err(|e| e.to_string());
                    let _ = out.send(Response::Quoted { peer, rows });
                }),
                Request::Sets => tokio::spawn(async move {
                    // se fallisce resta None: riaprire con Tab ritenta,
                    // invece di servire per sempre una finta lista vuota
                    let sets = tg::fetch_installed_sets(&cl)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = out.send(Response::Sets(sets));
                }),
                Request::SetDocs { set_id, short } => tokio::spawn(async move {
                    // un errore NON marca il set come caricato: la
                    // griglia riparte al prossimo input, dopo il backoff
                    let docs = tg::fetch_set_docs_by_name(&cl, &short)
                        .await
                        .map(|docs| docs.into_iter().filter_map(tg::media_from_doc).collect())
                        .map_err(|e| e.to_string());
                    let _ = out.send(Response::Docs { set_id, docs });
                }),
                Request::Send(job) => tokio::spawn(async move {
                    let res = send_job(&cl, job).await;
                    let _ = out.send(Response::Sent(res));
                }),
                Request::Del { peer_ref, id } => tokio::spawn(async move {
                    let res = tg::delete_messages(&cl, peer_ref, &[id])
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string());
                    let _ = out.send(Response::Deleted(res));
                }),
                Request::Search { peer_ref, query } => tokio::spawn(async move {
                    let res = tg::search_chat(&cl, peer_ref, &query, 40)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = out.send(Response::Searched(res));
                }),
                Request::React { peer_ref, msg_id, emoticon } => tokio::spawn(async move {
                    let res = tg::send_reaction(&cl, peer_ref, msg_id, emoticon.as_deref())
                        .await
                        .map_err(|e| e.to_string());
                    let _ = out.send(Response::Reacted(res));
                }),
                Request::Save { media, name } => tokio::spawn(async move {
                    let res = save_job(&cl, media, name).await;
                    let _ = out.send(Response::Saved(res));
                }),
                Request::Play { media, fname, label } => {
                    // sveglia il player vecchio (se c'è): lui chiude
                    // figlio e temporanei e non manda l'Ok postumo
                    let gen = NEXT_PLAY.fetch_add(1, Ordering::Relaxed) + 1;
                    if let Some(prev) = play_alert.take() {
                        prev.send_replace(gen);
                    }
                    let (alert, wait) = tokio::sync::watch::channel(gen);
                    play_alert = Some(alert);
                    // nel nome del temp la generazione: Ctrl-P vicini
                    // non si pestano i propri file
                    let target = tg::tmp_dir().join(format!("play-{gen}-{fname}"));
                    tokio::spawn(async move {
                        play_job(&cl, &media, target, wait, gen, label, out).await;
                    })
                }
            };
        }
    });
}
