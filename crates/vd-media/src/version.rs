//! FFmpeg discovery and capability probing.
//!
//! This is one of the two modules to touch when moving to a new FFmpeg release (the other is
//! [`crate::codec`]).  It locates the executable, parses its version, lists encoders/devices,
//! and performs the same tiny test encode the original used to detect working hardware
//! encoders (C#: `MediaReader` strings `-f lavfi -i color=black:s=256x256 -vframes 1 -an -c:v
//! <encoder> … -f null -`).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Stdio;

use tracing::debug;

use crate::error::{MediaError, Result};

/// Parsed `ffmpeg -version` number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FfmpegVersion {
    /// Major version (e.g. 7 for FFmpeg 7.1).
    pub major: u32,
    /// Minor version.
    pub minor: u32,
}

impl FfmpegVersion {
    /// Parses the first line of `ffmpeg -version` (`ffmpeg version 7.1-…`, `ffmpeg version n6.0`,
    /// or a git snapshot `N-112345-g…`, which is treated as a very new release).
    ///
    /// # Arguments
    /// * `banner` - `ffmpeg -version` output.
    ///
    /// # Returns
    /// The version, or `None` if it cannot be recognised.
    pub fn parse(banner: &str) -> Option<Self> {
        let line = banner.lines().next()?;
        let token = line.split_whitespace().nth(2)?;
        if token.starts_with('N') {
            return Some(Self { major: 99, minor: 0 });
        }
        let digits = token.trim_start_matches(|c: char| !c.is_ascii_digit());
        let mut it = digits.split(|c: char| !c.is_ascii_digit());
        let major = it.next()?.parse().ok()?;
        let minor = it.next().and_then(|m| m.parse().ok()).unwrap_or(0);
        Some(Self { major, minor })
    }

    /// Whether the `-readrate` input option exists (FFmpeg ≥ 5.0).
    ///
    /// # Returns
    /// `true` for 5.0 and newer.
    pub fn has_readrate(self) -> bool {
        self.major >= 5
    }

    /// Whether NVENC uses the P1–P7 preset/tune scheme (FFmpeg ≥ 4.3); older builds only know
    /// the legacy `llhp`/`ll` presets.
    ///
    /// # Returns
    /// `true` for 4.3 and newer.
    pub fn nvenc_p_presets(self) -> bool {
        self.major > 4 || (self.major == 4 && self.minor >= 3)
    }

    /// Option that disables frame duplication/dropping: `-fps_mode` since FFmpeg 5.1, `-vsync`
    /// before.
    ///
    /// # Returns
    /// `["-fps_mode", "passthrough"]` or `["-vsync", "passthrough"]`.
    pub fn passthrough_timing_args(self) -> [&'static str; 2] {
        if self.major > 5 || (self.major == 5 && self.minor >= 1) {
            ["-fps_mode", "passthrough"]
        } else {
            ["-vsync", "passthrough"]
        }
    }

    /// Whether the `obu` muxer (raw AV1 output) is available (FFmpeg ≥ 4.4).
    ///
    /// # Returns
    /// `true` for 4.4 and newer.
    pub fn has_obu_muxer(self) -> bool {
        self.major > 4 || (self.major == 4 && self.minor >= 4)
    }
}

/// Location and capabilities of the FFmpeg installation.
#[derive(Debug, Clone)]
pub struct Ffmpeg {
    /// `ffmpeg` executable.
    pub ffmpeg: PathBuf,
    /// `ffprobe` executable.
    pub ffprobe: PathBuf,
    /// Parsed version.
    pub version: FfmpegVersion,
    /// Names of every encoder listed by `-encoders`.
    pub encoders: BTreeSet<String>,
    /// Names of every input device / source filter of interest (`x11grab`, `kmsgrab`, `ddagrab` …).
    pub inputs: BTreeSet<String>,
}

/// Returns the candidate executable path for `tool`.
///
/// Order: `$VD_FFMPEG_DIR/<tool>`, next to this executable (also `ffmpeg/` and `ffmpeg/bin/`
/// below it), then the bare name (resolved through `PATH`).
///
/// # Arguments
/// * `tool` - `"ffmpeg"` or `"ffprobe"`.
///
/// # Returns
/// The path to try.
pub fn tool_path(tool: &str) -> PathBuf {
    let exe = if cfg!(windows) { format!("{tool}.exe") } else { tool.to_owned() };
    if let Some(dir) = std::env::var_os("VD_FFMPEG_DIR") {
        return PathBuf::from(dir).join(exe);
    }
    // A copy shipped next to the program (or in its `ffmpeg` folder) wins over `PATH`.
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)) {
        for candidate in [dir.join(&exe), dir.join("ffmpeg").join(&exe), dir.join("ffmpeg").join("bin").join(&exe)] {
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from(exe)
}

async fn run_capture(path: &PathBuf, args: &[&str]) -> Result<String> {
    let out = crate::command(path)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| MediaError::FfmpegNotFound(format!("{}: {e}", path.display())))?;
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(s)
}

