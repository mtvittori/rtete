//! Ponte fra la rete e iced: un runtime tokio tutto nostro tiene in piedi
//! il backend e i worker; quello che arriva (eventi live, risposte dei
//! worker) passa in un solo canale che la subscription di iced legge.

use std::sync::{Arc, Mutex, OnceLock};

use iced::futures::{Stream, StreamExt};
use tokio::sync::mpsc;

use rondine::send::{Request, Response};
use rondine::model::{ChatId, DialogInfo};
use rondine::{app, loader, tg, tr, trf};

/// Il runtime della rete: backend, worker e `mark_read` girano qui, non
/// nell'executor di iced (così `Handle::enter` basta per gli spawn).
pub static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
/// Telegram o WhatsApp: ogni evento dice da quale ambiente arriva.
pub use crate::settings::UiStyle as Env;

type Tx = mpsc::UnboundedSender<(Env, NetEvent)>;

/// il ricevitore degli eventi di rete: la subscription lo prende una volta
static EVENTS: Mutex<Option<mpsc::UnboundedReceiver<(Env, NetEvent)>>> = Mutex::new(None);
/// per chi manda eventi da fuori dal ciclo di rete (click sulle notifiche)
static SENDER: OnceLock<Tx> = OnceLock::new();

pub fn open_peer(env: Env, peer: ChatId) {
    if let Some(tx) = SENDER.get() {
        let _ = tx.send((env, NetEvent::OpenPeer(peer)));
    }
}

pub struct Link {
    pub req: mpsc::UnboundedSender<Request>,
    pub res: mpsc::UnboundedSender<Response>,
}

// come in send.rs: i payload viaggiano interi, una volta per evento
#[allow(clippy::large_enum_variant)]
pub enum NetEvent {
    /// passo dell'avvio (0 connessione, 1 account, 2 chat) e cosa succede
    Status(u8, String),
    Ready {
        link: Link,
        contacts: Vec<DialogInfo>,
        me: String,
    },
    Failed(String),
    Response(Response),
    /// link Telegram da aprire (argomento o da un'altra istanza)
    Link(String),
    /// click su una notifica: apri la chat
    OpenPeer(ChatId),
    /// WhatsApp da collegare: QR, codice, abbinato
    Pair(Pairing),
    /// Telegram chiede un dato per l'accesso
    Login(Login),
}

/// L'accesso a Telegram nella finestra: cosa chiedere (con l'errore del
/// tentativo prima, se c'è).
#[derive(Clone, Debug, PartialEq)]
pub enum Login {
    Credentials(Option<String>),
    Phone(Option<String>),
    Code(Option<String>),
    Password { hint: String, error: Option<String> },
}

/// le risposte dell'accesso (credenziali, numero, codice, password)
static LOGIN: Mutex<Option<mpsc::UnboundedSender<String>>> = Mutex::new(None);

pub fn answer(text: String) {
    if let Some(tx) = LOGIN.lock().ok().and_then(|g| g.clone()) {
        let _ = tx.send(text);
    }
}

/// Cosa mostrare mentre si collega WhatsApp (copia di quello del backend,
/// così la GUI compila anche senza la feature `whatsapp`).
#[derive(Clone, Debug, PartialEq)]
pub enum Pairing {
    Qr(String),
    Code(String),
    Paired,
    Status(String),
}

/// I messaggi di iced devono essere `Clone`: l'evento viaggia in una
/// scatola condivisa e chi lo riceve lo estrae una volta sola.
#[derive(Clone)]
pub struct Carrier(Arc<Mutex<Option<(Env, NetEvent)>>>);

impl Carrier {
    pub fn take(&self) -> Option<(Env, NetEvent)> {
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
    dir.join("rondine-gui.sock")
}

/// Istanza unica: se Rondine è già aperta le passa il link (o niente, solo
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
fn listen(tx: Tx) {
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
            if !line.is_empty() && tx.send((Env::Telegram, NetEvent::Link(line.to_string()))).is_err() {
                break;
            }
        }
    });
}

/// Avvia il runtime e Telegram; gli eventi aspettano la subscription.
/// `cache_bytes`: tetto della cache degli asset, sfoltita prima di tutto.
pub fn start(link: Option<String>, cache_bytes: u64) {
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
        let _ = tx.send((Env::Telegram, NetEvent::Link(l)));
    }
    let _ = RT.set(rt);
    loader::prune_cache_to(cache_bytes);
    start_telegram();
}

/// (Ri)avvia la connessione a Telegram.
pub fn start_telegram() {
    let (Some(rt), Some(tx)) = (RT.get(), SENDER.get().cloned()) else { return };
    rt.spawn(async move {
        if let Err(e) = run(tx.clone()).await {
            let _ = tx.send((Env::Telegram, NetEvent::Failed(e)));
        }
    });
}

/// Collega (o ricollega) WhatsApp: la prima volta arriva il QR, o col
/// numero `phone` il codice di 8 caratteri.
#[cfg(feature = "whatsapp")]
pub fn start_whatsapp(phone: Option<String>) {
    let (Some(rt), Some(tx)) = (RT.get(), SENDER.get().cloned()) else { return };
    rt.spawn(async move {
        if let Err(e) = run_wa(tx.clone(), phone).await {
            let _ = tx.send((Env::WhatsApp, NetEvent::Failed(e)));
        }
    });
}

