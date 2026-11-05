//! GIF animate nella chat: i fotogrammi si decodificano con ffmpeg (mp4 di
//! Telegram e .gif) solo mentre la GIF è in vista, e si tengono in memoria
//! ridotti alla larghezza della bolla.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use iced::widget::image::Handle;

/// fotogrammi al massimo per GIF: oltre, si abbassa la cadenza
const MAX_FRAMES: f64 = 120.0;
const MAX_FPS: f64 = 20.0;
const MIN_FPS: f64 = 8.0;
/// durata assunta quando il file non la dichiara
const DEFAULT_SECS: f64 = 5.0;

#[derive(Debug)]
pub struct Anim {
    frames: Vec<Handle>,
    step: Duration,
}

impl Anim {
    /// il fotogramma da mostrare dopo `t` dall'inizio (il loop è continuo)
    pub fn frame_at(&self, t: Duration) -> &Handle {
        let step = self.step.as_millis().max(1);
        &self.frames[(t.as_millis() / step) as usize % self.frames.len()]
    }
}

/// larghezza, altezza e durata dichiarate dal file
fn probe(src: &Path) -> Result<(u32, u32, f64), String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=width,height:format=duration", "-of", "default=nw=1"])
        .arg(src)
        .output()
        .map_err(|e| format!("ffprobe: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix('='))
            .and_then(|v| v.trim().parse::<f64>().ok())
    };
    match (field("width"), field("height")) {
        (Some(w), Some(h)) if w >= 2.0 && h >= 2.0 => {
            Ok((w as u32, h as u32, field("duration").filter(|d| *d > 0.0).unwrap_or(DEFAULT_SECS)))
        }
        _ => Err("ffprobe: dimensioni mancanti".into()),
    }
}

/// Decodifica `src` a larghezza al massimo `max_w`, in loop continuo.
/// Bloccante: da lanciare fuori dal thread della GUI.
pub fn decode(src: &Path, max_w: u32) -> Result<Anim, String> {
    let (sw, sh, secs) = probe(src)?;
    let w = sw.min(max_w) & !1;
    let h = ((sh as f64 * w as f64 / sw as f64).round() as u32) & !1;
    if w == 0 || h == 0 {
        return Err("dimensioni nulle".into());
    }
    let fps = (MAX_FRAMES / secs).clamp(MIN_FPS, MAX_FPS).round();
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(src)
        .args(["-an", "-vf", &format!("fps={fps},scale={w}:{h}:flags=bilinear")])
        .args(["-frames:v", &(MAX_FRAMES as u32).to_string()])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let size = (w * h * 4) as usize;
    let frames: Vec<Handle> = out
        .stdout
        .chunks_exact(size)
        .map(|px| Handle::from_rgba(w, h, px.to_vec()))
        .collect();
    if frames.is_empty() {
        return Err("ffmpeg: nessun fotogramma".into());
    }
    Ok(Anim { frames, step: Duration::from_millis((1000.0 / fps) as u64) })
}
