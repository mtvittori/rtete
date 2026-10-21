//! Stato applicativo: `App`, i suoi metodi di aggiornamento e il comando grafico `Cmd`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use grammers_client::media::Media;
use grammers_client::session::types::PeerId;
use tokio::sync::mpsc;

use crate::send::Request;
use crate::tg;

/// Messaggi tenuti in RAM per la chat aperta (guardia memoria).
pub const MAX_HISTORY: usize = 60;
/// righe tenute in memoria per chat (lo scorrimento all'indietro si ferma qui)
/// messaggi per pagina nello scorrimento all'indietro
pub const OLDER_PAGE: usize = 60;
pub const RING_CAP: usize = 500;
/// Colonne della griglia sticker nel pannello.
pub const STICKER_COLS: usize = 4;

/// Dopo `prev` fallimenti, il prossimo tentativo tra 1, 2, 4… fino a 60s:
/// backoff esponenziale unico per conversioni e richieste di rete.
pub(crate) fn backoff_secs(prev: u32) -> Duration {
    let secs = 1u64 << prev.min(6); // u64: nessun overflow
    Duration::from_secs(secs.min(60))
}

/// Comando grafico da eseguire dopo il frame ratatui.
pub enum Cmd {
    Place {
        rect: ratatui::layout::Rect,
        path: PathBuf,
    },
}

pub struct App {
    pub contacts: Vec<tg::DialogInfo>,
    pub contact_cursor: usize,
    pub open: Option<PeerId>,
    pub histories: HashMap<PeerId, VecDeque<tg::HistoryRow>>,
    pub compose: String,
    pub status: String,
    /// true: pannello sticker visibile.
    pub show_sticker: bool,
    /// false: lista set, true: dentro un set.
    pub set_level: bool,
    /// cursore nella lista dei set.
    pub set_cursor: usize,
    /// cursore nella griglia del set aperto.
    pub sticker_cursor: usize,
    /// None: lista in caricamento, Some: set installati.
    pub sets: Option<Vec<tg::StickerSetInfo>>,
    /// set_id -> sticker del set (scaricati on-demand).
    pub set_docs: HashMap<i64, Vec<Media>>,
    /// set il cui download è in pausa di backoff: set_id → (riprova da, n).
    pub docs_failed: HashMap<i64, (Instant, u32)>,
    /// id del set aperto.
    pub open_set: Option<i64>,
    /// media_key -> png convertito.
    pub previews: HashMap<String, PathBuf>,
    /// conversioni in volo.
    pub pending: HashSet<String>,
    /// asset falliti: chiave → (prossimo tentativo, quante volte). Il
    /// backoff evita il loop infinito download/converti di
    /// refresh_preview_queue su un asset irrimediabile.
    pub failed: HashMap<String, (Instant, u32)>,
    /// il nome dell'utente connesso (etichette "Nome │ testo").
    pub me: String,
    /// messaggio selezionato con ↑/↓ nella chat (id); None = vista "live".
    pub msg_cursor: Option<i32>,
    /// id del messaggio che Ctrl-D ha scelto da cancellare (conferma Invio).
    pub del_id: Option<i32>,
    /// id del messaggio che stiamo modificando (Ctrl-E).
    pub editing: Option<i32>,
    /// eliminazione armata da Ctrl-D: il prossimo Invio la conferma.
    pub del_arm: bool,
    /// ricerca nella chat aperta (Ctrl-F).
    pub show_search: bool,
    pub search_q: String,
    pub search_cursor: usize,
    /// None: ricerca ancora da lanciare; Some: risultati già ricevuti.
    pub search_results: Option<Vec<tg::SearchHit>>,
    /// righe (y schermo, id messaggio) dell'ultimo frame: click → selezione.
    pub mouse_map: Vec<(u16, i32)>,
    /// link a schermo: (riga, x inizio, x fine, url) per il click → browser.
    pub link_map: Vec<(u16, u16, u16, String)>,
    /// foto dagli appunti pronta da inviare (percorso del file in tmp).
    pub pending_photo: Option<String>,
    /// pannello reazioni veloci aperto sul messaggio selezionato.
    pub react_open: bool,
    /// indice nella lista tg::QUICK_REACTIONS.
    pub react_idx: usize,
    /// ultimo mark_as_read mandato: soglia anti-tempesta sui burst di
    /// messaggi nuovi nella chat aperta.
    pub read_mark: Option<Instant>,
    /// chat con una richiesta di messaggi più vecchi in volo
    pub older_busy: HashSet<PeerId>,
    /// chat senza altro da caricare (inizio raggiunto o RING_CAP)
    pub older_done: HashSet<PeerId>,
    /// ↑ è arrivato alla riga più vecchia: il ciclo chiede la pagina prima
    pub want_older: bool,
    pub quit: bool,
}

