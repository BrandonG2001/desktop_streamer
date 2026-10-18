//! Error type of the media layer.

use thiserror::Error;

/// Convenience alias.
pub type Result<T, E = MediaError> = std::result::Result<T, E>;

/// Media failures.
#[derive(Debug, Error)]
pub enum MediaError {
    /// Spawning or talking to `ffmpeg` / `ffprobe` failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The `ffmpeg` executable was not found.
    #[error("ffmpeg executable not found (set VD_FFMPEG or install FFmpeg): {0}")]
    FfmpegNotFound(String),
    /// FFmpeg exited with an error.
    #[error("ffmpeg failed: {0}")]
    Ffmpeg(String),
    /// No encoder supports the requested codec on this machine.
    #[error("no usable hardware or software encoder for {0}")]
    NoEncoder(&'static str),
    /// The media file could not be probed.
    #[error("cannot probe media: {0}")]
    Probe(String),
    /// JSON from `ffprobe` could not be parsed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// The pipeline was stopped.
    #[error("pipeline stopped")]
    Stopped,
    /// libav* error (feature `ffmpeg-lib`).
    #[error("libav error: {0}")]
    Libav(String),
}
