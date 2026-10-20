//! Video library and pass-through playback.  C#: `VirtualDesktop.Streamer.VideoStreamer`.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{info, warn};
use vd_media::probe::probe;
use vd_media::reader::{MediaPacket, PlaybackRequest, VideoFilePlayback, is_video_file};
use vd_protocol::math::{Rect, Size2};
use vd_protocol::messages::{AudioTrack, VideoEntry, encode_audio_tracks, encode_videos, system_time_to_ticks, video_entry_flags};
use vd_protocol::types::{StreamingSource, VideoCodec};
use vd_protocol::wire::{FrameHeader, StreamDescriptor};
use vd_protocol::{MessageType, NetMessage};

use crate::app::{App, VideoCommand};

/// Root folder of the library (`StreamerSettings.VideosRootPath`, default the user's Videos folder).
///
/// # Arguments
/// * `app` - application context.
///
/// # Returns
/// The root path.
pub fn videos_root(app: &App) -> PathBuf {
    app.settings.lock().expect("poisoned").videos_root_path.clone().map(PathBuf::from).or_else(dirs::video_dir).unwrap_or_else(|| PathBuf::from("."))
}

/// Joins a headset-supplied relative path to the root, rejecting `..` and absolute components.
///
/// # Arguments
/// * `root` - library root.
/// * `rel` - path from the headset (`/` separators).
///
/// # Returns
/// The safe absolute path, or `None` if the path escapes the root.
pub fn resolve(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = Path::new(rel.trim_start_matches(['/', '\\']));
    if rel.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) {
        return None;
    }
    Some(root.join(rel))
}

/// Sends the `Videos` listing for a folder.  C#: `VideoStreamer` `RequestVideos` handler.
///
/// # Arguments
/// * `app` - application context.
/// * `rel` - folder relative to the root.
pub async fn send_listing(app: &App, rel: &str) {
    let root = videos_root(app);
    let mut entries = Vec::new();
    if let Some(dir) = resolve(&root, rel) {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut items: Vec<_> = rd.filter_map(|e| e.ok()).collect();
            items.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
            for e in items {
                let Ok(meta) = e.metadata() else { continue };
                let name = e.file_name().to_string_lossy().into_owned();
                let hidden = if name.starts_with('.') { video_entry_flags::HIDDEN } else { 0 };
                let relpath = e.path().strip_prefix(&root).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or(name);
                let modified = meta.modified().map(system_time_to_ticks).unwrap_or(0);
                if meta.is_dir() {
                    entries.push(VideoEntry { path: relpath, size: 0, modified_ticks: modified, flags: hidden | video_entry_flags::DIRECTORY });
                } else if is_video_file(&e.path()) {
                    entries.push(VideoEntry { path: relpath, size: meta.len() as i64, modified_ticks: modified, flags: hidden });
                }
            }
        }
    }
    match encode_videos(&entries) {
        Ok(msg) => app.send(&msg).await,
        Err(e) => warn!(error = %e, "cannot encode video listing"),
    }
}

struct Playing {
    playback: VideoFilePlayback,
    path: String,
    use_annex_b: bool,
    /// Last presentation time sent, in source-time ticks (used to restart at the same position).
    position: i64,
    /// Playback speed the presentation times were divided by.
    speed: f32,
}

