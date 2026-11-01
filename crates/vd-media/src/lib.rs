//! # vd-media
//!
//! Video encoding and media playback for the streamer, isolated behind an API that does not
//! depend on a particular FFmpeg version.
//!
//! The original streamer used three hand-written encoder back-ends — NVENC through
//! `nvEncodeAPI64.dll` (`_008B._0003`), AMD AMF through `libVirtualDesktopAMF.dll`
//! (`_0003_0002._0005`) and Media Foundation MFTs for Intel/other GPUs (`_001B_0002._0002`) — and
//! FFmpeg 7 (`avcodec-61.dll` …, through FFmpeg.AutoGen) only for video-file playback,
//! thumbnails and encoder probing.  This crate replaces all of them with FFmpeg, which exposes
//! the same hardware encoders on every platform (`*_nvenc`, `*_amf`, `*_qsv`, `*_vaapi`,
//! `*_videotoolbox`, `*_v4l2m2m`, `*_mf`).
//!
//! Two back-ends are provided:
//!
//! * [`cli`] (always available) drives the `ffmpeg` executable through pipes.  It works with any
//!   FFmpeg ≥ 5 and on ARM, and needs no development headers.  **All FFmpeg-version specific
//!   knowledge lives in [`version`] and [`codec`]**, so upgrading FFmpeg means editing those two
//!   modules only.
//! * `inproc` (feature `ffmpeg-lib`) runs the same capture graph and encoder inside the
//!   process through the libav* C API (`ffmpeg-sys-next`), allowing on-demand key frames, live
//!   bitrate changes and measured latency without restarting anything.  Pin the
//!   `ffmpeg-sys-next` major version in `Cargo.toml` to the FFmpeg major version you build against.
//!
//! [`pipeline::VideoPipeline`] picks the in-process backend when available and falls back to the
//! command line otherwise.

#![warn(missing_docs)]

pub mod capture;
pub mod cli;
pub mod codec;
pub mod error;
#[cfg(feature = "ffmpeg-lib")]
pub mod inproc;
pub mod obu;
pub mod pipeline;
pub mod probe;
pub mod reader;
pub mod thumbnail;
pub mod version;

pub use error::{MediaError, Result};

/// One encoded access unit ready to be sent on the video channel.
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    /// Annex-B (H.264/HEVC) or low-overhead OBU (AV1) bitstream of one picture.
    pub data: Vec<u8>,
    /// Whether the picture is a random-access point.
    pub keyframe: bool,
    /// Capture time in 100 ns ticks of [`now_ticks`].
    pub capture_time: i64,
    /// Time spent between capture and encoder output, in milliseconds.
    pub encode_latency_ms: f32,
}

/// Monotonic clock in 100 ns ticks, equivalent to the original's
/// `Stopwatch.GetTimestamp() * (10^7 / Stopwatch.Frequency)`.
///
/// # Returns
/// Ticks since an arbitrary process-wide origin.
pub fn now_ticks() -> i64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let origin = *ORIGIN.get_or_init(Instant::now);
    (origin.elapsed().as_nanos() / 100) as i64
}

/// Creates a [`tokio::process::Command`] that never opens a console window on Windows (the
/// streamer may run without a console, e.g. when started from the settings window).
///
/// # Arguments
/// * `program` - executable to run.
///
/// # Returns
/// The command builder.
pub(crate) fn command(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut c = tokio::process::Command::new(program);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c
}
