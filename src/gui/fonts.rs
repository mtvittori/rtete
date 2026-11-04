//! Motore dei font a runtime: elenca le famiglie installate con i loro pesi,
//! sceglie il peso normale e quello grassetto, e applica (o ripristina) la
//! famiglia come sans-serif globale di iced, tramite una famiglia virtuale.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use iced::advanced::graphics::text::cosmic_text::fontdb::{
    self, Database, FaceInfo, Family, Language, Style, Weight,
};
use iced::advanced::graphics::text::cosmic_text::skrifa::{self, MetadataProvider};
use iced::advanced::graphics::text::font_system;

/// Nome della famiglia virtuale con cui il font scelto viene esposto a iced.
const VIRTUAL_FAMILY: &str = "Rondine UI";
/// Carattere campione: una faccia che non lo contiene non è un font di testo.
const PROBE_CHAR: char = 'a';
/// Quanto il grassetto supera il peso normale scelto.
const BOLD_OFFSET: u16 = 300;
/// Peso massimo della scala CSS, tetto del grassetto.
const MAX_WEIGHT: u16 = 900;

/// Una voce del selettore: il font di sistema (`Default`) o una famiglia con i suoi pesi.
#[derive(Clone, Debug, PartialEq)]
pub enum FontPick {
    Default,
    Family { name: String, weights: Vec<u16> },
}

/// Elenco delle famiglie, una volta caricato.
static LIST: OnceLock<Vec<FontPick>> = OnceLock::new();

/// Vero mentre l'elenco si carica: evita di avviare due caricamenti.
static LOADING: AtomicBool = AtomicBool::new(false);

/// Nome della sans-serif di cosmic-text, letto prima della prima modifica.
static ORIGINAL_SANS: OnceLock<String> = OnceLock::new();

/// Id delle facce virtuali create dall'ultimo `apply`, per rimuoverle al cambio.
static VIRTUAL_FACES: Mutex<Vec<fontdb::ID>> = Mutex::new(Vec::new());

/// Le famiglie installate, con `Default` per prima; `None` finché non sono caricate.
/// Input: nessuno. Output: l'elenco caricato. Effetti: nessuno.
pub fn list() -> Option<&'static [FontPick]> {
    LIST.get().map(Vec::as_slice)
}

/// Carica in background l'elenco delle famiglie installate.
/// Idempotente: non fa nulla se l'elenco è già caricato o in caricamento.
/// Effetti: copia il database sotto un breve lock di scrittura, poi lavora in un task bloccante.
/// In caso di errore l'elenco resta vuoto e la chiamata successiva riprova.
pub async fn load_list() {
    if LIST.get().is_some() || LOADING.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Some(picks) = build_list().await {
        // Il flag di caricamento garantisce che sia l'unica scrittura.
        let _ = LIST.set(picks);
    }
    LOADING.store(false, Ordering::Release);
}

/// Costruisce l'elenco delle famiglie su un task bloccante.
/// Input: nessuno. Output: l'elenco, `None` se il database non è leggibile o il task fallisce.
/// Effetti: nessuno sullo stato globale.
async fn build_list() -> Option<Vec<FontPick>> {
    let database = clone_database()?;
    tokio::task::spawn_blocking(move || families(&database))
        .await
        .ok()
}

/// Copia il database dei font, tenendo il lock di scrittura solo il tempo della copia.
/// Input: nessuno. Output: la copia, `None` se il lock è avvelenato. Effetti: nessuno.
fn clone_database() -> Option<Database> {
    let mut system = font_system().write().ok()?;
    Some(system.raw().db().clone())
}

/// L'elenco completo: `Default` e poi le famiglie, in ordine alfabetico senza distinguere le maiuscole.
/// Input: il database. Output: le voci del selettore. Effetti: nessuno.
fn families(database: &Database) -> Vec<FontPick> {
    let mut groups = group_weights(database);
    groups.sort_by_cached_key(|(name, _)| name.to_lowercase());
    std::iter::once(FontPick::Default)
        .chain(groups.into_iter().map(|(name, weights)| FontPick::Family { name, weights }))
        .collect()
}