async fn start(app: &App, path: &str, position: i64, audio_stream: i8) -> Option<Playing> {
    let media = app.media.get()?;
    let file = resolve(&videos_root(app), path)?;
    let info = match probe(&media.ffmpeg, &file).await {
        Ok(i) => i,
        Err(e) => {
            warn!(error = %e, "Error trying to play video");
            return None;
        }
    };
    let speed = app.state.mobile.borrow().video_playback_speed;
    let req = PlaybackRequest { path: file, start: position, audio_stream: (audio_stream >= 0).then_some(audio_stream as u32), speed };
    let playback = match VideoFilePlayback::start(&media.ffmpeg, &info, &req) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Error trying to play video");
            return None;
        }
    };
    let v = info.video()?;
    let codec = if v.codec_name == "hevc" { VideoCodec::HEVC } else { VideoCodec::H264 };
    let duration = info.duration_ticks();
    let mut d = StreamDescriptor::desktop(codec, v.fps().unwrap_or(30.0).round() as i32, &[(Size2::new(v.width.unwrap_or(0) as i32, v.height.unwrap_or(0) as i32), Rect { x: (duration >> 32) as i32, y: (duration & 0xFFFF_FFFF) as i32, width: 0, height: 0 })]);
    d.source = StreamingSource::Video as u8;
    let _ = app.channels.video.send_descriptor(&d).await;
    let tracks: Vec<AudioTrack> = info.audio().iter().enumerate().map(|(i, a)| AudioTrack { name: a.display_name(), channels: a.channels.unwrap_or(2) as u8, index: i as i8 }).collect();
    if let Ok(msg) = encode_audio_tracks(&tracks) {
        app.send(&msg).await;
    }
    app.state.streamer.send_modify(|s| s.streaming_source = StreamingSource::Video);
    info!(path, "video playback");
    Some(Playing { playback, path: path.to_owned(), use_annex_b: app.state.mobile.borrow().use_annex_b, position, speed: if speed > 0.0 { speed } else { 1.0 } })
}

async fn stop(app: &App, current: &mut Option<Playing>, notify: bool) {
    if let Some(mut p) = current.take() {
        p.playback.stop().await;
        if notify {
            app.send(&NetMessage::new(MessageType::VideoStopped)).await;
        }
        app.state.streamer.send_modify(|s| {
            if s.streaming_source == StreamingSource::Video {
                s.streaming_source = StreamingSource::Desktop;
            }
        });
        app.request_keyframe_all();
    }
}

/// Video player task.
///
/// # Arguments
/// * `app` - application context.
/// * `rx` - commands from the dispatcher.
pub async fn run(app: Arc<App>, mut rx: mpsc::UnboundedReceiver<VideoCommand>) {
    let mut current: Option<Playing> = None;
    let mut dyn_rx = app.state.dynamic.subscribe();
    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { return };
                match cmd {
                    VideoCommand::Play { path, position, audio_stream } => {
                        stop(&app, &mut current, false).await;
                        current = start(&app, &path, position, audio_stream).await;
                    }
                    VideoCommand::Seek(pos) => {
                        if let Some(path) = current.as_ref().map(|p| p.path.clone()) {
                            stop(&app, &mut current, false).await;
                            current = start(&app, &path, pos, -1).await;
                        }
                    }
                    VideoCommand::SetAudioStream(stream) => {
                        // The CLI decoder cannot switch tracks live; restart at the current position.
                        if let Some((path, pos)) = current.as_ref().map(|p| (p.path.clone(), p.position)) {
                            stop(&app, &mut current, false).await;
                            current = start(&app, &path, pos, stream).await;
                        }
                    }
                    VideoCommand::Stop => stop(&app, &mut current, false).await,
                }
            }
            pkt = async { match current.as_mut() { Some(p) => p.playback.next().await, None => std::future::pending().await } } => {
                let annex_b = current.as_ref().is_some_and(|p| p.use_annex_b);
                match pkt {
                    Some(MediaPacket::Video { data, pts, keyframe }) => {
                        if let Some(p) = current.as_mut() {
                            p.position = (pts as f64 * f64::from(p.speed)) as i64;
                        }
                        let header = FrameHeader::video(pts, keyframe, false, data.len() as i32);
                        if app.channels.video.send_frame(header, &data, annex_b).await.is_err() {
                            stop(&app, &mut current, false).await;
                        }
                    }
                    Some(MediaPacket::Audio(pcm)) => { let _ = app.channels.audio.send_pcm(&pcm).await; }
                    Some(MediaPacket::EndOfStream) | None => stop(&app, &mut current, true).await,
                }
            }
            r = dyn_rx.changed() => {
                if r.is_err() { return }
                if !dyn_rx.borrow().is_connected {
                    stop(&app, &mut current, false).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal() {
        let root = Path::new("/videos");
        assert!(resolve(root, "../etc/passwd").is_none());
        assert_eq!(resolve(root, "/movies/a.mkv").unwrap(), Path::new("/videos/movies/a.mkv"));
    }
}
