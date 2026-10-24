//! Filesystem helpers: asset cache (sticker/foto/gif) + conversioni.
//!
//! Obiettivo: non trattenere mai frame in heap più a lungo di una conversione;
//! ogni asset finisce come PNG (statico o APNG) su disco così kitty lo
//! decodifica solo quando lo piazza.

use std::path::{Path, PathBuf};

use crate::tg::media_dir;

/// Cache path for a given media key.
pub fn cache_png(key: &str) -> PathBuf {
    media_dir().join(format!("{key}.png"))
}

/// Cache path for the original raw bytes (convert once only).
fn cache_raw(key: &str, ext: &str) -> PathBuf {
    media_dir().join(format!("{key}.{ext}"))
}

const CACHE_MAX_FILES: usize = 200;
const CACHE_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// Sfoltisce la cache degli asset: via i file più vecchi (per mtime) finché
/// restano al massimo ~200 file e ~32 MiB. Da chiamare all'avvio, prima che
/// il terminale riceva qualunque immagine.
pub fn prune_cache() {
    let Ok(rd) = std::fs::read_dir(media_dir()) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            md.is_file()
                .then(|| (md.modified().unwrap_or(std::time::UNIX_EPOCH), md.len(), e.path()))
        })
        .collect();
    // più recenti prima: si tiene la testa, si butta la coda
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    let (mut n, mut bytes) = (0usize, 0u64);
    for (_, len, path) in files {
        n += 1;
        bytes += len;
        if n > CACHE_MAX_FILES || bytes > CACHE_MAX_BYTES {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Scarica (se manca) e converte in PNG/APNG pronto per kitty.
pub async fn convert_to_png(
    client: grammers_client::Client,
    media: &grammers_client::media::Media,
    key: String,
) -> Result<PathBuf, String> {
    let png_path = cache_png(&key);
    if png_path.exists() {
        return Ok(png_path);
    }
    let raw_path = raw_source(&client, media).await?;
    let src = raw_path.clone();
    let dst = png_path.clone();
    let files = (src.clone(), dst.clone());
    match tokio::task::spawn_blocking(move || {
        let (src, dst) = files;
        convert_sync(&src, &dst)
    })
    .await
    {
        // un errore a conversione partita lascia dietro un png monco:
        // rimuovilo, altrimenti esists() lo restituisce roto per sempre
        Ok(Ok(())) => Ok(png_path),
        Ok(Err(e)) => {
            let _ = tokio::fs::remove_file(&dst).await;
            Err(e)
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&dst).await;
            Err(format!("join: {e}"))
        }
    }
}

fn raw_ext(media: &grammers_client::media::Media) -> &'static str {
    match media {
        grammers_client::media::Media::Photo(_) => "jpg",
        grammers_client::media::Media::Document(doc) => match doc.mime_type().unwrap_or("") {
            "image/gif" => "gif",
            "image/png" | "image/apng" => "png",
            "image/webp" => "webp",
            m if m.starts_with("image/jpeg") => "jpg",
            _ => "bin",
        },
        _ => "bin",
    }
}

/// L'originale in cache (foto a piena risoluzione per il visore), se c'è
/// ed è un formato che si apre al volo (jpg/png).
pub fn original(media: &grammers_client::media::Media) -> Option<PathBuf> {
    let ext = raw_ext(media);
    if !matches!(ext, "jpg" | "png") {
        return None;
    }
    let path = cache_raw(&crate::tg::media_key(media), ext);
    path.exists().then_some(path)
}

/// Scarica l'originale nella cache (se assente) e preleva il percorso con
/// l'estensione reale.
async fn raw_source(
    client: &grammers_client::Client,
    media: &grammers_client::media::Media,
) -> Result<PathBuf, String> {
    let key = crate::tg::media_key(media);
    let ext = raw_ext(media);
    // video: basta la miniatura più grande, non il file intero
    if let grammers_client::media::Media::Document(doc) = media {
        if doc.mime_type().unwrap_or("").starts_with("video/") {
            let raw_path = cache_raw(&key, "jpg");
            if !raw_path.exists() {
                let thumb = doc
                    .thumbs()
                    .into_iter()
                    .max_by_key(|t| t.size())
                    .ok_or("video senza miniatura")?;
                crate::tg::ensure_private_dir(&media_dir())
                    .map_err(|e| format!("cache dir: {e}"))?;
                client
                    .download_media(&thumb, raw_path.clone())
                    .await
                    .map_err(|e| format!("download: {e}"))?;
            }
            return Ok(raw_path);
        }
    }
    let raw_path = cache_raw(&key, ext);
    if !raw_path.exists() {
        crate::tg::ensure_private_dir(&media_dir())
            .map_err(|e| format!("cache dir: {e}"))?;
        client
            .download_media(media, raw_path.clone())
            .await
            .map_err(|e| format!("download: {e}"))?;
    }
    Ok(raw_path)
}

/// Rileva il formato dal magic header.
fn sniff(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        "png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "jpeg"
    } else if bytes.starts_with(b"GIF8") {
        "gif"
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "webp"
    } else {
        ""
    }
}

/// APNG? (chunk `acTL` nei primi 1 kB).
fn is_apng(bytes: &[u8]) -> bool {
    let head = bytes.get(..1024.min(bytes.len()));
    head.is_some_and(|h| h.windows(4).any(|w| w == b"acTL"))
}

/// Dimensioni dichiarate nell'header PNG (IHDR): nessuna decodifica necessaria.
fn png_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    let b = bytes.get(16..24)?;
    Some((
        u32::from_be_bytes(b[0..4].try_into().ok()?),
        u32::from_be_bytes(b[4..8].try_into().ok()?),
    ))
}

