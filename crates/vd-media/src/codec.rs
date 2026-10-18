//! Encoder selection and per-backend low-latency settings.
//!
//! Replaces the encoder factory `StreamerManager._0001(in AdapterDescription, Size2, VideoCodec,
//! bool)` (NVIDIA → NVENC, AMD → AMF, otherwise Media Foundation) and the codec fallback rules
//! of `_008B._0003._0001(VideoCodec, bool)`.  The NVENC configuration of the original —
//! infinite GOP with forced IDR on demand, no B-frames, constant bitrate, ultra-low-latency
//! tuning, optional spatial AQ / 4:4:4 / two-pass, 10-bit for HEVC10 / AV1-10 — is translated
//! into the equivalent FFmpeg private options of each hardware encoder.

use vd_protocol::types::{Features, HmdType, VideoCodec};

use crate::version::{Ffmpeg, FfmpegVersion};

/// Hardware (or software) encoder family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// NVIDIA NVENC (`*_nvenc`) — Windows and Linux.
    Nvenc,
    /// AMD AMF (`*_amf`) — Windows, and Linux with the AMD Pro stack.
    Amf,
    /// Intel Quick Sync (`*_qsv`) — Windows and Linux.
    Qsv,
    /// VA-API (`*_vaapi`) — Linux (Mesa AMD/Intel).
    Vaapi,
    /// Apple VideoToolbox (`*_videotoolbox`).
    VideoToolbox,
    /// Media Foundation MFTs (`*_mf`) — Windows fallback, as `_001B_0002._0002`.
    MediaFoundation,
    /// V4L2 memory-to-memory (`*_v4l2m2m`) — ARM SoCs such as Raspberry Pi.
    V4l2m2m,
    /// libx264 / libx265 / libsvtav1 software encoding (last resort).
    Software,
}

impl Backend {
    /// Preference order used when auto-detecting.
    pub const PREFERENCE: [Backend; 8] = [
        Backend::Nvenc,
        Backend::Amf,
        Backend::Qsv,
        Backend::Vaapi,
        Backend::VideoToolbox,
        Backend::MediaFoundation,
        Backend::V4l2m2m,
        Backend::Software,
    ];

    /// FFmpeg encoder name for a codec family.
    ///
    /// # Arguments
    /// * `codec` - requested codec (10-bit variants map to the same encoder).
    ///
    /// # Returns
    /// The encoder name, or `None` if the backend has no encoder for the codec.
    pub fn encoder_name(self, codec: VideoCodec) -> Option<&'static str> {
        let family = if codec.is_hevc() {
            1
        } else if codec.is_av1() {
            2
        } else {
            0
        };
        let names: [Option<&'static str>; 3] = match self {
            Backend::Nvenc => [Some("h264_nvenc"), Some("hevc_nvenc"), Some("av1_nvenc")],
            Backend::Amf => [Some("h264_amf"), Some("hevc_amf"), Some("av1_amf")],
            Backend::Qsv => [Some("h264_qsv"), Some("hevc_qsv"), Some("av1_qsv")],
            Backend::Vaapi => [Some("h264_vaapi"), Some("hevc_vaapi"), Some("av1_vaapi")],
            Backend::VideoToolbox => [Some("h264_videotoolbox"), Some("hevc_videotoolbox"), None],
            Backend::MediaFoundation => [Some("h264_mf"), Some("hevc_mf"), Some("av1_mf")],
            Backend::V4l2m2m => [Some("h264_v4l2m2m"), Some("hevc_v4l2m2m"), None],
            Backend::Software => [Some("libx264"), Some("libx265"), Some("libsvtav1")],
        };
        names[family]
    }

    /// Whether the backend is hardware accelerated (the original's
    /// `DynamicSettings.HasHardwareEncoder`, which gates multi-monitor streaming).
    ///
    /// # Returns
    /// `false` only for [`Backend::Software`].
    pub fn is_hardware(self) -> bool {
        self != Backend::Software
    }
}

/// Codec support of the selected backend on this machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EncoderCaps {
    /// H.264 works.
    pub h264: bool,
    /// HEVC 8-bit works.
    pub hevc: bool,
    /// HEVC Main10 works.
    pub hevc_10bit: bool,
    /// AV1 works.
    pub av1: bool,
    /// AV1 10-bit works.
    pub av1_10bit: bool,
    /// 4:4:4 chroma supported (`DynamicSettings.SupportsYuv444`).
    pub yuv444: bool,
    /// Two-pass / multipass supported (`DynamicSettings.Supports2Pass`).
    pub two_pass: bool,
}

