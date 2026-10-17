//! Filesystem helpers: sticker cache + webp→png conversion.
//!
//! Obiettivo: non trattenere mai frame in heap più a lungo di una conversione;
//! ogni sticker finisce come PNG su disco così kitty non dovrà ri-decodificarlo.

use std::path::PathBuf;

use crate::tg::stickers_dir;

/// Cache path for a given media key.
pub fn cache_png(key: &str) -> PathBuf {
    stickers_dir().join(format!("{key}.png"))
}

/// Cache path for the original raw bytes (debug + convert once only).
pub fn cache_raw(key: &str) -> PathBuf {
    stickers_dir().join(format!("{key}.webp"))
}

/// Download + convert a sticker into a PNG we can draw with kitty. Returns
/// the png path when ready. Downgrades animated webp to its first frame.
pub async fn convert_to_png(
    client: grammers_client::Client,
    media: &grammers_client::media::Media,
    key: String,
) -> Result<PathBuf, String> {
    let png_path = cache_png(&key);
    if png_path.exists() {
        return Ok(png_path);
    }
    let raw_path = cache_raw(&key);
    if !raw_path.exists() {
        tokio::fs::create_dir_all(stickers_dir())
            .await
            .map_err(|e| format!("cache dir: {e}"))?;
        client
            .download_media(media, raw_path.clone())
            .await
            .map_err(|e| format!("download: {e}"))?;
    }
    let src = raw_path.clone();
    let dst = png_path.clone();
    let converted = tokio::task::spawn_blocking(move || convert_sync(&src, &dst))
        .await
        .map_err(|e| format!("join: {e}"))??;
    if !converted {
        tokio::fs::remove_file(png_path).await.ok();
        return Err("conversione impossibile".into());
    }
    Ok(png_path)
}

fn convert_sync(src: &PathBuf, dst: &PathBuf) -> Result<bool, String> {
    use image::ImageEncoder;
    let bytes = std::fs::read(src).map_err(|e| format!("read: {e}"))?;
    let img = image::load_from_memory(&bytes).map_err(|e| format!("decode: {e}"))?;
    let rgba = img.to_rgba8();
    let file = std::fs::File::create(dst).map_err(|e| format!("create: {e}"))?;
    let encoder = image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file));
    encoder
        .write_image(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("encode: {e}"))?;
    Ok(true)
}
