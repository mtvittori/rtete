# RTete

Client Telegram *nativo per Linux* pensato per consumare pochissima RAM:

- **MTProto puro Rust** con [`grammers`](https://docs.rs/grammers-client) — nessun TDLib, nessun interprete JS, nessun browser embedded.
- **TUI** con [`ratatui`](https://docs.rs/ratatui) + crossterm: gira nel terminale, leggero anche senza GPU.
- **Sticker visualizzati davvero** tramite il *kitty graphics protocol* (Kitty, Ghostty, WezTerm e altri terminali compatibili); anteprime WebP statiche convertite in PNG su disco, con fallback testuale (solo etichetta emoji) su terminali non compatibili.
- Nessuna cronologia persistente oltre alla **sessione sqlite** (chiavi di autorizzazione): la RAM resta piatta.

## Dipendenze di sistema

Solo un toolchain Rust stabile; nessun'altra libreria di sistema. Su Arch:

```
sudo pacman -S rust
```

## Build

```
cargo build --release
```

Il binario finisce in `target/release/rtete`.

## Primo avvio

```
target/release/rtete
```

Al primo avvio chiede (una sola volta, salvate in `~/.config/rtete/credentials`):

1. **API ID** e **API hash** — da <https://my.telegram.org> → *API development tools*;
2. **numero di telefono** (formato internazionale, `+39…`);
3. **codice** inviato da Telegram (dentro l'app ufficiale);
4. eventuale **password 2FA**.

Dopodiché la sessione vive in `~/.local/share/rtete/rtete.db`.

## Comandi

| Tasto | Azione |
| --- | --- |
| `j` / `k` | giù / su |
| `Invio` | apre la chat selezionata (la prima volta carica la cronologia) |
| `❯ …⏎` | invia il testo composto alla chat aperta |
| `s` | apre il pannello sticker |
| `l` | entra nel set / naviga a destra nella griglia |
| `h` | torna indietro / naviga a sinistra |
| `Invio` (nel set) | invia lo sticker selezionato alla chat aperta |
| `Esc` | indietro / chiudi pannello / svuota composizione |
| `q` | esci (fuori dal pannello e senza testo in composizione) |
| `Ctrl-C` | esci subito |

## Limitazioni note

- Solo sticker **statici** (WebP): i TGS animati vengono mostrati come etichetta emoji.
- GIF/video/documenti/elenco di reazioni non gestiti (mostrati come segnaposto).
- Nessun supporto a segnalazione, modifica/cancellazione messaggi o ricerca.
- Il terminale non compatibile con il *kitty graphics protocol* mostra solo l'etichetta emoji.

## RAM

Nessuna cache in memoria: ogni sticker scaricato viene subito convertito e buttato
su disco (`~/.local/share/rtete/stickers/<md5>.png`); la cronologia in RAM è
limitata a 60 messaggi per chat aperta. `smem`/`ps` per verificare: il RSS tipico
sta nelle decine di MB, non centinaia.
