# Rondine

*Rondine* è la rondine, l'uccello messaggero (prima l'app si chiamava RTete:
al primo avvio cartelle, sessione e impostazioni si spostano da sole, e le
vecchie variabili `RTETE_*` valgono ancora).

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

Il binario finisce in `target/release/rondine`.

## Versione grafica (iced)

Accanto alla TUI c'è `rondine-gui`, la stessa Rondine (rete, worker, cache
degli sticker, stato) con un'interfaccia [iced](https://iced.rs): lista
chat a sinistra, cronologia a bolle, sticker e foto come immagini vere.

```
cargo build --release --features gui --bin rondine-gui
target/release/rondine-gui
```

L'accesso (numero, codice, 2FA) si fa una volta con `rondine` nel
terminale: la GUI riusa credenziali e sessione salvate. Click su un
messaggio per le azioni (reazioni, rispondi, modifica, copia, elimina),
📎 incolla un'immagine dagli appunti, 😊 apre gli sticker, Esc annulla.
Click sul nome in alto (o ⓘ) apre la scheda della chat: bio, username,
membri, media/file/link/musica/vocali condivisi, blocco dell'utente ed
eliminazione della chat (o uscita dal gruppo), sempre con un popup di
conferma. I non letti si aggiornano anche se leggi da un altro dispositivo.
⚙ apre le impostazioni divise come in Telegram: account (nome, username,
bio, foto profilo), aspetto, notifiche, immagini e media (download
automatico, miniature, misura delle foto), dati e archiviazione (spazio
usato, svuota cache, tetto della cache), dispositivi (sessioni attive da
chiudere) e link.

## Telegram e WhatsApp nella GUI

`cargo build --release --features gui,whatsapp`: a sinistra una barra con
i due **ambienti**, ognuno con le sue chat, il suo stile (Telegram o
WhatsApp) e il suo tema. L'accesso si fa nella finestra: per Telegram
API ID/hash (una volta), numero, codice e 2FA; per WhatsApp «Mostra il
QR» e lo si inquadra dal telefono (Dispositivi collegati). WhatsApp è un
collegamento non ufficiale: meglio un numero secondario per provare.

La barra si nasconde da *Impostazioni → Aspetto* (restano Ctrl+1 per
Telegram e Ctrl+2 per WhatsApp). Per averli come due app indipendenti:

```sh
rondine-gui --telegram   # o -t: finestra rondine-telegram
rondine-gui --whatsapp   # o -w: finestra rondine-whatsapp
```

Ognuna ha la sua finestra (classe per Hyprland), la sua istanza unica e
il suo tema; i link `tg://` vanno a quella con Telegram. Se è già aperta
la finestra con tutti e due, si porta a fuoco quella (userebbero le
stesse sessioni).

## Stile WhatsApp