/// Extra FFmpeg arguments needed *before* `-i` for a backend's test encode and filter chain.
///
/// # Arguments
/// * `backend` - encoder backend.
/// * `render_node` - DRM render node for VA-API (e.g. `/dev/dri/renderD128`).
///
/// # Returns
/// `(pre-input arguments, test filter chain)`.
pub fn backend_device_args(backend: Backend, render_node: &str) -> (Vec<String>, Option<&'static str>) {
    match backend {
        Backend::Vaapi => (vec!["-vaapi_device".into(), render_node.into()], Some("format=nv12,hwupload")),
        Backend::Qsv if cfg!(target_os = "linux") => (vec!["-init_hw_device".into(), format!("qsv=hw:{render_node}")], Some("format=nv12,hwupload=extra_hw_frames=16")),
        _ => (Vec::new(), None),
    }
}

/// Probes which codecs a backend can actually encode.
///
/// # Arguments
/// * `ff` - FFmpeg installation.
/// * `backend` - backend to probe.
/// * `render_node` - DRM render node for VA-API / QSV on Linux.
///
/// # Returns
/// The capability set (all `false` if the backend is unusable).
pub async fn probe_caps(ff: &Ffmpeg, backend: Backend, render_node: &str) -> EncoderCaps {
    let (pre, filter) = backend_device_args(backend, render_node);
    let mut caps = EncoderCaps::default();
    if let Some(n) = backend.encoder_name(VideoCodec::H264) {
        caps.h264 = ff.test_encoder(n, &pre, filter).await;
    }
    if let Some(n) = backend.encoder_name(VideoCodec::HEVC) {
        caps.hevc = ff.test_encoder(n, &pre, filter).await;
        // Main10 support is assumed on GPUs that encode HEVC with these backends (all NVENC
        // Pascal+, AMF VCN, QSV Kaby Lake+, VAAPI Mesa radeonsi/iHD expose it).
        caps.hevc_10bit = caps.hevc && matches!(backend, Backend::Nvenc | Backend::Amf | Backend::Qsv | Backend::Vaapi | Backend::Software);
    }
    if let Some(n) = backend.encoder_name(VideoCodec::AV1) {
        caps.av1 = ff.test_encoder(n, &pre, filter).await;
        caps.av1_10bit = caps.av1;
    }
    caps.yuv444 = backend == Backend::Nvenc && caps.h264;
    caps.two_pass = backend == Backend::Nvenc && caps.h264;
    caps
}

/// Picks the first backend whose H.264 encoder works.
///
/// # Arguments
/// * `ff` - FFmpeg installation.
/// * `render_node` - DRM render node for VA-API / QSV on Linux.
/// * `allow_software` - fall back to libx264 when no GPU encoder works.
///
/// # Returns
/// `(backend, caps)` or `None` if nothing works.
pub async fn detect_backend(ff: &Ffmpeg, render_node: &str, allow_software: bool) -> Option<(Backend, EncoderCaps)> {
    for backend in Backend::PREFERENCE {
        if backend == Backend::Software && !allow_software {
            continue;
        }
        if cfg!(not(windows)) && backend == Backend::MediaFoundation {
            continue;
        }
        if cfg!(windows) && matches!(backend, Backend::Vaapi | Backend::V4l2m2m) {
            continue;
        }
        let caps = probe_caps(ff, backend, render_node).await;
        if caps.h264 {
            return Some((backend, caps));
        }
    }
    None
}

