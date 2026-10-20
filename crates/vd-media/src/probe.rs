//! Media file probing with `ffprobe` (replaces `avformat_find_stream_info` calls of
//! `VirtualDesktop.FFmpeg.MediaReader`).

use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;

use crate::error::{MediaError, Result};
use crate::version::Ffmpeg;

/// One stream of a media file.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct StreamInfo {
    /// Absolute stream index.
    pub index: u32,
    /// `video`, `audio`, `subtitle`, …
    pub codec_type: String,
    /// Codec name (`h264`, `hevc`, `aac`, …).
    #[serde(default)]
    pub codec_name: String,
    /// Video width.
    #[serde(default)]
    pub width: Option<u32>,
    /// Video height.
    #[serde(default)]
    pub height: Option<u32>,
    /// Average frame rate as `num/den`.
    #[serde(default)]
    pub avg_frame_rate: Option<String>,
    /// Audio channel count.
    #[serde(default)]
    pub channels: Option<u32>,
    /// Transfer characteristic (`smpte2084`, `arib-std-b67` → HDR).
    #[serde(default)]
    pub color_transfer: Option<String>,
    /// Stream tags (`language`, `title`).
    #[serde(default)]
    pub tags: std::collections::BTreeMap<String, String>,
}

impl StreamInfo {
    /// Frame rate in frames per second.
    ///
    /// # Returns
    /// The rate, or `None` when unknown / zero.
    pub fn fps(&self) -> Option<f64> {
        let r = self.avg_frame_rate.as_deref()?;
        let (n, d) = r.split_once('/')?;
        let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
        (d > 0.0 && n > 0.0).then_some(n / d)
    }

    /// Whether the video uses an HDR transfer function (needs tone mapping for thumbnails).
    ///
    /// # Returns
    /// `true` for PQ or HLG.
    pub fn is_hdr(&self) -> bool {
        matches!(self.color_transfer.as_deref(), Some("smpte2084" | "arib-std-b67"))
    }

    /// Track title for the UI (`title`, else `language`, else codec).
    ///
    /// # Returns
    /// Display name.
    pub fn display_name(&self) -> String {
        self.tags.get("title").or_else(|| self.tags.get("language")).cloned().unwrap_or_else(|| self.codec_name.clone())
    }
}

/// Container-level information.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct FormatInfo {
    /// Duration in seconds.
    #[serde(default)]
    pub duration: Option<String>,
}

/// Result of probing a file.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct MediaInfo {
    /// All streams.
    #[serde(default)]
    pub streams: Vec<StreamInfo>,
    /// Container information.
    #[serde(default)]
    pub format: FormatInfo,
}

impl MediaInfo {
    /// First video stream.
    ///
    /// # Returns
    /// The stream, if any.
    pub fn video(&self) -> Option<&StreamInfo> {
        self.streams.iter().find(|s| s.codec_type == "video")
    }

    /// All audio streams in file order.
    ///
    /// # Returns
    /// Audio streams.
    pub fn audio(&self) -> Vec<&StreamInfo> {
        self.streams.iter().filter(|s| s.codec_type == "audio").collect()
    }

    /// All subtitle streams in file order.
    ///
    /// # Returns
    /// Subtitle streams.
    pub fn subtitles(&self) -> Vec<&StreamInfo> {
        self.streams.iter().filter(|s| s.codec_type == "subtitle").collect()
    }

    /// Duration in 100 ns ticks.
    ///
    /// # Returns
    /// Ticks, or 0 if unknown.
    pub fn duration_ticks(&self) -> i64 {
        self.format.duration.as_deref().and_then(|d| d.parse::<f64>().ok()).map(|s| (s * 1e7) as i64).unwrap_or(0)
    }
}

/// Runs `ffprobe` on `path`.  C#: "Cannot open input file" / "Cannot find stream information".
///
/// # Arguments
/// * `ff` - FFmpeg installation.
/// * `path` - media file.
///
/// # Returns
/// The parsed information.
///
/// # Errors
/// [`MediaError::Probe`] if ffprobe fails, [`MediaError::Json`] if its output is malformed.
pub async fn probe(ff: &Ffmpeg, path: &Path) -> Result<MediaInfo> {
    let out = crate::command(&ff.ffprobe)
        .args(["-v", "error", "-show_streams", "-show_format", "-of", "json"])
        .arg(path)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    if !out.status.success() {
        return Err(MediaError::Probe(String::from_utf8_lossy(&out.stderr).trim().to_owned()));
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffprobe_json() {
        let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"hevc","width":3840,"height":2160,"avg_frame_rate":"24000/1001","color_transfer":"smpte2084"},{"index":1,"codec_type":"audio","codec_name":"eac3","channels":6,"tags":{"language":"eng"}}],"format":{"duration":"120.5"}}"#;
        let info: MediaInfo = serde_json::from_str(json).unwrap();
        assert!(info.video().unwrap().is_hdr());
        assert!((info.video().unwrap().fps().unwrap() - 23.976).abs() < 0.01);
        assert_eq!(info.audio()[0].display_name(), "eng");
        assert_eq!(info.duration_ticks(), 1_205_000_000);
    }
}