/// Raggruppa per famiglia le facce di testo, escluso il font virtuale, con pesi distinti e ordinati.
/// Input: il database. Output: coppie (nome, pesi). Effetti: nessuno.
fn group_weights(database: &Database) -> Vec<(String, Vec<u16>)> {
    let mut groups: BTreeMap<String, Vec<u16>> = BTreeMap::new();
    for face in database.faces().filter(|face| is_text_face(database, face)) {
        let Some(name) = display_name(face) else {
            continue;
        };
        if name != VIRTUAL_FAMILY {
            groups.entry(name).or_default().push(face.weight.0);
        }
    }
    groups
        .into_iter()
        .map(|(name, weights)| (name, distinct_sorted(weights)))
        .collect()
}

/// Vero se la faccia è normale (non corsiva), non ha una cmap simbolica e contiene il carattere campione.
/// Le facce con cmap simbolica (Wingdings, Webdings, Symbol, Marlett) sono scartate: skrifa le mapperebbe anche per 'a' tramite U+F061.
/// Input: il database e la faccia. Output: l'esito, `false` se i dati del font non sono leggibili.
/// Effetti: legge il file del font dal disco.
fn is_text_face(database: &Database, face: &FaceInfo) -> bool {
    face.style == Style::Normal
        && database
            .with_face_data(face.id, |data, index| {
                skrifa::FontRef::from_index(data, index).is_ok_and(|font| {
                    let charmap = font.charmap();
                    !charmap.is_symbol() && charmap.map(PROBE_CHAR).is_some()
                })
            })
            .unwrap_or(false)
}

/// Il nome da mostrare per una faccia: quello inglese se c'è, altrimenti il primo.
/// Input: la faccia. Output: il nome, `None` se la faccia non ne ha. Effetti: nessuno.
fn display_name(face: &FaceInfo) -> Option<String> {
    face.families
        .iter()
        .find(|(_, language)| *language == Language::English_UnitedStates)
        .or_else(|| face.families.first())
        .map(|(name, _)| name.clone())
}

/// Pesi senza duplicati, in ordine crescente.
/// Input: i pesi. Output: i pesi puliti. Effetti: nessuno.
fn distinct_sorted(mut weights: Vec<u16>) -> Vec<u16> {
    weights.sort_unstable();
    weights.dedup();
    weights
}

/// Il peso disponibile più vicino a `wanted`; a parità vince il più leggero.
/// Input: i pesi disponibili e il peso voluto. Output: il peso scelto, `None` se non ce ne sono.
/// Effetti: nessuno.
pub fn normal_weight(weights: &[u16], wanted: u16) -> Option<u16> {
    weights
        .iter()
        .copied()
        .min_by_key(|&weight| (weight.abs_diff(wanted), weight))
}

/// Il peso disponibile più vicino a `min(wanted + BOLD_OFFSET, MAX_WEIGHT)`; a parità vince il più pesante.
/// Input: i pesi disponibili e il peso normale voluto. Output: il peso scelto, `None` se non ce ne sono.
/// Effetti: nessuno.
pub fn bold_weight(weights: &[u16], wanted: u16) -> Option<u16> {
    let target = wanted.saturating_add(BOLD_OFFSET).min(MAX_WEIGHT);
    weights
        .iter()
        .copied()
        .min_by_key(|&weight| (weight.abs_diff(target), Reverse(weight)))
}

/// Applica una famiglia come sans-serif globale di iced, o ripristina il default.
/// Input: il nome della famiglia (`None` per il default) e il peso normale voluto.
/// Effetti: rimuove le facce virtuali precedenti, cambia la sans-serif del database globale
/// e richiede il ridisegno del testo. Se un lock è avvelenato non fa nulla.
pub fn apply(family: Option<&str>, weight: u16) {
    let Ok(mut system) = font_system().write() else {
        return;
    };
    let Ok(mut virtual_ids) = VIRTUAL_FACES.lock() else {
        return;
    };
    let database = system.raw().db_mut();
    let original =
        ORIGINAL_SANS.get_or_init(|| database.family_name(&Family::SansSerif).to_owned());
    remove_faces(database, &mut virtual_ids);
    let faces = family.map_or_else(Vec::new, |name| upright_faces(database, name));
    if faces.is_empty() {
        database.set_sans_serif_family(original);
    } else {
        *virtual_ids = install_virtual(database, &faces, weight);
        database.set_sans_serif_family(VIRTUAL_FAMILY);
    }
    system.load_font(Cow::Owned(Vec::new()));
}

