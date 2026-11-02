//! Backend-neutral video pipeline: the in-process libav* pipeline ([`crate::inproc`], feature
//! `ffmpeg-lib`) when it can capture the source, otherwise the `ffmpeg` command-line pipeline
//! ([`crate::cli`]).
//!
//! Set `VD_ENCODER=cli` to force the command-line backend (for comparisons or troubleshooting).

#[cfg(feature = "ffmpeg-lib")]
use tracing::warn;

use crate::EncodedFrame;
use crate::capture::CaptureSpec;
use crate::cli::CliVideoPipeline;
use crate::codec::EncoderConfig;
use crate::error::Result;
use crate::version::Ffmpeg;

/// Run-time controls of a pipeline, if it has any.
#[derive(Debug, Clone)]
pub enum Controls {
    /// The CLI backend: key frames and bitrate changes need a restart.
    None,
    /// In-process pipeline controls.
    #[cfg(feature = "ffmpeg-lib")]
    Inproc(crate::inproc::Controller),
}

impl Controls {
    /// Requests a key frame without restarting.
    ///
    /// # Returns
    /// `false` when the pipeline must be restarted to produce one.
    pub fn force_keyframe(&self) -> bool {
        match self {
            Controls::None => false,
            #[cfg(feature = "ffmpeg-lib")]
            Controls::Inproc(c) => {
                c.force_keyframe();
                true
            }
        }
    }

    /// Changes the bitrate without restarting.
    ///
    /// # Arguments
    /// * `bps` - new bitrate in bit/s.
    ///
    /// # Returns
    /// `false` when the pipeline must be restarted to change it.
    pub fn set_bitrate(&self, bps: u32) -> bool {
        match self {
            Controls::None => {
                let _ = bps;
                false
            }
            #[cfg(feature = "ffmpeg-lib")]
            Controls::Inproc(c) => c.set_bitrate(bps),
        }
    }
}

/// A running capture + encode pipeline.
#[allow(clippy::large_enum_variant, reason = "one per stream, never moved in a hot path")]
pub enum VideoPipeline {
    /// `ffmpeg` child process.
    Cli(CliVideoPipeline),
    /// In-process libav* pipeline.
    #[cfg(feature = "ffmpeg-lib")]
    Inproc(crate::inproc::InprocVideoPipeline),
}

impl VideoPipeline {
    /// Starts a pipeline, preferring the in-process backend.
    ///
    /// # Arguments
    /// * `ff` - FFmpeg installation (CLI backend, option spellings).
    /// * `capture` - capture request.
    /// * `enc` - encoder configuration.
    ///
    /// # Returns
    /// The running pipeline.
    ///
    /// # Errors
    /// The CLI backend's start-up error when neither backend can start.
    pub fn start(ff: &Ffmpeg, capture: &CaptureSpec, enc: &EncoderConfig) -> Result<Self> {
        #[cfg(feature = "ffmpeg-lib")]
        {
            let forced_cli = std::env::var("VD_ENCODER").is_ok_and(|v| v.eq_ignore_ascii_case("cli"));
            if !forced_cli && enc.backend.is_hardware() && crate::inproc::supports(&capture.source) {
                match crate::inproc::InprocVideoPipeline::start(crate::inproc::linked_version(), capture, enc) {
                    Ok(p) => return Ok(Self::Inproc(p)),
                    Err(e) => warn!(error = %e, "in-process encoder unavailable; using the ffmpeg command line"),
                }
            }
        }
        Ok(Self::Cli(CliVideoPipeline::start(ff, capture, enc)?))
    }

    /// Run-time controls.
    ///
    /// # Returns
    /// The controls ([`Controls::None`] for the CLI backend).
    pub fn controls(&self) -> Controls {
        match self {
            Self::Cli(_) => Controls::None,
            #[cfg(feature = "ffmpeg-lib")]
            Self::Inproc(p) => Controls::Inproc(p.controller()),
        }
    }

    /// Short backend name for logs.
    ///
    /// # Returns
    /// `"in-process"` or `"ffmpeg cli"`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Cli(_) => "ffmpeg cli",
            #[cfg(feature = "ffmpeg-lib")]
            Self::Inproc(_) => "in-process",
        }
    }

    /// Waits for the next encoded frame.
    ///
    /// # Returns
    /// The frame, or `None` when the pipeline stopped.
    pub async fn next_frame(&mut self) -> Option<EncodedFrame> {
        match self {
            Self::Cli(p) => p.next_frame().await,
            #[cfg(feature = "ffmpeg-lib")]
            Self::Inproc(p) => p.next_frame().await,
        }
    }

    /// Stops the pipeline and waits until it has released the capture.
    pub async fn stop(&mut self) {
        match self {
            Self::Cli(p) => p.stop().await,
            #[cfg(feature = "ffmpeg-lib")]
            Self::Inproc(p) => p.stop().await,
        }
    }
}
