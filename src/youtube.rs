//! "Paste URL" video download.  C#: `YouTubeHelper` driving `yt-dlp.exe`.
//!
//! The video is saved in the video library root so the headset can play it; progress is
//! published through `SharedStreamerSettings.DownloadState` / `DownloadProgress`, which both the
//! settings window and the headset display.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::task::AbortHandle;
use tracing::{info, warn};
use vd_protocol::types::DownloadState;

use crate::app::App;

/// The running download, if any (aborting it kills `yt-dlp`).
static CURRENT: Mutex<Option<AbortHandle>> = Mutex::new(None);

/// Reports whether `text` looks like a YouTube URL (C#: `url.Contains("youtu")`).
///
/// # Arguments
/// * `text` - clipboard text.
///
/// # Returns
/// `true` for `youtube.com` / `youtu.be` links.
pub fn is_youtube_url(text: &str) -> bool {
    let t = text.trim();
    (t.starts_with("http://") || t.starts_with("https://")) && t.contains("youtu") && !t.contains(char::is_whitespace)
}

/// Finds `yt-dlp`: next to this executable, on `PATH`, or in the original installation.
///
/// # Returns
/// The executable path, or `None`.
fn find_ytdlp() -> Option<PathBuf> {
    let name = if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        candidates.push(dir.join(name));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|d| d.join(name)));
    }
    if cfg!(windows) {
        candidates.push(PathBuf::from(r"C:\Program Files\Virtual Desktop Streamer\yt-dlp.exe"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Parses a `[download]  42.5% of …` progress line.
///
/// # Arguments
/// * `line` - one line of `yt-dlp` output.
///
/// # Returns
/// Progress in `0..=1`, if the line carries one.
fn parse_progress(line: &str) -> Option<f32> {
    let rest = line.strip_prefix("[download]")?.trim_start();
    let pct = rest.split('%').next()?.trim();
    rest.contains("% of").then(|| pct.parse::<f32>().ok()).flatten().map(|p| (p / 100.0).clamp(0.0, 1.0))
}

/// Publishes the download state.
///
/// # Arguments
/// * `app` - application context.
/// * `state` - new state.
/// * `progress` - progress in `0..=1`.
fn publish(app: &App, state: DownloadState, progress: f32) {
    app.state.streamer.send_modify(|s| {
        s.download_state = state;
        s.download_progress = progress;
    });
}

/// Starts downloading `url` into the video library (a running download is cancelled first).
///
/// # Arguments
/// * `app` - application context.
/// * `url` - YouTube URL.
///
/// # Errors
/// A message for the user when `yt-dlp` is not installed.
pub fn start(app: &Arc<App>, url: &str) -> Result<(), String> {
    let exe = find_ytdlp().ok_or_else(|| "yt-dlp was not found. Install it (or place it next to this program) to download videos.".to_owned())?;
    cancel(app);
    let root = crate::videos::videos_root(app);
    let ffmpeg = app.media.get().map(|m| m.ffmpeg.ffmpeg.clone());
    let url = url.trim().to_owned();
    let task_app = Arc::clone(app);
    let task = tokio::spawn(async move {
        let app = task_app;
        publish(&app, DownloadState::RetrievingInfo, 0.0);
        let mut cmd = crate::async_command(&exe);
        cmd.args(["-f", "bestvideo+bestaudio[ext=m4a]/mp4", "-o"])
            .arg(root.join("%(title)s.%(ext)s"))
            .args(["--compat-opt", "prefer-vp9-sort", "--restrict-filenames", "--no-check-certificate", "--newline"]);
        if let Some(ff) = ffmpeg {
            cmd.arg("--ffmpeg-location").arg(ff);
        }
        cmd.arg(&url).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "cannot start yt-dlp");
                publish(&app, DownloadState::None, 0.0);
                return;
            }
        };
        if let Some(out) = child.stdout.take() {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(p) = parse_progress(&line) {
                    publish(&app, DownloadState::Downloading, p);
                } else if let Some(dest) = line.strip_prefix("[download] Destination: ") {
                    info!(file = dest, "downloading video");
                }
            }
        }
        match child.wait().await {
            Ok(s) if s.success() => info!(%url, "video downloaded"),
            Ok(s) => warn!(%url, status = %s, "yt-dlp failed"),
            Err(e) => warn!(error = %e, "yt-dlp failed"),
        }
        publish(&app, DownloadState::None, 0.0);
    });
    *CURRENT.lock().expect("poisoned") = Some(task.abort_handle());
    Ok(())
}

/// Cancels the running download (C#: `YouTubeHelper.Cancel`).
///
/// # Arguments
/// * `app` - application context.
pub fn cancel(app: &App) {
    if let Some(h) = CURRENT.lock().expect("poisoned").take() {
        h.abort();
        publish(app, DownloadState::None, 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_progress_lines() {
        assert_eq!(parse_progress("[download]  42.5% of   10.00MiB at  1.00MiB/s ETA 00:05"), Some(0.425));
        assert_eq!(parse_progress("[download] Destination: x.mp4"), None);
    }

    #[test]
    fn recognises_youtube_urls() {
        assert!(is_youtube_url("https://youtu.be/abc"));
        assert!(is_youtube_url(" https://www.youtube.com/watch?v=abc "));
        assert!(!is_youtube_url("hello youtube"));
    }
}