impl App {
    pub fn current_chat(&self) -> Option<&tg::DialogInfo> {
        self.open
            .as_ref()
            .and_then(|pid| self.contacts.iter().find(|c| tg::same_peer(c.peer_id, *pid)))
    }

    /// Limite di Telegram in caratteri: 4096 il testo, 1024 la didascalia
    /// di una foto.
    pub fn compose_cap(&self) -> usize {
        if self.pending_photo.is_some() {
            1024
        } else {
            4096
        }
    }

    pub fn open_history(&self) -> Option<&VecDeque<tg::HistoryRow>> {
        let chat = self.current_chat()?;
        self.histories.get(&chat.peer_id)
    }

    pub(crate) fn push_row(&mut self, peer_id: PeerId, row: tg::HistoryRow) {
        let ring = self.histories.entry(peer_id).or_default();
        // dedup: uno stesso messaggio arriva sia dalla conferma dell'invio
        // sia dalle notifiche live di Telegram (anche dai nostri stessi invii)
        if ring.iter().any(|r| r.id == row.id) {
            return;
        }
        ring.push_back(row);
        while ring.len() > RING_CAP {
            ring.pop_front();
        }
    }

    pub(crate) fn push_rows(&mut self, peer_id: PeerId, rows: Vec<tg::HistoryRow>) {
        let ring = self.histories.entry(peer_id).or_default();
        for row in rows {
            if ring.iter().any(|r| r.id == row.id) {
                continue;
            }
            ring.push_back(row);
        }
        while ring.len() > RING_CAP {
            ring.pop_front();
        }
    }

    /// Porta la chat in cima alla lista come fa Telegram a ogni messaggio,
    /// ma sotto le chat fissate (preferiti); se è nuova la inserisce
    /// (`fresh`). Il cursore resta sulla stessa chat.
    pub(crate) fn bump_chat(&mut self, peer_id: PeerId, fresh: Option<tg::DialogInfo>) {
        let sel = self.contacts.get(self.contact_cursor).map(|c| c.peer_id);
        let pinned_len = self.contacts.iter().take_while(|c| c.pinned).count();
        match self.contacts.iter().position(|c| tg::same_peer(c.peer_id, peer_id)) {
            Some(i) if self.contacts[i].pinned => return,
            Some(i) if i == pinned_len => return,
            Some(i) => {
                let c = self.contacts.remove(i);
                self.contacts.insert(pinned_len, c);
            }
            None => match fresh {
                Some(d) => self.contacts.insert(pinned_len, d),
                None => return,
            },
        }
        if let Some(i) =
            sel.and_then(|s| self.contacts.iter().position(|c| tg::same_peer(c.peer_id, s)))
        {
            self.contact_cursor = i;
        }
    }

    /// Messaggi più vecchi caricati con lo scorrimento all'indietro (oldest
    /// first): vanno in testa al ring, senza superare RING_CAP. Ritorna
    /// quante righe sono entrate davvero.
    pub(crate) fn prepend_rows(&mut self, peer_id: PeerId, rows: Vec<tg::HistoryRow>) -> usize {
        let ring = self.histories.entry(peer_id).or_default();
        let room = RING_CAP.saturating_sub(ring.len());
        let fresh: Vec<tg::HistoryRow> = rows
            .into_iter()
            .filter(|r| !ring.iter().any(|x| x.id == r.id))
            .collect();
        let skip = fresh.len().saturating_sub(room);
        let n = fresh.len() - skip;
        for row in fresh.into_iter().skip(skip).rev() {
            ring.push_front(row);
        }
        n
    }

