//! Su Windows con `--features whatsapp` lo storage di WhatsApp
//! (libsqlite3-sys, senza la sua copia di SQLite) chiede al linker
//! `sqlite3.lib`, che lì non esiste. I simboli sqlite3 li porta già libsql
//! (grammers): basta una libreria vuota con quel nome perché il link riesca.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let windows = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let whatsapp = env::var_os("CARGO_FEATURE_WHATSAPP").is_some();
    if !(windows && whatsapp) {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let stub = out.join("sqlite3_stub.c");
    fs::write(&stub, "/* vuoto: i simboli sqlite3 vengono da libsql */\n")
        .expect("scrittura di sqlite3_stub.c");
    // niente metadata di cc: serve solo che sqlite3.lib esista nel percorso
    cc::Build::new().file(&stub).cargo_metadata(false).compile("sqlite3");
    println!("cargo:rustc-link-search=native={}", out.display());
}
