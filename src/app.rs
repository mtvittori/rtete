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
pub(crate) fn pinned_first(contacts: &mut [tg::DialogInfo]) {
    contacts.sort_by_key(|d| !d.pinned);
}

pub(crate) fn backoff_secs(prev: u32) -> Duration {
    let secs = 1u64 << prev.min(6); // u64: nessun overflow
    Duration::from_secs(secs.min(60))
}

/// Registra un fallimento (`failed` per gli asset chiave stringa,
/// `docs_failed` per i set) e fissa il prossimo tentativo col backoff
/// esponenziale di `backoff_secs`: 1s, 2s, 4s… fino a 60s.
fn bump_backoff<K: Eq + std::hash::Hash>(map: &mut HashMap<K, (Instant, u32)>, key: K) {
    let prev = map.remove(&key).map(|(_, n)| n).unwrap_or(0);
    map.insert(key, (Instant::now() + backoff_secs(prev), prev + 1));
}

/// Scala il mio voto dall'emoji indicata (true = il mio voto c'era);
/// l'item sparisce se il conteggio resta a zero.
fn retract(row: &mut tg::HistoryRow, mine_on: impl Fn(&tg::ReactionItem) -> bool) -> bool {
    match row.reactions.iter().position(mine_on) {
        // l'item passa solo se resta a zero (prima il retain toglieva
        // l'intera voce anche se altri hanno messo la stessa reazione)
        Some(i) => {
            let entry = &mut row.reactions[i];
            entry.count = entry.count.saturating_sub(1);
            if entry.count <= 0 {
                row.reactions.remove(i);
            }
            true
        }
        None => false,
    }
}

/// Comando grafico da eseguire dopo il frame ratatui.
pub enum Cmd {
    Place {
        rect: ratatui::layout::Rect,
        path: PathBuf,
    },
}

