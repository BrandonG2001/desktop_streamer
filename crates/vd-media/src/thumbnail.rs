//! Video thumbnails for the headset's file browser.  C#: `VideoStreamer` thumbnail code, which
//! runs `ffmpeg -ss <t> -y -i "<file>" -vf scale=<w>… -vframes 1 …` with an HDR tone-mapping
//! prefix (`zscale=transfer=linear,tonemap=hable,zscale=transfer=bt709,format=yuv420p,`).

use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

use crate::error::{MediaError, Result};
use crate::probe::MediaInfo;
use crate::version::Ffmpeg;

/// HDR → SDR filter prefix used by the original.
pub const HDR_TONEMAP: &str = "zscale=transfer=linear,tonemap=hable,zscale=transfer=bt709,format=yuv420p,";

/// Renders a JPEG thumbnail.
///
/// # Arguments
/// * `ff` - FFmpeg installation.
/// * `info` - probe result (for duration and HDR detection).
/// * `path` - media file.
/// * `width` - thumbnail width (height keeps the aspect ratio).
///
/// # Returns
/// JPEG bytes.
///
/// # Errors
/// [`MediaError::Ffmpeg`] if no frame could be extracted.
pub async fn render_jpeg(ff: &Ffmpeg, info: &MediaInfo, path: &Path, width: u32) -> Result<Vec<u8>> {
    // Seek 10 % into the file (at most 10 s, as the original's "00:00:10" default).
    let dur = info.duration_ticks() as f64 / 1e7;
    let at = if dur > 0.0 { (dur * 0.1).min(10.0) } else { 0.0 };
    let hdr = info.video().is_some_and(|v| v.is_hdr()) && ff.has_input("zscale") && ff.has_input("tonemap");
    let vf = format!("{}scale={width}:-2", if hdr { HDR_TONEMAP } else { "" });
    let out = Command::new(&ff.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-ss", &format!("{at:.3}"), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-vf", &vf, "-f", "image2pipe", "-c:v", "mjpeg", "-q:v", "4", "pipe:1"])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(MediaError::Ffmpeg(String::from_utf8_lossy(&out.stderr).trim().to_owned()));
    }
    Ok(out.stdout)
}