#[cfg(feature = "whatsapp")]
async fn run_wa(tx: Tx, phone: Option<String>) -> Result<(), String> {
    use rondine::backend::wa;
    let send = |ev: NetEvent| {
        let _ = tx.send((Env::WhatsApp, ev));
    };
    send(NetEvent::Status(0, tr("mi collego a WhatsApp…").into()));
    let ptx = tx.clone();
    let sink: wa::PairSink = Arc::new(move |p| {
        let p = match p {
            wa::Pairing::Qr(c) => Pairing::Qr(c),
            wa::Pairing::Code(c) => Pairing::Code(c),
            wa::Pairing::Paired => Pairing::Paired,
            wa::Pairing::Status(s) => Pairing::Status(s),
        };
        let _ = ptx.send((Env::WhatsApp, NetEvent::Pair(p)));
    });
    let (req_tx, req_rx) = mpsc::unbounded_channel::<Request>();
    let (res_tx, mut res_rx) = mpsc::unbounded_channel::<Response>();
    let (mut contacts, me) = wa::start_with(req_rx, res_tx.clone(), sink, phone).await.map_err(|e| e.to_string())?;
    app::pinned_first(&mut contacts);
    send(NetEvent::Ready { link: Link { req: req_tx, res: res_tx }, contacts, me });
    while let Some(r) = res_rx.recv().await {
        if tx.send((Env::WhatsApp, NetEvent::Response(r))).is_err() {
            return Ok(()); // finestra chiusa
        }
    }
    Ok(())
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

async fn run(tx: Tx) -> Result<(), String> {
    let status = |step: u8, s: &str| {
        let _ = tx.send((Env::Telegram, NetEvent::Status(step, s.into())));
    };
    // l'accesso nella finestra: domande alla GUI, risposte da `answer`
    let (ans_tx, mut answers) = mpsc::unbounded_channel::<String>();
    *LOGIN.lock().unwrap() = Some(ans_tx);
    let ask = |a: Login| {
        let _ = tx.send((Env::Telegram, NetEvent::Login(a)));
    };
    let creds = match tg::load_credentials() {
        Some(c) => c,
        None => {
            let mut error = None;
            loop {
                ask(Login::Credentials(error.take()));
                let raw = answers.recv().await.ok_or(tr("accesso interrotto"))?;
                let mut it = raw.split_whitespace();
                let id = it.next().and_then(|s| s.parse::<i32>().ok());
                match (id, it.next()) {
                    (Some(api_id), Some(hash)) if hash.len() >= 16 => {
                        let c = tg::Credentials { api_id, api_hash: hash.to_string() };
                        tg::save_credentials(&c).map_err(|e| e.to_string())?;
                        break c;
                    }
                    _ => error = Some(tr("API ID (un numero) e API hash (32 caratteri) non sembrano giusti").to_string()),
                }
            }
        }
    };

    status(0, tr("mi collego a Telegram…"));
    let mut wait = 2u64;
    let (_session, client, updates) = loop {
        match tg::connect(&creds).await.map_err(|e| e.to_string()) {
            Ok((session, client, updates)) => {
                status(1, tr("verifico l'account…"));
                let authorized = client.is_authorized().await;
                match authorized {
                    Ok(true) => break (session, client, updates),
                    Ok(false) => {
                        let (qtx, mut qrx) = mpsc::unbounded_channel::<tg::LoginAsk>();
                        let fwd = tx.clone();
                        tokio::spawn(async move {
                            while let Some(q) = qrx.recv().await {
                                let l = match q {
                                    tg::LoginAsk::Phone(e) => Login::Phone(e),
                                    tg::LoginAsk::Code(e) => Login::Code(e),
                                    tg::LoginAsk::Password { hint, error } => Login::Password { hint, error },
                                };
                                let _ = fwd.send((Env::Telegram, NetEvent::Login(l)));
                            }
                        });
                        tg::sign_in_steps(&client, &creds.api_hash, &qtx, &mut answers).await?;
                        break (session, client, updates);
                    }
                    Err(e) if wait <= 16 => status(0, &trf!("rete non pronta ({}), ritento…", e)),
                    Err(e) => return Err(e.to_string()),
                }
            }
            Err(e) if wait <= 16 => status(0, &trf!("rete non pronta ({}), ritento…", e)),
            Err(e) => return Err(e.to_string()),
        }
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
        wait *= 2;
    };

    status(2, tr("carico le conversazioni…"));
    let mut contacts = tg::load_dialogs(&client).await.map_err(|e| e.to_string())?;
    app::pinned_first(&mut contacts);
    let me = tg::own_name(&client).await;

    let (req_tx, req_rx) = mpsc::unbounded_channel::<Request>();
    let (res_tx, mut res_rx) = mpsc::unbounded_channel::<Response>();
    // il backend: dispatcher e update live, tutto sul canale delle risposte
    tg::worker::start(client, updates, req_rx, res_tx.clone()).await?;

    let _ = tx.send((Env::Telegram, NetEvent::Ready {
        link: Link {
            req: req_tx,
            res: res_tx,
        },
        contacts,
        me,
    }));

    while let Some(r) = res_rx.recv().await {
        if tx.send((Env::Telegram, NetEvent::Response(r))).is_err() {
            return Ok(()); // finestra chiusa
        }
    }
    Ok(())
}