/// Rimuove dal database le facce virtuali indicate e svuota la lista.
/// Input: il database e la lista degli id. Output: nessuno. Effetti: modifica database e lista.
fn remove_faces(database: &mut Database, ids: &mut Vec<fontdb::ID>) {
    for id in ids.drain(..) {
        database.remove_face(id);
    }
}

/// Le facce non corsive di una famiglia, cercata per uno qualsiasi dei suoi nomi.
/// Input: il database e il nome. Output: copie delle facce, vuoto se la famiglia non ne ha.
/// Effetti: nessuno.
fn upright_faces(database: &Database, family: &str) -> Vec<FaceInfo> {
    database
        .faces()
        .filter(|face| face.style == Style::Normal && has_family(face, family))
        .cloned()
        .collect()
}

/// Vero se la faccia ha `family` tra i suoi nomi.
/// Input: la faccia e il nome. Output: l'esito. Effetti: nessuno.
fn has_family(face: &FaceInfo, family: &str) -> bool {
    face.families.iter().any(|(name, _)| name == family)
}

/// Crea le facce virtuali, normale e grassetto, copiando quelle della famiglia scelta.
/// Input: le facce della famiglia, il peso normale voluto. Output: gli id delle facce create.
/// Effetti: aggiunge facce al database.
fn install_virtual(database: &mut Database, faces: &[FaceInfo], weight: u16) -> Vec<fontdb::ID> {
    let weights: Vec<u16> = faces.iter().map(|face| face.weight.0).collect();
    let targets = [
        (normal_weight(&weights, weight), Weight::NORMAL),
        (bold_weight(&weights, weight), Weight::BOLD),
    ];
    let mut ids = Vec::new();
    for (source, target) in targets {
        for face in faces.iter().filter(|face| Some(face.weight.0) == source) {
            let virtual_face = FaceInfo {
                families: vec![(VIRTUAL_FAMILY.to_owned(), Language::English_UnitedStates)],
                weight: target,
                ..face.clone()
            };
            ids.push(database.push_face_info(virtual_face));
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Il peso normale è il disponibile più vicino, anche con pesi a metà strada.
    /// Input: pesi e peso voluto. Output: asserzioni. Effetti: nessuno.
    #[test]
    fn normal_weight_takes_closest_available() {
        assert_eq!(normal_weight(&[300, 400, 600, 700], 400), Some(400));
        assert_eq!(normal_weight(&[300, 400, 600, 700], 600), Some(600));
        assert_eq!(normal_weight(&[350, 400, 600, 700], 350), Some(350));
        assert_eq!(normal_weight(&[400], 500), Some(400));
        assert_eq!(normal_weight(&[300, 600], 400), Some(300));
    }

    /// Il grassetto è il disponibile più vicino a peso normale + 300, a parità il più pesante.
    /// Input: pesi e peso normale voluto. Output: asserzioni. Effetti: nessuno.
    #[test]
    fn bold_weight_takes_closest_to_wanted_plus_offset() {
        assert_eq!(bold_weight(&[300, 400, 600, 700], 400), Some(700));
        assert_eq!(bold_weight(&[300, 400, 600, 700], 600), Some(700));
        assert_eq!(bold_weight(&[350, 400, 600, 700], 350), Some(700));
        assert_eq!(bold_weight(&[400], 500), Some(400));
    }

    /// Senza pesi disponibili non si sceglie nulla.
    /// Input: nessuno. Output: asserzioni. Effetti: nessuno.
    #[test]
    fn weights_are_none_when_empty() {
        assert_eq!(normal_weight(&[], 400), None);
        assert_eq!(bold_weight(&[], 400), None);
    }
}