impl Ffmpeg {
    /// Locates FFmpeg and lists its encoders and capture inputs.
    ///
    /// # Returns
    /// The installation description.
    ///
    /// # Errors
    /// [`MediaError::FfmpegNotFound`] if `ffmpeg` cannot be executed or its version is unknown.
    pub async fn discover() -> Result<Self> {
        let ffmpeg = tool_path("ffmpeg");
        let ffprobe = tool_path("ffprobe");
        let banner = run_capture(&ffmpeg, &["-hide_banner", "-version"]).await?;
        let version = FfmpegVersion::parse(&banner).ok_or_else(|| MediaError::FfmpegNotFound(format!("unrecognised version banner: {}", banner.lines().next().unwrap_or(""))))?;
        let enc_list = run_capture(&ffmpeg, &["-hide_banner", "-encoders"]).await?;
        let encoders = enc_list
            .lines()
            .filter_map(|l| {
                let mut parts = l.split_whitespace();
                let flags = parts.next()?;
                (flags.len() == 6 && flags.starts_with('V')).then(|| parts.next().map(str::to_owned)).flatten()
            })
            .collect();
        let dev_list = run_capture(&ffmpeg, &["-hide_banner", "-devices"]).await?;
        let filt_list = run_capture(&ffmpeg, &["-hide_banner", "-filters"]).await?;
        let mut inputs = BTreeSet::new();
        for name in ["x11grab", "kmsgrab", "gdigrab", "dshow", "avfoundation", "pulse", "alsa", "lavfi", "v4l2"] {
            if dev_list.split_whitespace().any(|w| w == name) {
                inputs.insert(name.to_owned());
            }
        }
        for name in ["ddagrab", "hwupload", "hwmap", "scale_vaapi", "scale_cuda", "zscale", "tonemap"] {
            if filt_list.split_whitespace().any(|w| w == name) {
                inputs.insert(name.to_owned());
            }
        }
        debug!(?version, encoders = ?encoders, inputs = ?inputs, "ffmpeg discovered");
        Ok(Self { ffmpeg, ffprobe, version, encoders, inputs })
    }

    /// Whether an encoder is compiled in.
    ///
    /// # Arguments
    /// * `name` - encoder name (e.g. `hevc_nvenc`).
    ///
    /// # Returns
    /// `true` if listed by `-encoders`.
    pub fn has_encoder(&self, name: &str) -> bool {
        self.encoders.contains(name)
    }

    /// Whether an input device or filter is available.
    ///
    /// # Arguments
    /// * `name` - device/filter name.
    ///
    /// # Returns
    /// `true` if available.
    pub fn has_input(&self, name: &str) -> bool {
        self.inputs.contains(name)
    }

    /// Checks that an encoder actually works on this machine (driver and GPU present) with a
    /// one-frame test encode, exactly like the original's probe command line.
    ///
    /// # Arguments
    /// * `encoder` - encoder name.
    /// * `pre_input` - extra arguments needed before `-i` (e.g. `-vaapi_device …`).
    /// * `filter` - optional `-vf` chain (e.g. `format=nv12,hwupload` for VAAPI).
    ///
    /// # Returns
    /// `true` if FFmpeg exits successfully.
    pub async fn test_encoder(&self, encoder: &str, pre_input: &[String], filter: Option<&str>) -> bool {
        if !self.has_encoder(encoder) {
            return false;
        }
        let mut cmd = crate::command(&self.ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error"]).args(pre_input);
        cmd.args(["-f", "lavfi", "-i", "color=black:s=256x256", "-vframes", "1", "-an"]);
        if let Some(f) = filter {
            cmd.args(["-vf", f]);
        }
        cmd.args(["-c:v", encoder]);
        if encoder.ends_with("_videotoolbox") {
            cmd.args(["-allow_sw", "1"]);
        }
        cmd.args(["-f", "null", "-"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
        matches!(cmd.status().await, Ok(s) if s.success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(FfmpegVersion::parse("ffmpeg version 7.1-full_build Copyright"), Some(FfmpegVersion { major: 7, minor: 1 }));
        assert_eq!(FfmpegVersion::parse("ffmpeg version n6.0 Copyright"), Some(FfmpegVersion { major: 6, minor: 0 }));
        assert_eq!(FfmpegVersion::parse("ffmpeg version 8.0.1-essentials_build-www.gyan.dev"), Some(FfmpegVersion { major: 8, minor: 0 }));
        assert_eq!(FfmpegVersion::parse("ffmpeg version N-112233-gabc").unwrap().major, 99);
    }
}