/// Dimensioni logiche dal GIF header (LE dopo la firma di 6 byte).
fn gif_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    let b = bytes.get(6..10)?;
    Some((
        u16::from_le_bytes(b[0..2].try_into().ok()?) as u32,
        u16::from_le_bytes(b[2..4].try_into().ok()?) as u32,
    ))
}

fn convert_sync(src: &Path, dst: &Path) -> Result<(), String> {
    let bytes = std::fs::read(src).map_err(|e| format!("read: {e}"))?;
    match (sniff(&bytes), is_apng(&bytes)) {
        // APNG: byte intatti, kitty anima in autonomia.
        ("png", true) => {
            std::fs::write(dst, &bytes).map_err(|e| format!("write: {e}"))?;
        }
        // PNG statico: se già piccolo, copia; se enorme, riscalalo.
        ("png", false) => {
            let (w, h) = png_dims(&bytes).unwrap_or((0, 0));
            if w > 0 && w.max(h) <= 1000 {
                std::fs::write(dst, &bytes).map_err(|e| format!("write: {e}"))?;
            } else {
                let img = decode_cap(&bytes)?;
                encode_png(&img, dst)?;
            }
        }
        // GIF: anima APNG solo se piccola; altrimenti primo frame riscalato.
        ("gif", _) => {
            let (w, h) = gif_dims(&bytes).unwrap_or((0, 0));
            if w > 0 && w.max(h) <= 700 {
                gif_to_apng(src, dst, 80)?;
            } else {
                let img = decode_cap(&bytes)?;
                encode_png(&img, dst)?;
            }
        }
        // tutto il resto: decodifica compatta → png
        _ => {
            let img = decode_cap(&bytes)?;
            encode_png(&img, dst)?;
        }
    }
    Ok(())
}

/// Decodifica con lato massimo limitato (bassa RAM; le foto di Telegram
/// rendono bene fino a ~900px).
fn decode_cap(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("decode: {e}"))?;
    const MAX_SIDE: u32 = 1000;
    let img = if img.width().max(img.height()) > MAX_SIDE {
        img.thumbnail(MAX_SIDE, MAX_SIDE)
    } else {
        img
    };
    Ok(img.to_rgba8())
}

fn encode_png(img: &image::RgbaImage, dst: &Path) -> Result<(), String> {
    use image::ImageEncoder;
    let file = std::fs::File::create(dst).map_err(|e| format!("create: {e}"))?;
    let encoder = image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file));
    encoder
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("encode: {e}"))?;
    Ok(())
}

/// Ricodifica una GIF in APNG (kitty anima gli APNG da solo).
/// Frame cap e delay per frame; da chiamare solo su gif ≤ 700px.
///
/// RAM piatta: due giri — il primo conta i frame (serve il numero esatto
/// per l'header APNG), il secondo scrive ogni frame appena lo decodifica.
/// Prima si accumulavano in RAM fino a 80 frame RGBA 700×700 ≈ 150 MB.
pub fn gif_to_apng(src: &Path, dst: &Path, max_frames: usize) -> Result<(), String> {
    use image::AnimationDecoder;
    use png::{BitDepth, ColorType, Encoder as PngEnc};

    let open = || std::fs::File::open(src).map_err(|e| format!("open: {e}"));
    let count = {
        let file = open()?;
        let decoder = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(file))
            .map_err(|e| format!("gif: {e}"))?;
        decoder.into_frames().take(max_frames).count()
    };
    if count == 0 {
        return Err("gif vuota".into());
    }

    // secondo giro: streaming, un frame alla volta
    let file = open()?;
    let decoder = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(file))
        .map_err(|e| format!("gif: {e}"))?;
    let mut frames = decoder.into_frames();
    let first = match frames.next() {
        Some(Ok(f)) => f,
        Some(Err(e)) => return Err(format!("frame: {e}")),
        None => return Err("gif vuota".into()),
    };
    let w = first.buffer().width();
    let h = first.buffer().height();
    if w == 0 || h == 0 {
        return Err("gif vuota".into());
    }
    let (num, denom) = first.delay().numer_denom_ms();
    let first_delay = (num.max(1) / denom.max(1)).clamp(20, 600) as u16;

    let out = std::fs::File::create(dst).map_err(|e| format!("create: {e}"))?;
    let mut enc = PngEnc::new(std::io::BufWriter::new(out), w, h);
    enc.set_color(ColorType::Rgba);
    enc.set_depth(BitDepth::Eight);
    enc.set_animated(count as u32, 0)
        .map_err(|e| e.to_string())?;
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    // il delay vale anche per il primo frame: senza, l'animazione partiva
    // sempre al massimo della velocità
    writer
        .set_frame_delay(first_delay, 1000)
        .map_err(|e| e.to_string())?;
    writer
        .write_image_data(first.buffer().as_raw())
        .map_err(|e| format!("write f0: {e}"))?;

    // dal secondo in poi, fino a completare il conteggio del primo giro
    let mut left = count - 1;
    while left > 0 {
        let item = match frames.next() {
            Some(i) => i,
            None => return Err("gif inattesa: frame finiti in anticipo".into()),
        };
        let frame = item.map_err(|e| format!("frame: {e}"))?;
        let (num, denom) = frame.delay().numer_denom_ms();
        let delay_ms = (num.max(1) / denom.max(1)).clamp(20, 600) as u16;
        let bw = frame.buffer().width();
        let bh = frame.buffer().height();
        if bw != w || bh != h {
            return Err(format!("frame inatteso {bw}x{bh} su {w}x{h}"));
        }
        writer
            .set_frame_delay(delay_ms, 1000)
            .map_err(|e| e.to_string())?;
        writer
            .write_image_data(frame.buffer().as_raw())
            .map_err(|e| format!("write f: {e}"))?;
        left -= 1;
    }
    Ok(())
}
