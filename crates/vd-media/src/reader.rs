//! Video-file playback for the "Video" streaming source.  C#: `VirtualDesktop.FFmpeg.MediaReader`
//! driven by `VirtualDesktop.Streamer.VideoStreamer`.
//!
//! The original never re-encodes H.264/HEVC files: it demuxes packets, converts them to Annex-B
//! with the `h264_mp4toannexb` / `hevc_mp4toannexb` bitstream filters, and sends them on the video
//! channel with a `StreamingSource.Video` frame header so the headset decodes them directly.
//! Audio is decoded and resampled to 48 kHz stereo s16 (`aresample=ochl=stereo:osf=s16:osr=…`,
//! plus `atempo=` for playback speed) and sent on the audio channel.
//!
//! This port runs two `ffmpeg` processes started at the same position — one copying video,
//! one producing PCM — both paced in real time with `-readrate <speed>`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::{info, warn};
use vd_protocol::nal::{self, AccessUnitSplitter, Bitstream};

use crate::error::{MediaError, Result};
use crate::probe::MediaInfo;
use crate::version::Ffmpeg;

/// A demuxed packet ready for the network.
#[derive(Debug, Clone)]
pub enum MediaPacket {
    /// One Annex-B access unit.
    Video {
        /// Bitstream.
        data: Vec<u8>,
        /// Presentation time in 100 ns ticks (already divided by the playback speed).
        pts: i64,
        /// Key frame.
        keyframe: bool,
    },
    /// 48 kHz stereo s16le PCM (1152-byte chunks, matching the desktop audio packets).
    Audio(Vec<u8>),
    /// End of file reached.
    EndOfStream,
}

/// Playback request.
#[derive(Debug, Clone)]
pub struct PlaybackRequest {
    /// File to play.
    pub path: PathBuf,
    /// Start position in 100 ns ticks.
    pub start: i64,
    /// Index among the file's audio streams (`None` = first; −1 from the headset maps to `None`).
    pub audio_stream: Option<u32>,
    /// Playback speed (`SharedMobileSettings.VideoPlaybackSpeed`).
    pub speed: f32,
}

/// Running playback.
pub struct VideoFilePlayback {
    children: Vec<Child>,
    packets: mpsc::Receiver<MediaPacket>,
}

fn seconds(ticks: i64) -> String {
    format!("{:.3}", ticks.max(0) as f64 / 1e7)
}

