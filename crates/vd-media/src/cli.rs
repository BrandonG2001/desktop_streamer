//! Capture + encode pipeline driven by the `ffmpeg` command-line tool.
//!
//! One `ffmpeg` process per monitor captures, scales and encodes with the selected hardware
//! encoder and writes a raw elementary stream (`-f h264|hevc|obu pipe:1`).  A reader task splits
//! the stream into access units ([`vd_protocol::nal::AccessUnitSplitter`] /
//! [`crate::obu::TemporalUnitSplitter`]) and delivers [`EncodedFrame`]s on a bounded queue.
//!
//! Run-time control is limited compared with the original's direct NVENC API use: bitrate
//! changes and on-demand key frames restart the process (≈100–300 ms), and a finite GOP is used
//! to bound recovery time.  Build with the `ffmpeg-lib` feature for in-process control.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use vd_protocol::nal::{self, AccessUnitSplitter, Bitstream};

use crate::capture::CaptureSpec;
use crate::codec::EncoderConfig;
use crate::error::{MediaError, Result};
use crate::obu::TemporalUnitSplitter;
use crate::version::Ffmpeg;
use crate::{EncodedFrame, now_ticks};

/// Builds the complete argument list for a capture + encode process.
///
/// # Arguments
/// * `ff` - FFmpeg installation (version decides option spellings).
/// * `capture` - capture request.
/// * `enc` - encoder configuration.
///
/// # Returns
/// Arguments (without the executable).
pub fn build_args(ff: &Ffmpeg, capture: &CaptureSpec, enc: &EncoderConfig) -> Vec<String> {
    let cap = capture.ffmpeg_args(enc.backend, enc.pixel_format(), &enc.render_node);
    let gpu_frames = cap.gpu_frames;
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-probesize", "32", "-analyzeduration", "0"].iter().map(|s| s.to_string()).collect();
    args.extend(cap.input);
    if !cap.filter.is_empty() {
        args.extend(["-vf".to_string(), cap.filter]);
    }
    args.push("-an".to_string());
    args.extend(ff.version.passthrough_timing_args().iter().map(|s| s.to_string()));
    let mut enc_args = enc.encoder_args(ff.version);
    if gpu_frames {
        // GPU surfaces: let the hardware encoder convert BGRA → NV12/P010 itself.
        if let Some(i) = enc_args.iter().position(|a| a == "-pix_fmt") {
            enc_args.drain(i..(i + 2).min(enc_args.len()));
        }
    }
    args.extend(enc_args);
    args.extend(["-flush_packets".to_string(), "1".to_string(), "-f".to_string(), enc.output_format().to_string(), "pipe:1".to_string()]);
    args
}

/// A running capture + encode process.
pub struct CliVideoPipeline {
    child: Child,
    frames: mpsc::Receiver<EncodedFrame>,
    config: EncoderConfig,
}

impl CliVideoPipeline {
    /// Starts the pipeline.
    ///
    /// # Arguments
    /// * `ff` - FFmpeg installation.
    /// * `capture` - capture request.
    /// * `enc` - encoder configuration.
    ///
    /// # Returns
    /// The running pipeline.
    ///
    /// # Errors
    /// [`MediaError::Io`] / [`MediaError::FfmpegNotFound`] if the process cannot be spawned.
    pub fn start(ff: &Ffmpeg, capture: &CaptureSpec, enc: &EncoderConfig) -> Result<Self> {
        let args = build_args(ff, capture, enc);
        info!(cmd = %format!("{} {}", ff.ffmpeg.display(), args.join(" ")), "starting encoder pipeline");
        let mut child = Command::new(&ff.ffmpeg)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| MediaError::FfmpegNotFound(format!("{}: {e}", ff.ffmpeg.display())))?;
        let stdout = child.stdout.take().ok_or(MediaError::Stopped)?;
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    warn!(target: "ffmpeg", "{line}");
                }
            });
        }
        let (tx, rx) = mpsc::channel(4);
        let codec = enc.codec;
        let frame_ticks = 10_000_000 / i64::from(enc.framerate.max(1));
        tokio::spawn(async move {
            let mut stdout = stdout;
            let mut buf = vec![0u8; 256 * 1024];
            let bitstream = if codec.is_hevc() {
                Some(Bitstream::Hevc)
            } else if codec.is_av1() {
                None
            } else {
                Some(Bitstream::H264)
            };
            let mut nal_split = bitstream.map(AccessUnitSplitter::new);
            let mut obu_split = TemporalUnitSplitter::new();
            loop {
                let n = match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let arrival = now_ticks();
                // The capture time is not exposed by a raw elementary stream; one frame interval
                // of encoder latency is assumed (hardware encoders in ULL mode add < 1 frame).
                let capture_time = arrival - frame_ticks;
                let units: Vec<(Vec<u8>, bool)> = match (&mut nal_split, bitstream) {
                    (Some(split), Some(bs)) => split
                        .push(&buf[..n])
                        .into_iter()
                        .map(|au| {
                            let key = nal::split_annex_b(&au).iter().any(|n| nal::is_keyframe_nal(bs, n));
                            (au, key)
                        })
                        .collect(),
                    _ => obu_split.push(&buf[..n]),
                };
                for (data, keyframe) in units {
                    let frame = EncodedFrame { data, keyframe, capture_time, encode_latency_ms: frame_ticks as f32 / 10_000.0 };
                    if tx.send(frame).await.is_err() {
                        return;
                    }
                }
            }
            debug!("encoder output closed");
        });
        Ok(Self { child, frames: rx, config: enc.clone() })
    }

    /// Returns the encoder configuration.
    ///
    /// # Returns
    /// The configuration.
    pub fn config(&self) -> &EncoderConfig {
        &self.config
    }

    /// Waits for the next encoded frame.
    ///
    /// # Returns
    /// The frame, or `None` when the process has exited.
    pub async fn next_frame(&mut self) -> Option<EncodedFrame> {
        self.frames.recv().await
    }

    /// Returns a frame if one is ready, without waiting.
    ///
    /// # Returns
    /// The frame, if any.
    pub fn try_next_frame(&mut self) -> Option<EncodedFrame> {
        self.frames.try_recv().ok()
    }

    /// Reports whether the process is still running.
    ///
    /// # Returns
    /// `true` while running.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Stops the process.
    pub async fn stop(&mut self) {
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::CaptureSource;
    use crate::codec::Backend;
    use crate::version::FfmpegVersion;
    use vd_protocol::types::VideoCodec;

    #[test]
    fn command_line_shape() {
        let ff = Ffmpeg {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            version: FfmpegVersion { major: 7, minor: 1 },
            encoders: Default::default(),
            inputs: Default::default(),
        };
        let cap = CaptureSpec {
            source: CaptureSource::TestPattern { width: 640, height: 360 },
            framerate: 30,
            draw_cursor: false,
            output_size: (640, 360),
            source_size: Some((640, 360)),
        };
        let enc = EncoderConfig {
            backend: Backend::Software,
            codec: VideoCodec::H264,
            width: 640,
            height: 360,
            framerate: 30,
            bitrate: 2_000_000,
            enable_aq: false,
            yuv444: false,
            two_pass: false,
            gop: 60,
            render_node: String::new(),
        };
        let args = build_args(&ff, &cap, &enc);
        assert_eq!(args.last().unwrap(), "pipe:1");
        assert!(args.windows(2).any(|w| w == ["-f", "h264"]));
    }
}