    /// Sostituisce (o inserisce in coda) una riga stessa-id: usato per gli
    /// edit e per il feedback locali.
    pub(crate) fn replace_row(&mut self, peer_id: PeerId, row: tg::HistoryRow) {
        let ring = self.histories.entry(peer_id).or_default();
        if let Some(old) = ring.iter_mut().find(|r| r.id == row.id) {
            let mut row = row;
            // la riga locale di un edit non porta orario né contesto
            if row.date == 0 {
                row.date = old.date;
                row.reply_to = old.reply_to;
                row.fwd = old.fwd.take();
                row.reactions = std::mem::take(&mut old.reactions);
            }
            *old = row;
        } else {
            ring.push_back(row);
            while ring.len() > RING_CAP {
                ring.pop_front();
            }
        }
    }

    /// Sostituisce (o applica) le reazioni di un messaggio: update server
    /// o toggle locale ottimistico.
    pub(crate) fn apply_reactions(&mut self, peer_id: PeerId, msg_id: i32, items: Vec<tg::ReactionItem>) {
        let Some(ring) = self.histories.get_mut(&peer_id) else {
            return;
        };
        if let Some(row) = ring.iter_mut().find(|r| r.id == msg_id) {
            row.reactions = items;
        }
    }

    /// Toggle locale immediato di una reazione sull'ultimo cursore (il
    /// server conferma poi con l'update vero e proprio).
    pub(crate) fn optimistic_react(&mut self, emoticon: &str) {
        let Some(chat) = self.current_chat() else {
            return;
        };
        let (pid, mid) = (chat.peer_id, self.msg_cursor);
        let Some(mid) = mid else {
            return;
        };
        let Some(ring) = self.histories.get_mut(&pid) else {
            return;
        };
        let Some(row) = ring.iter_mut().find(|r| r.id == mid) else {
            return;
        };
        // Telegram senza Premium ammette una sola reazione per messaggio:
        // se il mio voto sta già su questa emoji la tolgo; se sta su
        // un'altra cambia bersaglio; il mio voto non resta mai su due item
        if let Some(i) = row
            .reactions
            .iter()
            .position(|r| r.mine && r.emoticon == emoticon)
        {
            // il mio voto cala; l'item passa solo se resta a zero
            // (prima il retain toglieva l'intera voce anche se altri hanno
            // messo la stessa reazione)
            let entry = &mut row.reactions[i];
            entry.count = entry.count.saturating_sub(1);
            if entry.count <= 0 {
                row.reactions.remove(i);
            }
            return;
        }
        // cambio bersaglio: il mio voto vecchio cala (item via se resta a zero)
        if let Some(i) = row
            .reactions
            .iter()
            .position(|r| r.mine && r.emoticon != emoticon)
        {
            let entry = &mut row.reactions[i];
            entry.count = entry.count.saturating_sub(1);
            if entry.count <= 0 {
                row.reactions.remove(i);
            }
        }
        match row.reactions.iter_mut().find(|r| r.emoticon == emoticon) {
            Some(item) => {
                item.count += 1;
                item.mine = true;
            }
            None => row.reactions.push(tg::ReactionItem {
                emoticon: emoticon.to_string(),
                count: 1,
                mine: true,
            }),
        }
    }