impl VideoFilePlayback {
    /// Starts playback.
    ///
    /// # Arguments
    /// * `ff` - FFmpeg installation.
    /// * `info` - result of [`crate::probe::probe`] for the file.
    /// * `req` - playback request.
    ///
    /// # Returns
    /// The running playback.
    ///
    /// # Errors
    /// [`MediaError::Probe`] when the file has no H.264/HEVC video (the headset only decodes those
    /// in pass-through mode), or spawn errors.
    pub fn start(ff: &Ffmpeg, info: &MediaInfo, req: &PlaybackRequest) -> Result<Self> {
        let video = info.video().ok_or_else(|| MediaError::Probe("no video stream".into()))?;
        let (bitstream, bsf, fmt) = match video.codec_name.as_str() {
            "h264" => (Bitstream::H264, "h264_mp4toannexb", "h264"),
            "hevc" => (Bitstream::Hevc, "hevc_mp4toannexb", "hevc"),
            other => return Err(MediaError::Probe(format!("unsupported pass-through codec {other}"))),
        };
        let fps = video.fps().unwrap_or(30.0);
        let speed = if req.speed > 0.0 { req.speed } else { 1.0 };
        let pacing: Vec<String> = if ff.version.has_readrate() { vec!["-readrate".into(), format!("{speed}")] } else { vec!["-re".into()] };
        let common = |cmd: &mut Command| {
            cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"]).args(&pacing).args(["-ss", &seconds(req.start)]).arg("-i").arg(&req.path);
            cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        };
        let (tx, rx) = mpsc::channel(32);

        let mut vcmd = Command::new(&ff.ffmpeg);
        common(&mut vcmd);
        vcmd.args(["-map", "0:v:0", "-an", "-sn", "-c:v", "copy", "-bsf:v", bsf, "-f", fmt, "pipe:1"]);
        let mut vchild = vcmd.spawn()?;
        let mut vout = vchild.stdout.take().ok_or(MediaError::Stopped)?;
        let vtx = tx.clone();
        let start = req.start;
        tokio::spawn(async move {
            let mut split = AccessUnitSplitter::new(bitstream);
            let mut buf = vec![0u8; 512 * 1024];
            let mut frame_no: i64 = 0;
            let frame_ticks = 1e7 / fps / f64::from(speed);
            let start_scaled = (start as f64 / f64::from(speed)) as i64;
            loop {
                let n = match vout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                for au in split.push(&buf[..n]) {
                    let keyframe = nal::split_annex_b(&au).iter().any(|u| nal::is_keyframe_nal(bitstream, u));
                    let pts = start_scaled + (frame_no as f64 * frame_ticks) as i64;
                    frame_no += 1;
                    if vtx.send(MediaPacket::Video { data: au, pts, keyframe }).await.is_err() {
                        return;
                    }
                }
            }
            if let Some(au) = split.flush() {
                let _ = vtx.send(MediaPacket::Video { data: au, pts: start_scaled + (frame_no as f64 * frame_ticks) as i64, keyframe: false }).await;
            }
            let _ = vtx.send(MediaPacket::EndOfStream).await;
        });
        let mut children = vec![vchild];

        if !info.audio().is_empty() {
            let idx = req.audio_stream.unwrap_or(0).min(info.audio().len() as u32 - 1);
            let mut af = "aresample=48000".to_string();
            if (speed - 1.0).abs() > f32::EPSILON {
                af.push_str(&format!(",atempo={speed:.1}"));
            }
            let mut acmd = Command::new(&ff.ffmpeg);
            common(&mut acmd);
            acmd.args(["-map", &format!("0:a:{idx}"), "-vn", "-sn", "-af", &af, "-ac", "2", "-ar", "48000", "-f", "s16le", "pipe:1"]);
            match acmd.spawn() {
                Ok(mut achild) => {
                    if let Some(mut aout) = achild.stdout.take() {
                        let atx = tx.clone();
                        tokio::spawn(async move {
                            let mut chunk = vec![0u8; vd_protocol::channels::audio::SPEAKER_PACKET_BYTES];
                            while aout.read_exact(&mut chunk).await.is_ok() {
                                if atx.send(MediaPacket::Audio(chunk.clone())).await.is_err() {
                                    return;
                                }
                            }
                        });
                    }
                    children.push(achild);
                }
                Err(e) => warn!(error = %e, "audio decoder failed to start; playing without sound"),
            }
        }
        info!(path = %req.path.display(), codec = fmt, fps, speed, "video playback started");
        Ok(Self { children, packets: rx })
    }

    /// Waits for the next packet.
    ///
    /// # Returns
    /// The packet, or `None` after both processes ended and every packet was consumed.
    pub async fn next(&mut self) -> Option<MediaPacket> {
        self.packets.recv().await
    }

    /// Stops playback.
    pub async fn stop(&mut self) {
        for c in &mut self.children {
            let _ = c.start_kill();
        }
        for c in &mut self.children {
            let _ = c.wait().await;
        }
    }
}

/// File extensions listed in the video library (C#: the extension table of `VideoStreamer`).
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "3gp", "aaf", "asf", "avchd", "avi", "flv", "hevc", "iso", "m1v", "m2ts", "m2v", "m4v", "mjpeg", "mkv", "mov", "mp2", "mp4", "mpe", "mpeg", "mpg", "mxf", "rm",
    "swf", "ts", "vob", "webm", "wmv", "wtv",
];

/// Whether a path has a video extension.
///
/// # Arguments
/// * `path` - file path.
///
/// # Returns
/// `true` for known video extensions (case-insensitive).
pub fn is_video_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| VIDEO_EXTENSIONS.iter().any(|v| v.eq_ignore_ascii_case(e)))
}