/// Resolves the codec actually used, given the user's preference, the headset and the encoder.
///
/// Port of the selection at the top of `_008B._0003._0001(VideoCodec, bool)`:
///
/// * `Automatic` → HEVC 10-bit when the headset is newer than the original Quest and decodes HEVC
///   and the encoder supports it, otherwise H.264;
/// * AV1 requests fall back to HEVC (10-bit) when the headset or the encoder lacks AV1;
/// * HEVC 10-bit falls back to HEVC, and HEVC to H.264, when unsupported.
///
/// # Arguments
/// * `requested` - `StreamerSettings.PreferredCodec`.
/// * `hmd` - `SharedMobileSettings.HmdType`.
/// * `features` - `SharedMobileSettings.Features`.
/// * `caps` - encoder capabilities.
///
/// # Returns
/// The codec to encode with.
pub fn resolve_codec(requested: VideoCodec, hmd: HmdType, features: Features, caps: &EncoderCaps) -> VideoCodec {
    let headset_hevc = hmd >= HmdType::QUEST || features.contains(Features::HEVC_DECODE);
    let mut codec = requested;
    if codec == VideoCodec::Automatic {
        return if hmd != HmdType::QUEST && headset_hevc && caps.hevc_10bit { VideoCodec::HEVC10bit } else { VideoCodec::H264 };
    }
    let headset_av1 = hmd >= HmdType::QUEST3 || features.contains(Features::AV1_DECODE);
    if codec == VideoCodec::AV110bit && headset_hevc && (!headset_av1 || !caps.av1_10bit) {
        codec = VideoCodec::HEVC10bit;
    } else if codec == VideoCodec::AV1 && headset_hevc && (!headset_av1 || !caps.av1) {
        codec = VideoCodec::HEVC;
    }
    if codec == VideoCodec::HEVC10bit && headset_hevc && !caps.hevc_10bit {
        codec = VideoCodec::HEVC;
    }
    if codec == VideoCodec::HEVC && (!headset_hevc || !caps.hevc) {
        codec = VideoCodec::H264;
    }
    if matches!(codec, VideoCodec::VP8 | VideoCodec::VP9) || (codec.is_h264() && !caps.h264) {
        codec = VideoCodec::H264;
    }
    codec
}

/// Encoder configuration of one monitor stream.
#[derive(Debug, Clone, PartialEq)]
pub struct EncoderConfig {
    /// Backend.
    pub backend: Backend,
    /// Resolved codec.
    pub codec: VideoCodec,
    /// Encoded width (even).
    pub width: u32,
    /// Encoded height (even).
    pub height: u32,
    /// Frames per second.
    pub framerate: u32,
    /// Target bitrate in bit/s.
    pub bitrate: u32,
    /// Spatial adaptive quantisation (`StreamerSettings.EnableAQ`).
    pub enable_aq: bool,
    /// 4:4:4 chroma (`StreamerSettings.EnableYuv444` && headset feature).
    pub yuv444: bool,
    /// Two-pass rate control (`StreamerSettings.Enable2Pass`).
    pub two_pass: bool,
    /// GOP length.  The original uses an infinite GOP and forces IDR frames on demand; the CLI
    /// backend cannot force key frames at run time, so it uses a finite GOP (2 s by default) to
    /// bound recovery after packet loss.
    pub gop: u32,
    /// VA-API / QSV render node (Linux).
    pub render_node: String,
}