    /// Coda le conversioni degli sticker visibili non in cache. Un asset
    /// appena fallito non parte di nuovo finché il backoff non scade.
    pub(crate) fn ensure_previews<'a>(
        &mut self,
        req: &mpsc::UnboundedSender<Request>,
        medias: impl IntoIterator<Item = &'a Media>,
    ) {
        for media in medias {
            let key = tg::media_key(media);
            if self.previews.contains_key(&key) || self.pending.contains(&key) {
                continue;
            }
            if let Some(&(until, _)) = self.failed.get(&key) {
                if Instant::now() < until {
                    continue;
                }
            }
            if self.pending.insert(key) {
                let _ = req.send(Request::Conv(media.clone()));
            }
        }
    }

    /// Registra un asset fallito e fissa il prossimo tentativo col backoff
    /// esponenziale: 1s, 2s, 4s… fino a 60s. refresh_preview_queue gira a
    /// ogni cambio di layout: senza questo un singolo download guasto (rete
    /// assente, webp corrotto) verrebbe rilanciato all'infinito.
    pub(crate) fn conversion_failed(&mut self, key: String) {
        let prev = self.failed.remove(&key).map(|(_, n)| n).unwrap_or(0);
        self.failed
            .insert(key, (Instant::now() + backoff_secs(prev), prev + 1));
    }

    /// idem per i download di set: senza pausa, ogni freccia sul set
    /// rilanciava la richiesta e un set guasto martellava la rete; un
    /// errore non deve neanche marca il set come "caricato" (vuoto eterno)
    pub(crate) fn docs_cooling(&self, set_id: i64) -> bool {
        self.docs_failed
            .get(&set_id)
            .map(|&(until, _)| Instant::now() < until)
            .unwrap_or(false)
    }

    pub(crate) fn docs_failed(&mut self, set_id: i64) {
        let prev = self.docs_failed.remove(&set_id).map(|(_, n)| n).unwrap_or(0);
        self.docs_failed
            .insert(set_id, (Instant::now() + backoff_secs(prev), prev + 1));
    }

    pub(crate) fn docs_fine(&mut self, set_id: i64) {
        self.docs_failed.remove(&set_id);
    }

    /// true una volta ogni READ_MARK_GAP (o alla prima richiesta): limita i
    /// mark_as_read quando nella chat aperta arrivano raffiche di messaggi.
    pub(crate) fn read_mark_due(&mut self) -> bool {
        const READ_MARK_GAP: Duration = Duration::from_secs(2);
        let now = Instant::now();
        let due = self
            .read_mark
            .map(|t| now.duration_since(t) >= READ_MARK_GAP)
            .unwrap_or(true);
        if due {
            self.read_mark = Some(now);
        }
        due
    }
}

impl App {
    /// Se ↑ ha toccato il fondo della storia in memoria, chiede la pagina
    /// precedente (una per volta, mai oltre l'inizio o RING_CAP).
    pub(crate) fn request_older(&mut self, req: &mpsc::UnboundedSender<Request>) {
        if !std::mem::take(&mut self.want_older) {
            return;
        }
        let Some(chat) = self.current_chat() else {
            return;
        };
        let (peer_ref, peer) = (chat.peer_ref, chat.peer_id);
        if self.older_busy.contains(&peer) || self.older_done.contains(&peer) {
            return;
        }
        let Some(ring) = self.histories.get(&peer) else {
            return;
        };
        if ring.len() >= RING_CAP {
            self.older_done.insert(peer);
            self.status = format!("limite di {RING_CAP} messaggi in memoria");
            return;
        }
        let Some(before) = ring.front().map(|r| r.id) else {
            return;
        };
        self.older_busy.insert(peer);
        self.status = "carico messaggi precedenti…".into();
        let _ = req.send(Request::HistOlder { peer_ref, peer, before });
    }

    pub(crate) fn refresh_preview_queue(&mut self, req: &mpsc::UnboundedSender<Request>) {
        let mut medias: Vec<Media> = Vec::new();
        if let Some(ring) = self.open_history() {
            medias.extend(ring.iter().filter_map(|r| match &r.kind {
                tg::RowKind::Sticker {
                    media, animated, ..
                } => {
                    // tgs/webm non convertibili: solo etichetta
                    if *animated {
                        None
                    } else {
                        Some(media.clone())
                    }
                }
                tg::RowKind::Photo { media, .. } => Some(media.clone()),
                _ => None,
            }));
        }
        if self.show_sticker && self.set_level {
            if let Some(docs) = self.open_set.and_then(|id| self.set_docs.get(&id)).cloned() {
                medias.extend(docs);
            }
            medias.truncate(24);
        }
        self.ensure_previews(req, medias.iter());
    }
}
