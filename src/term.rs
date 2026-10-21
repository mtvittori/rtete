//! Ciclo di vita del terminale: restore, guard e panic hook per uscite sempre pulite.

use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::execute;

use crate::gfx;

/// La TUI sta a schermo; consumata una volta sola dal panic hook o dal
/// Drop della guard, così non si ripristina due volte la stessa cosa.
pub(crate) static TUI_UP: AtomicBool = AtomicBool::new(false);


/// Ripristina raw mode, schermo alternativo, mouse e keyboard protocol.
/// Ogni istruzione è eseguita con `let _`: se una fallisce, le altre
/// partono comunque.
pub(crate) fn restore_terminal() {
    gfx::clear_all();
    let mut out = std::io::stdout();
    let _ = execute!(out, crossterm::cursor::Show);
    let _ = execute!(out, crossterm::event::PopKeyboardEnhancementFlags);
    let _ = execute!(out, crossterm::event::DisableMouseCapture);
    let _ = execute!(out, crossterm::event::DisableBracketedPaste);
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = execute!(out, crossterm::terminal::LeaveAlternateScreen);
}

/// Uscita pulita dal terminale per qualunque ritorno: con `Ok`, con `?`
/// o con panic. In release `panic = "abort"` lascia il terminale in
/// raw mode (il Drop della guard non viene eseguito), quindi serve
/// anche un hook — che invece gira sempre.
pub(crate) struct TermGuard;

impl Drop for TermGuard {
    fn drop(&mut self) {
        if TUI_UP.swap(false, Ordering::SeqCst) {
            restore_terminal();
        }
    }
}

pub(crate) fn panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if TUI_UP.swap(false, Ordering::SeqCst) {
            restore_terminal();
        }
        prev(info);
    }));
}