In Impostazioni → Aspetto si sceglie lo **stile**: Telegram (com'era) o
WhatsApp (titolo «Chat», bolle con la punta, campo tondo e invio verde).
Ogni stile ricorda il **suo tema** (Omarchy, Rondine scuro o Verde
WhatsApp): cambiarlo in uno non tocca l'altro. In entrambi gli stili:

- filtri sopra la lista: Tutte, Non lette, Preferite, Gruppi, più la voce
  **Archiviate** (l'archivio di Telegram, sincronizzato);
- **tasto destro su una chat**: archivia, fissa in alto, silenzia, segna
  come letta/da leggere, aggiungi alle preferite, elimina;
- **messaggi effimeri** dalla scheda della chat: spenti, 24 ore, 7 giorni,
  90 giorni (il timer di auto-eliminazione di Telegram);
- **messaggi importanti ⭐**: «⭐ Importante» nelle azioni del messaggio, la
  lista completa dal pulsante ⭐ in alto. Telegram non ha la stella, quindi
  restano sul PC (`~/.local/share/rondine/starred.json`), con una copia del testo.

## Traduzione dei messaggi

Con una chiave **DeepL** (predefinito; il piano «API Free» è gratuito) o di
un servizio compatibile OpenAI (**Groq**, OpenRouter, Ollama in locale) i
messaggi si traducono: «🌐 Traduci» nelle azioni del messaggio nella GUI,
`Ctrl-G` nella TUI. Dalla scheda di una chat si attiva la traduzione
automatica dei messaggi in arrivo. Si configura in Impostazioni →
Traduzione (servizio, chiave, lingua di destinazione, pulsante «Prova»);
la configurazione sta in `~/.config/rondine/translate.conf` (0600). I testi
vanno al servizio scelto solo quando si traduce.

## Lingue

L'interfaccia (GUI e TUI) è in italiano e in inglese: segue la lingua del
sistema (`LANG`), e nella GUI si sceglie anche da Impostazioni → Aspetto.
I testi italiani nel codice fanno da chiave; le traduzioni stanno in
`locales/<lingua>.txt`, a blocchi di due righe (italiano, traduzione). Per
aggiungere una lingua basta un file nuovo e una voce in `src/i18n.rs`;
`cargo test` segnala i testi senza traduzione.
Disegna in software (tiny-skia, ~75 MB di RAM); `ICED_BACKEND=wgpu` usa
la GPU (~105 MB).

## WhatsApp (sperimentale)

Rondine può usare anche WhatsApp, con
[`whatsapp-rust`](https://github.com/jlucaso1/whatsapp-rust): lo stesso
protocollo multi-device di WhatsApp Web, in Rust puro. **Non è un client
ufficiale**: Meta può sospendere l'account, meglio provarlo su un numero
secondario.

```
cargo build --release --features whatsapp
target/release/rondine --whatsapp
```

Al primo avvio compare un QR nel terminale: sul telefono *WhatsApp →
Dispositivi collegati → Collega un dispositivo*. In alternativa
`RONDINE_WA_PHONE=39333… rondine --whatsapp` mostra un codice di 8
caratteri da inserire nel telefono (*Collega con numero*). Poi la sessione
resta in `~/.local/share/rondine/whatsapp/`.

WhatsApp non tiene la cronologia sul server: le chat arrivano dal telefono
all'abbinamento e da lì in poi Rondine le salva in locale
(`whatsapp/chats.json` e `whatsapp/msgs/*.jsonl`, leggibili solo da te).

Cosa c'è già: lista chat, cronologia, messaggi in tempo reale, invio di
testo, risposte, modifica, cancellazione dei propri messaggi, reazioni,
spunte di lettura, «sta scrivendo», foto/sticker/miniature dei video,
salvataggio e riproduzione dei media, invio di foto e file, ricerca nella
chat. Non ci sono (o non ancora): set di sticker, foto profilo, inoltro,
blocco, eliminazione delle chat, nomi dei gruppi nuovi prima della
sincronizzazione, la GUI (per ora solo la TUI).

## Architettura

L'interfaccia (TUI e GUI) e lo stato non conoscono il servizio di
messaggistica: parlano con un **backend** solo coi tipi neutri di
`src/model.rs` (chat, messaggi, media) e col protocollo `Request` /
`Response` di `src/send.rs`. Anche gli eventi live (messaggi nuovi,
letture, «sta scrivendo») arrivano come `Response::Live` sullo stesso
canale.

| Dove | Cosa |
| --- | --- |
| `src/model.rs`, `src/paths.rs`, `src/links.rs` | tipi, cartelle e link, uguali per ogni backend |
| `src/app.rs`, `src/sync.rs` | stato e come cambia a ogni evento/risposta |
| `src/worker.rs`, `src/loader.rs` | salvataggi, player, traduzioni, conversione delle anteprime |
| `src/backend/tg/` | Telegram (grammers): accesso, dispatcher delle richieste, update live |
| `src/backend/wa/` | WhatsApp (whatsapp-rust): abbinamento, archivio locale, dispatcher, eventi |

Ogni backend esegue le stesse `Request` e manda le stesse `Response`; un
servizio nuovo è un'altra cartella sotto `src/backend/`.

## Primo avvio

```
target/release/rondine
```

Al primo avvio chiede (una sola volta, salvate in `~/.config/rondine/credentials`):

1. **API ID** e **API hash** — da <https://my.telegram.org> → *API development tools*;
2. **numero di telefono** (formato internazionale, `+39…`);
3. **codice** inviato da Telegram (dentro l'app ufficiale);
4. eventuale **password 2FA**.

Dopodiché la sessione vive in `~/.local/share/rondine/rondine.db`.

## Comandi

| Tasto | Azione |
| --- | --- |
| `↑`/`↓` (o `j`/`k` senza chat aperta) | contatti; **dentro una chat**: seleziona i messaggi (nuovo: `↓` oltre l'ultimo esce dalla selezione) |
| `↑` sulla riga più vecchia | carica altri 60 messaggi precedenti (fino a 500 in memoria per chat); `Esc`/`↓` come prima. La riga di stato mostra ora, risposta (`↩#id`), inoltro (`⤳`) e «modificato» del messaggio selezionato |
| `ruota`/`trackpad` | scorre: sui contatti in sinistra, sui messaggi a destra (selezione) |
| `→`/`←` (o `l`/`h` nel pannello) | entra/esce nel set, sinistra/destra nella griglia |
| `Invio` | apre la chat selezionata (la prima volta carica la cronologia) |
| `❯ …⏎` | invia il testo composto alla chat aperta |
| `Tab` | apre/chiude il pannello sticker |
| `Invio` (nel pannello) | apre il set / invia lo sticker sulla chat aperta |
| `Esc` | a strati: chiude ricerca/pannello/modifica/cancellazione/selezione; poi **chiude la chat** e torna ai contatti |
| `q` | esci dalla lista contatti (prima `Esc` chiude la chat) |
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

Il cursore va sopra un messaggio con `↑/↓` (o la ruota nella zona messaggi):
il blocco viene evidenziato con la barra scura e porta in mezzo il messaggio
nella vista; `Esc` esce dalla selezione. `Ctrl-E/O/D/P` agiscono allora sul
messaggio selezionato invece che sull'ultimo.

| Tasto | Azione |
| --- | --- |
| `Ctrl-E` | modifica un tuo messaggio di testo (selezionato o, in mancanza, l'ultimo; il testo va in composizione, `Invio` conferma) |
| `Ctrl-D` | arma la cancellazione di un tuo messaggio (selezionato o l'ultimo), `Invio` conferma (Esc annulla) |
| `Ctrl-O` | salva un documento (`~/Downloads`): selezionato o l'ultimo della chat |
| `Ctrl-P` | riproduce audio selezionato/ultimo: `🎙 vocali` e brani tra i documenti; mpv/ffplay (`RONDINE_PLAYER` sovrascrive) |
| `Ctrl-Y` | copia negli appunti il messaggio selezionato (senza selezione: l'ultimo con testo) via `wl-copy` |
| `Ctrl-R` | risponde al messaggio selezionato: sopra la composizione compare `↱ Autore: …`, il prossimo invio (testo, sticker o foto) è la risposta; di nuovo `Ctrl-R` o `Esc` annulla |
| `Ctrl-T` | reazioni veloci sul messaggio selezionato: si apre la barra delle emoji (`←/→` o tasti `1..8`), `Invio` aggiunge/toglie la reazione, `Esc` chiude |
| `Ctrl+C` / `Cmd+C` | copia il messaggio selezionato; **senza selezione esce** (`q` per uscire sempre) |
| `Ctrl+V` / `Cmd+V` | incolla dagli appunti via `wl-paste`: se c'è un'**immagine** (png/jpeg) la prepara in tmp → Invio la invia come foto alla chat aperta (il testo del compose è la didascalia); testo altrimenti (multi-riga nel compose, spazi in ricerca) |
| `Ctrl-F` | ricerca server-side nella chat aperta: la query si scrive nel riquadro in fondo (multifila, `Invio` cerca) |
| `Ctrl-G` | traduce il messaggio selezionato (o l'ultimo ricevuto); di nuovo `Ctrl-G` torna all'originale |
| `click` su un messaggio | lo seleziona (come ↑/↓); sulla colonna contatti non fa nulla |
| `click` su un link | apre il browser di default (`xdg-open`); l'URL funziona anche se spezzato su più righe |

**Incollare**: il terminale avvolge il paste (`bracketed paste`): il testo con
accapo incollato nel compose resta un solo messaggio multi-riga (niente invii
a raffica); incollato nel riquadro di ricerca, gli accapo diventano spazi.
Con un'**immagine** negli appunti (`cmd+C` da browser/GIMP/screenshot),
`cmd+V` prepara l'invio come foto: il riquadro di composizione diventa ambra
con `🖼 foto dagli appunti`, `Invio` carica il file, lo carica su Telegram
(`upload_file` + foto, didascalia = testo composto) e lo manda; `Esc` scarta.

**Copiare col terminale**: con la rotella/trackpad catturata dallo scorrimento,
kitty permette comunque di selezionare testo **tenendo premuto Shift** +
`Ctrl-Shift-C` per copiarlo negli appunti.

## Media e audio

- **Foto Telegram, allegati immagini e GIF**: renderizzati in terminale con il kitty graphics protocol; le GIF e gli sticker APNG **animano da sole (kitty anima gli APNG nati come GIF: l'animazione gira sul terminale).
- **Documenti**: riga con nome e dimensione; `Ctrl-O` li salva in `~/Downloads`
- **Audio (vocali `🎙 m:ss` e brani tra i documenti `🎧`)**: `Ctrl-P` scarica e suona con un player esterno (mpv se presente, altrimenti ffplay; env `RONDINE_PLAYER` per un binario specifico). Nessuna libreria audio: 0 RAM extra, il player gira in un processo separato e un nuovo `Ctrl-P` lo termina.
- **Grandezza immagini**: variabile d'ambiente `RONDINE_SCALE` (0.5–4) moltiplica sticker/foto nella griglia e messaggi. Il rapporto celle si regola con `RONDINE_CELL_W`/`RONDINE_CELL_H` (pixel per cella del terminale).
- **Video (mp4)**: etichetta informativa (durata/caption), non renderizzati.
- Gli sticker TGS (Lottie) e i video-sticker webm restano etichetta emoji: renderizzarli servirebbero librerie C pesanti (rlottie/libvpx), in contrasto con la filosofia "poco RAM, build semplice".
- Nessun supporto a survey di reazioni o reazioni mobili (stanti, per ora).
- Il terminale non compatibile con il *kitty graphics protocol* mostra solo le etichette; ghostty/wezterm possono mostrare APNG/GIF statici.

## RAM

Nessuna cache in memoria: ogni media scaricato viene subito convertito/compresso e buttato
su disco (`~/.local/share/rondine/assets/<md5>.png`, cache condivisa per tutti i tipi);
la cronologia in RAM è limitata a 60 messaggi per chat aperta, le conversioni girano
in spawn_blocking e NESSUN frame completo resta a lungo nello heap. `smem`/`ps` per
verificare: il RSS tipico sta nelle decine di MB, non centinaia.