#[derive(Default)]
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
    /// id del messaggio a cui il prossimo invio risponde (Ctrl-R).
    pub replying: Option<i32>,
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
    /// messaggi citati fuori dalla cronologia caricata, per chat e id:
    /// `None` = eliminato. Solo per la riga di citazione delle risposte.
    pub quoted: HashMap<PeerId, HashMap<i32, Option<tg::HistoryRow>>>,
    /// chat con una richiesta `Quoted` in volo
    pub quoted_busy: HashSet<PeerId>,
    /// chat la cui ultima richiesta `Quoted` è fallita: prossimo tentativo
    pub quoted_failed: HashMap<PeerId, (Instant, u32)>,
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

    /// Guardia comune dei comandi sulla chat aperta (Ctrl-E/D/O/P/Y): azzera
    /// lo status, esige nessun pannello aperto (ricerca/sticker), una chat
    /// aperta e la sua cronologia caricata; altrimenti imposta la ragione
    /// dello status e ritorna `None`.
    pub(crate) fn chat_ready_ring(&mut self) -> Option<&VecDeque<tg::HistoryRow>> {
        self.status.clear();
        if self.show_search || self.show_sticker {
            return None;
        }
        let Some(chat) = self.current_chat() else {
            self.status = "apri prima una chat".into();
            return None;
        };
        match self.histories.get(&chat.peer_id) {
            Some(ring) => Some(ring),
            None => {
                self.status = "cronologia non ancora caricata".into();
                None
            }
        }
    }

    /// La riga scelta da un comando: quella sotto il cursore di selezione
    /// (se soddisfa `pred`), altrimenti l'ultima di `ring` che la soddisfa.
    pub(crate) fn picked_row(
        ring: &VecDeque<tg::HistoryRow>,
        cursor: Option<i32>,
        pred: impl Fn(&tg::HistoryRow) -> bool,
    ) -> Option<&tg::HistoryRow> {
        cursor
            .and_then(|cid| ring.iter().rev().find(|r| r.id == cid && pred(r)))
            .or_else(|| ring.iter().rev().find(|r| pred(r)))
    }

    /// Disarma l'eliminazione in attesa di conferma (qualunque tasto che
    /// non sia Invio la disarma, come in Telegram).
    pub(crate) fn disarm_delete(&mut self) {
        self.del_arm = false;
        self.del_id = None;
    }

    /// Scarta la foto degli appunti in attesa invio e rimuove il suo file
    /// temporaneo (usa `take`: la foto non è più inviabile).
    pub(crate) fn discard_pending_photo(&mut self) {
        if let Some(p) = self.pending_photo.take() {
            let _ = std::fs::remove_file(p);
        }
    }

    pub(crate) fn push_row(&mut self, peer_id: PeerId, row: tg::HistoryRow) {
        self.push_each(peer_id, std::iter::once(row));
    }

    pub(crate) fn push_rows(&mut self, peer_id: PeerId, rows: Vec<tg::HistoryRow>) {
        self.push_each(peer_id, rows);
    }

    /// Inserisce niente, una (o tante) righe in fondo alla cronologia una
    /// volta sola: dedup (uno stesso messaggio arriva sia dalla conferma
    /// dell'invio sia dalle notifiche live di Telegram, anche dai nostri
    /// stessi invii) e tetto RING_CAP.
    fn push_each<I>(&mut self, peer_id: PeerId, rows: I)
    where
        I: IntoIterator<Item = tg::HistoryRow>,
    {
        let ring = self.histories.entry(peer_id).or_default();
        for row in rows {
            if ring.iter().any(|r| r.id == row.id) {
                continue;
            }
            ring.push_back(row);
            while ring.len() > RING_CAP {
                ring.pop_front();
            }
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
                row.reply_quote = old.reply_quote.take();
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
        if retract(row, |r| r.mine && r.emoticon == emoticon) {
            return;
        }
        // cambio bersaglio: il mio voto vecchio cala (item via se resta a zero)
        retract(row, |r| r.mine && r.emoticon != emoticon);
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
        bump_backoff(&mut self.failed, key);
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

    /// idem per i messaggi citati: request_quoted gira a ogni frame
    pub(crate) fn quoted_failed(&mut self, peer: PeerId) {
        bump_backoff(&mut self.quoted_failed, peer);
    }

    pub(crate) fn docs_failed(&mut self, set_id: i64) {
        bump_backoff(&mut self.docs_failed, set_id);
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

    /// Chiede i messaggi citati dalle risposte della chat aperta che non
    /// sono né nella cronologia né già chiesti (una richiesta alla volta per
    /// chat; dopo un errore si riprova col backoff di `quoted_failed`).
    pub(crate) fn request_quoted(&mut self, req: &mpsc::UnboundedSender<Request>) {
        let Some(chat) = self.current_chat() else {
            return;
        };
        let (peer_ref, peer) = (chat.peer_ref, chat.peer_id);
        let cooling = self
            .quoted_failed
            .get(&peer)
            .is_some_and(|&(until, _)| Instant::now() < until);
        if cooling || self.quoted_busy.contains(&peer) {
            return;
        }
        let Some(ring) = self.histories.get(&peer) else {
            return;
        };
        let known = self.quoted.get(&peer);
        let mut ids: Vec<i32> = ring
            .iter()
            .filter_map(|r| r.reply_to)
            .filter(|id| !ring.iter().any(|r| r.id == *id))
            .filter(|id| !known.is_some_and(|k| k.contains_key(id)))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        // GetMessages accetta fino a 100 id
        ids.truncate(100);
        if ids.is_empty() {
            return;
        }
        self.quoted_busy.insert(peer);
        let _ = req.send(Request::Quoted { peer_ref, peer, ids });
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

#[cfg(test)]
mod tests {
    use super::*;
    use grammers_client::session::types::{PeerAuth, PeerId};

    fn dialog(name: &str, id: i64, pinned: bool) -> tg::DialogInfo {
        let pid = PeerId::user_unchecked(id);
        tg::DialogInfo {
            name: name.to_string(),
            peer_ref: tg::ChatRef {
                id: pid,
                auth: PeerAuth::default(),
            },
            peer_id: pid,
            is_group: false,
            unread: 0,
            pinned,
        }
    }

    #[test]
    fn pinned_first_remette_le_fissate_sopra() {
        let mut contacts =
            vec![dialog("a", 1, false), dialog("b", 2, true), dialog("c", 3, false), dialog("d", 4, true)];
        pinned_first(&mut contacts);
        let flags: Vec<bool> = contacts.iter().map(|d| d.pinned).collect();
        assert_eq!(flags, [true, true, false, false]);
        // ordinamento stabile: l'ordine relativo dentro ogni gruppo resta
        let names: Vec<&str> = contacts.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["b", "d", "a", "c"]);
    }

    #[test]
    fn bump_chat_lascia_ferme_le_chat_fissate() {
        let (f1, f2, a3, a4, a5) =
            (dialog("f1", 11, true), dialog("f2", 12, true), dialog("a3", 13, false), dialog("a4", 14, false), dialog("a5", 15, false));
        let mut app = App::default();
        app.contacts = vec![f1.clone(), f2.clone(), a3.clone(), a4.clone(), a5.clone()];
        app.contact_cursor = 4;

        // un messaggio da a5 la porta alla cima delle NON fissate
        app.bump_chat(a5.peer_id, None);
        let soon = (0..2).all(|i| app.contacts[i].pinned);
        assert!(soon, "le fissate devono restare nella loro posizione");
        assert_eq!(app.contacts[2].peer_id, a5.peer_id);
        assert_eq!(app.contacts[app.contact_cursor].peer_id, a5.peer_id);

        // una chat nuova entra sempre sotto le fissate
        let fresh = dialog("nuovo", 16, false);
        app.bump_chat(fresh.peer_id, Some(fresh.clone()));
        assert!(app.contacts.iter().take(2).all(|d| d.pinned));
        assert_eq!(app.contacts[2].peer_id, fresh.peer_id);

        // una fissata che riceve messaggi NON si muove per niente
        app.bump_chat(f2.peer_id, None);
        assert_eq!(app.contacts[1].peer_id, f2.peer_id);
        assert_eq!(app.contacts[2].peer_id, fresh.peer_id);
    }
}
