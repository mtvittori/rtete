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
| `↑`/`↓` (o `j`/`k` senza chat aperta) | giù / su |
| `→`/`←` (o `l`/`h` nel pannello) | entra/esce nel set, sinistra/destra nella griglia |
| `Invio` | apre la chat selezionata (la prima volta carica la cronologia) |
| `❯ …⏎` | invia il testo composto alla chat aperta |
| `Tab` | apre/chiude il pannello sticker |
| `Invio` (nel pannello) | apre il set / invia lo sticker sulla chat aperta |
| `Esc` | indietro / chiudi pannello / svuota composizione |
| `q` | esci (fuori dal pannello e senza testo in composizione) |
| `Ctrl-C` | esci subito |

> Nota: quando una chat è aperta tutte le lettere (inclusi `j`, `k`, `h`,
> `l`, `s`) finiscono nel testo del messaggio; i tasti vim navigano solo con
> la composizione vuota, e il pannello sticker si apre/chiude con `Tab`.

## Tema

Interfaccia ispirata a **jellyfin-tui**: carta crema, cornici arrotondate col
titolo incastonato nella riga superiore, conteggio sull'estremo destro della
cornice, barra di selezione scura con `▶`, cornice ambra sul pannello attivo,
suggerimenti dei tasti (`nome <tasto>`) nella cornice inferiore e rail di
scorrimento con frecce sul bordo destro. La composizione del messaggio vive
nella cornice in basso (`❯ testo`), gli avvisi transitori appaiono in alto a
destra come il volume di jellyfin-tui.

## Comandi extra

| Tasto | Azione |
| --- | --- |
| `Ctrl-E` | modifica l'ultimo tuo messaggio di testo (il testo va in composizione, `Invio` conferma) |
| `Ctrl-D` | arma la cancellazione dell'ultimo tuo messaggio, `Invio` conferma (Esc annulla) |
| `Ctrl-O` | scarica l'ultimo documento della chat in `~/Downloads/` |
| `Ctrl-F` | ricerca server-side nella chat aperta (frase esatta, fino a 40 risultati) |

## E media

- **Foto Telegram, allegati immagini e GIF**: renderizzati in terminale con il kitty graphics protocol; le GIF e gli sticker APNG **animano da sole (kitty anima gli APNG nati come GIF: l'animazione gira sul terminale).
- **Documenti**: riga con nome e dimensione; `Ctrl-O` li salva in `~/Downloads`
- **Video/vocali**: etichetta informativa (durata/caption), non renderizzati.
- **Grandezza immagini**: variabile d'ambiente `RTETE_SCALE` (0.5–4) moltiplica sticker/foto/nella griglia e messaggi. Il rapporto celle si regola con `RTETE_CELL_W`/`RTETE_CELL_H` (pixel per cella del terminale).

## Limitazioni note


- Gli sticker **TGS** (Lottie) e i **video-sticker webm** restano etichetta emoji: renderizzarli servirebbero librerie C pesanti (rlottie/libvpx), in contrasto con la filosofia "poco RAM, build semplice".
- I **video** (mp4) sono solo etichetta informativa (servirebbe un decodificatore video).
- Nessun supporto a survey di reazioni o reazioni mobili (stanti, per ora).
- Il terminale non compatibile con il *kitty graphics protocol* mostra solo le etichette; ghostty/wezterm possono mostrare APNG/GIF statici.

## RAM

Nessuna cache in memoria: ogni media scaricato viene subito convertito/compresso e buttato
su disco (`~/.local/share/rtete/assets/<md5>.png`, cache condivisa per tutti i tipi);
la cronologia in RAM è limitata a 60 messaggi per chat aperta, le conversioni girano
in spawn_blocking e NESSUN frame completo resta a lungo nello heap. `smem`/`ps` per
verificare: il RSS tipico sta nelle decine di MB, non centinaia.