impl EncoderConfig {
    /// FFmpeg output pixel format.
    ///
    /// # Returns
    /// `p010le` for 10-bit, `yuv444p` for 4:4:4, `nv12` otherwise (VA-API uses hardware surfaces).
    pub fn pixel_format(&self) -> &'static str {
        if self.codec.is_10bit() {
            "p010le"
        } else if self.yuv444 {
            "yuv444p"
        } else {
            "nv12"
        }
    }

    /// Raw elementary-stream muxer name for the codec.
    ///
    /// # Returns
    /// `h264`, `hevc` or `obu`.
    pub fn output_format(&self) -> &'static str {
        if self.codec.is_hevc() {
            "hevc"
        } else if self.codec.is_av1() {
            "obu"
        } else {
            "h264"
        }
    }

    /// Builds the encoder part of the command line (`-c:v …` and private options).
    ///
    /// # Arguments
    /// * `version` - FFmpeg version, used to choose option spellings.
    ///
    /// # Returns
    /// The argument list.
    pub fn encoder_args(&self, version: FfmpegVersion) -> Vec<String> {
        let enc = self.backend.encoder_name(self.codec).unwrap_or("libx264");
        let b = self.bitrate.to_string();
        // One-frame VBV like the original (vbvBufferSize = averageBitRate / fps).
        let vbv = (self.bitrate / self.framerate.max(1)).max(1).to_string();
        let gop = self.gop.to_string();
        let mut a: Vec<String> = vec!["-c:v".into(), enc.into(), "-g".into(), gop, "-bf".into(), "0".into(), "-b:v".into(), b.clone(), "-maxrate".into(), b, "-bufsize".into(), vbv];
        let mut push = |kv: &[&str]| a.extend(kv.iter().map(|s| s.to_string()));
        match self.backend {
            Backend::Nvenc => {
                if version.nvenc_p_presets() {
                    push(&["-preset", "p1", "-tune", "ull"]);
                } else {
                    push(&["-preset", "llhp"]);
                }
                push(&["-rc", "cbr", "-zerolatency", "1", "-delay", "0", "-forced-idr", "1", "-strict_gop", "1"]);
                if self.enable_aq {
                    push(&["-spatial-aq", "1"]);
                }
                if self.two_pass && version.nvenc_p_presets() {
                    push(&["-multipass", "qres"]);
                }
                if self.codec.is_h264() {
                    push(&["-profile:v", if self.yuv444 { "high444p" } else { "high" }]);
                } else if self.codec == VideoCodec::HEVC10bit {
                    push(&["-profile:v", "main10"]);
                }
            }
            Backend::Amf => {
                push(&["-usage", "ultralowlatency", "-quality", "speed", "-rc", "cbr", "-enforce_hrd", "1"]);
                if self.codec.is_hevc() {
                    push(&["-header_insertion_mode", "idr"]);
                }
                if self.codec == VideoCodec::HEVC10bit {
                    push(&["-profile:v", "main10"]);
                }
            }
            Backend::Qsv => {
                push(&["-preset", "veryfast", "-look_ahead", "0", "-async_depth", "1", "-low_power", "1"]);
                if self.codec == VideoCodec::HEVC10bit {
                    push(&["-profile:v", "main10"]);
                }
            }
            Backend::Vaapi => {
                push(&["-rc_mode", "CBR", "-async_depth", "1"]);
                if self.codec == VideoCodec::HEVC10bit {
                    push(&["-profile:v", "main10"]);
                }
            }
            Backend::VideoToolbox => push(&["-realtime", "1", "-allow_sw", "1", "-prio_speed", "1"]),
            Backend::MediaFoundation => push(&["-hw_encoding", "1", "-scenario", "display_remoting", "-rate_control", "cbr"]),
            Backend::V4l2m2m => {}
            Backend::Software => {
                if self.codec.is_av1() {
                    push(&["-preset", "12", "-svtav1-params", "tune=0:fast-decode=1:lookahead=0"]);
                } else {
                    push(&["-preset", "ultrafast", "-tune", "zerolatency"]);
                }
            }
        }
        if !matches!(self.backend, Backend::Vaapi) {
            a.extend(["-pix_fmt".to_string(), self.pixel_format().to_string()]);
        }
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: EncoderCaps = EncoderCaps { h264: true, hevc: true, hevc_10bit: true, av1: true, av1_10bit: true, yuv444: true, two_pass: true };

    #[test]
    fn automatic_prefers_hevc10_on_quest3() {
        assert_eq!(resolve_codec(VideoCodec::Automatic, HmdType::QUEST3, Features::default(), &ALL), VideoCodec::HEVC10bit);
        assert_eq!(resolve_codec(VideoCodec::Automatic, HmdType::QUEST, Features::default(), &ALL), VideoCodec::H264);
    }

    #[test]
    fn av1_falls_back_on_quest2() {
        assert_eq!(resolve_codec(VideoCodec::AV1, HmdType::QUEST2, Features::default(), &ALL), VideoCodec::HEVC);
        assert_eq!(resolve_codec(VideoCodec::AV1, HmdType::QUEST3, Features::default(), &ALL), VideoCodec::AV1);
        let no_hevc = EncoderCaps { hevc: false, hevc_10bit: false, ..ALL };
        assert_eq!(resolve_codec(VideoCodec::HEVC, HmdType::QUEST3, Features::default(), &no_hevc), VideoCodec::H264);
    }

    #[test]
    fn nvenc_args() {
        let cfg = EncoderConfig {
            backend: Backend::Nvenc,
            codec: VideoCodec::HEVC10bit,
            width: 1920,
            height: 1080,
            framerate: 60,
            bitrate: 30_000_000,
            enable_aq: true,
            yuv444: false,
            two_pass: false,
            gop: 120,
            render_node: String::new(),
        };
        let a = cfg.encoder_args(FfmpegVersion { major: 7, minor: 1 });
        assert!(a.windows(2).any(|w| w == ["-c:v", "hevc_nvenc"]));
        assert!(a.windows(2).any(|w| w == ["-tune", "ull"]));
        assert!(a.windows(2).any(|w| w == ["-pix_fmt", "p010le"]));
    }
}
