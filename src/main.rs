//! RTete — client Telegram TUI per Linux, pensato per consumare pochissima RAM:
//! grammers (MTProto puro-Rust, niente TDLib), ratatui per l'interfaccia e il
//! kitty graphics protocol per mostrare gli sticker.

mod app;
mod clipboard;
mod events;
mod kitty;
mod loader;
mod send;
mod styles;
mod term;
mod tg;
mod view;
mod worker;

use std::sync::atomic::Ordering;

use crossterm::event::EventStream;
use crossterm::execute;
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::app::{App, Cmd};
use crate::events::{handle_event, handle_response, handle_update};
use crate::send::{Request, Response};
use crate::term::{panic_hook, TUI_UP, TermGuard};

async fn render(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> Result<Vec<Cmd>, Box<dyn std::error::Error>> {
    let mut cmds = Vec::new();
    terminal.draw(|f| view::draw(f, app, &mut cmds))?;
    Ok(cmds)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let creds = match tg::load_credentials() {
        Some(c) => c,
        None => tg::prompt_credentials().await?,
    };
    // rileva il supporto kitty graphics prima che la TUI prenda stdin
    kitty::startup_detect();
    loader::prune_cache();

    // connect + sign_in: se la rete non è pronta ritenta qualche colpo
    let (_session, client, updates) = tg::connect_and_sign(&creds).await?;

    println!("Carico le conversazioni…");
    let mut contacts = tg::load_dialogs(&client).await?;
    app::pinned_first(&mut contacts);
    let mut app = App {
        contacts,
        me: tg::own_name(&client).await,
        ..App::default()
    };

    // ---------------- canali di servizio -------------------------------
    // Un solo canale di richieste e uno di risposte: prima erano dieci
    // coppie mpsc e un waiting_pick che girava a mano col primo canale che
    // rispondeva, con `expect("servizio chiuso")` su ogni receive.
    let (req_tx, req_rx) = mpsc::unbounded_channel::<Request>();
    let (res_tx, mut res_rx) = mpsc::unbounded_channel::<Response>();

    worker::run_worker(client.clone(), req_rx, res_tx.clone());

    // stream aggiornamenti live
    let cfg = grammers_client::sender::UpdatesConfiguration { catch_up: false };
    let mut update_stream = client
        .stream_updates(updates, cfg)
        .await
        .map_err(|e| e.to_string())?;

    // ---------------- TUI ----------------------------------------------
    panic_hook();
    crossterm::terminal::enable_raw_mode()?;
    // da qui ogni uscita ripristina il terminale: guard per i `?` e per il
    // return, hook per i panic (con panic = "abort" il Drop non viene eseguito)
    let _guard = TermGuard;
    TUI_UP.store(true, Ordering::SeqCst);
    execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    terminal.hide_cursor()?;
    // trackpad/rotella: invii di scorrimento verso il cursore selezione
    execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)?;
    // il terminale avvolge gli incolla: niente Invio involontari multipli
    execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste)?;
    // kitty keyboard protocol: cmd+lettera arriva come SUPER pulito
    execute!(
        std::io::stdout(),
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        )
    )?;

    let mut events = EventStream::new();
    let mut frame_sig: Vec<(u16, u16, u16, u16, String)> = Vec::new();

    while !app.quit {
        let mut cmds = render(&mut terminal, &mut app).await?;
        let sig: Vec<(u16, u16, u16, u16, String)> = cmds
            .iter()
            .map(|c| {
                let Cmd::Place { rect, path } = c;
                (
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                    path.display().to_string(),
                )
            })
            .collect();

        // prenota le anteprime solo quando cambia l'asset visibile
        if sig != frame_sig {
            app.refresh_preview_queue(&req_tx);
        }
        app.request_older(&req_tx);

        // immagini: a=p a ogni frame; il dato dell'immagine entra UNA volta
        // sola per path (t=f: il terminale legge il file da solo) e a fine
        // frame le scomparse vengono rimosse per id — prima un `a=d,d=A` a
        // ogni cambio di layout le svuotava tutte e la view le ritrasmetteva
        // da zero (base64 sul pty, flicker)
        kitty::frame_begin();
        for cmd in cmds.drain(..) {
            let Cmd::Place { rect, path } = cmd;
            let size = kitty::square_fit(rect.width as u32, rect.height as u32);
            let _ = kitty::draw_png(&path, (rect.x, rect.y), size);
        }
        kitty::frame_end();
        frame_sig = sig;

        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(e)) => {
                    handle_event(&mut app, e, &req_tx, &res_tx).await
                }
                Some(Err(_)) => {}
                None => break,
            },
            upd = update_stream.next() => match upd {
                Ok(u) => handle_update(&mut app, &client, u).await,
                Err(e) => app.status = format!("rete: {e}"),
            },
            done = res_rx.recv() => match done {
                Some(resp) => handle_response(&mut app, resp, &client, &req_tx).await,
                None => break,
            },
        }
    }

    Ok(())
    // il Drop di _guard fa il restore: qualunque via d'uscita esce pulito
}
