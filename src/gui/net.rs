//! Ponte fra la rete e iced: un runtime tokio tutto nostro tiene in piedi
//! grammers e i worker; quello che arriva (update live, risposte dei
//! worker) passa in un solo canale che la subscription di iced legge.

use std::sync::{Arc, Mutex, OnceLock};

use grammers_client::update::Update;
use grammers_client::Client;
use iced::futures::{Stream, StreamExt};
use tokio::sync::mpsc;

use rtete::send::{Request, Response};
use rtete::{app, loader, tg, worker};

/// Il runtime della rete: grammers, worker e `mark_read` girano qui, non
/// nell'executor di iced (così `Handle::enter` basta per gli spawn).
pub static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
/// il ricevitore degli eventi di rete: la subscription lo prende una volta
static EVENTS: Mutex<Option<mpsc::UnboundedReceiver<NetEvent>>> = Mutex::new(None);
/// per chi manda eventi da fuori dal ciclo di rete (click sulle notifiche)
static SENDER: OnceLock<mpsc::UnboundedSender<NetEvent>> = OnceLock::new();

pub fn open_peer(peer: grammers_client::session::types::PeerId) {
    if let Some(tx) = SENDER.get() {
        let _ = tx.send(NetEvent::OpenPeer(peer));
    }
}

pub struct Link {
    pub client: Client,
    pub req: mpsc::UnboundedSender<Request>,
    pub res: mpsc::UnboundedSender<Response>,
}

// come in send.rs: i payload viaggiano interi, una volta per evento
#[allow(clippy::large_enum_variant)]
pub enum NetEvent {
    Status(String),
    Ready {
        link: Link,
        contacts: Vec<tg::DialogInfo>,
        me: String,
    },
    Failed(String),
    Update(Update),
    UpdateErr(String),
    Response(Response),
    /// link Telegram da aprire (argomento o da un'altra istanza)
    Link(String),
    /// click su una notifica: apri la chat
    OpenPeer(grammers_client::session::types::PeerId),
}

/// I messaggi di iced devono essere `Clone`: l'evento viaggia in una
/// scatola condivisa e chi lo riceve lo estrae una volta sola.
#[derive(Clone)]
pub struct Carrier(Arc<Mutex<Option<NetEvent>>>);

impl Carrier {
    pub fn take(&self) -> Option<NetEvent> {
        self.0.lock().ok()?.take()
    }
}

impl std::fmt::Debug for Carrier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Carrier")
    }
}

fn sock_path() -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("rtete-gui.sock")
}

/// Istanza unica: se RTete è già aperta le passa il link (o niente, solo
/// per farsi trovare) e ritorna true; il chiamante allora esce.
pub fn hand_off(link: Option<&str>) -> bool {
    use std::io::Write;
    let Ok(mut s) = std::os::unix::net::UnixStream::connect(sock_path()) else {
        return false;
    };
    let _ = writeln!(s, "{}", link.unwrap_or(""));
    true
}

/// Ascolta le istanze successive: ogni riga non vuota è un link da aprire.
fn listen(tx: mpsc::UnboundedSender<NetEvent>) {
    use std::io::BufRead;
    let path = sock_path();
    // un socket rimasto da un'uscita brusca: nessuno risponde (hand_off è
    // già fallito), si può togliere
    let _ = std::fs::remove_file(&path);
    let Ok(listener) = std::os::unix::net::UnixListener::bind(&path) else {
        return;
    };
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let mut line = String::new();
            let _ = std::io::BufReader::new(conn).read_line(&mut line);
            let line = line.trim();
            if !line.is_empty() && tx.send(NetEvent::Link(line.to_string())).is_err() {
                break;
            }
        }
    });
}

/// Avvia il runtime e la connessione; gli eventi aspettano la subscription.
pub fn start(link: Option<String>) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime tokio");
    let (tx, rx) = mpsc::unbounded_channel();
    *EVENTS.lock().unwrap() = Some(rx);
    let _ = SENDER.set(tx.clone());
    listen(tx.clone());
    if let Some(l) = link {
        let _ = tx.send(NetEvent::Link(l));
    }
    rt.spawn(async move {
        if let Err(e) = run(tx.clone()).await {
            let _ = tx.send(NetEvent::Failed(e));
        }
    });
    let _ = RT.set(rt);
}

/// Lo stream della subscription: inoltra gli eventi del canale come messaggi.
pub fn events() -> impl Stream<Item = Carrier> {
    let rx = EVENTS.lock().ok().and_then(|mut g| g.take());
    iced::futures::stream::unfold(rx, |rx| async move {
        let mut rx = rx?;
        let ev = rx.recv().await?;
        Some((Carrier(Arc::new(Mutex::new(Some(ev)))), Some(rx)))
    })
    .boxed()
}

async fn run(tx: mpsc::UnboundedSender<NetEvent>) -> Result<(), String> {
    let status = |s: &str| {
        let _ = tx.send(NetEvent::Status(s.into()));
    };
    // la GUI non ha un terminale per numero e codice: l'accesso si fa una
    // volta con `rtete`, che salva credenziali e sessione
    let creds = tg::load_credentials()
        .ok_or("nessuna credenziale: avvia prima `rtete` nel terminale per l'accesso")?;
    loader::prune_cache();

    status("mi collego a Telegram…");
    let mut wait = 2u64;
    let (_session, client, updates) = loop {
        match tg::connect(&creds).await.map_err(|e| e.to_string()) {
            Ok((session, client, updates)) => match client.is_authorized().await {
                Ok(true) => break (session, client, updates),
                Ok(false) => {
                    return Err(
                        "sessione non autorizzata: accedi una volta con `rtete` nel terminale"
                            .into(),
                    )
                }
                Err(e) if wait <= 16 => status(&format!("rete non pronta ({e}), ritento…")),
                Err(e) => return Err(e.to_string()),
            },
            Err(e) if wait <= 16 => status(&format!("rete non pronta ({e}), ritento…")),
            Err(e) => return Err(e.to_string()),
        }
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
        wait *= 2;
    };

    status("carico le conversazioni…");
    let mut contacts = tg::load_dialogs(&client).await.map_err(|e| e.to_string())?;
    app::pinned_first(&mut contacts);
    let me = tg::own_name(&client).await;

    let (req_tx, req_rx) = mpsc::unbounded_channel::<Request>();
    let (res_tx, mut res_rx) = mpsc::unbounded_channel::<Response>();
    worker::run_worker(client.clone(), req_rx, res_tx.clone());

    let cfg = grammers_client::sender::UpdatesConfiguration { catch_up: false };
    let mut stream = client
        .stream_updates(updates, cfg)
        .await
        .map_err(|e| e.to_string())?;

    let _ = tx.send(NetEvent::Ready {
        link: Link {
            client: client.clone(),
            req: req_tx,
            res: res_tx,
        },
        contacts,
        me,
    });

    loop {
        let ev = tokio::select! {
            upd = stream.next() => match upd {
                Ok(u) => NetEvent::Update(u),
                Err(e) => NetEvent::UpdateErr(e.to_string()),
            },
            done = res_rx.recv() => match done {
                Some(r) => NetEvent::Response(r),
                None => return Ok(()),
            },
        };
        if tx.send(ev).is_err() {
            return Ok(()); // finestra chiusa
        }
    }
}
