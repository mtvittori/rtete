//! Rondine — client Telegram TUI per Linux, pensato per consumare pochissima RAM:
//! grammers (MTProto puro-Rust, niente TDLib), ratatui per l'interfaccia e il
//! kitty graphics protocol (o sixel su foot) per mostrare gli sticker.

mod events;
mod gfx;
mod kitty;
mod sixel;
mod styles;
mod term;
mod view;

use rondine::{app, clipboard, loader, send, tg, worker};

use std::sync::atomic::Ordering;

use crossterm::event::EventStream;
use crossterm::execute;
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::app::App;
use crate::view::Cmd;
use crate::events::handle_event;
use rondine::sync::{handle_response, handle_update};
use crate::send::{Request, Response};
use crate::term::{panic_hook, TUI_UP, TermGuard};

async fn render(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> Result<Vec<Cmd>, Box<dyn std::error::Error>> {
    let mut cmds = Vec::new();
    let done = terminal.draw(|f| view::draw(f, app, &mut cmds))?;
    // sixel: serve il buffer disegnato per sapere quali celle sono state riscritte
    gfx::observe_frame(done.buffer);
    Ok(cmds)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tg::migrate_old_dirs();
    let creds = match tg::load_credentials() {
        Some(c) => c,
        None => tg::prompt_credentials().await?,
    };
    // rileva kitty / sixel e la misura delle celle prima che la TUI prenda stdin
    gfx::startup_detect();
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
    // all'avvio alcuni terminali non hanno ancora i pixel: si rilegge a TUI su
    gfx::refresh_cells();
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
        gfx::sync_begin();
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
        app.request_quoted(&req_tx);

        // immagini. Kitty: a=p a ogni frame; il dato entra UNA volta sola per
        // path (t=f: il terminale legge il file da solo) e a fine frame le
        // scomparse vengono rimosse per id — prima un `a=d,d=A` a ogni cambio
        // di layout le svuotava tutte e la view le ritrasmetteva da zero
        // (base64 sul pty, flicker). Sixel: non ha id, i pixel restano finché
        // qualcuno riscrive le celle; se un'immagine è sparita o si è mossa
        // `frame_end` riscrive le sue celle, e le immagini si emettono in
        // `frame_flush`, dopo l'ultimo draw (ratatui sopra le cancellerebbe)
        gfx::frame_begin();
        for cmd in cmds.drain(..) {
            let Cmd::Place { rect, path } = cmd;
            let size = gfx::square_fit(rect.width as u32, rect.height as u32);
            let _ = gfx::draw_png(&path, (rect.x, rect.y), size);
        }
        gfx::frame_end(&mut terminal);
        gfx::frame_flush();
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
