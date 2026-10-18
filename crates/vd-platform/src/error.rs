//! Error type of the platform layer.

use thiserror::Error;

/// Convenience alias.
pub type Result<T, E = PlatformError> = std::result::Result<T, E>;

/// Platform failures.
#[derive(Debug, Error)]
pub enum PlatformError {
    /// An OS call failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A Win32 API returned an error code.
    #[error("{api} failed with error {code}")]
    Win32 {
        /// API name.
        api: &'static str,
        /// Error code.
        code: u32,
    },
    /// The feature is not available on this platform.
    #[error("not supported on this platform: {0}")]
    Unsupported(&'static str),
    /// A helper tool (`pactl`, `parec`, `iw`, `xrandr`, …) is missing or failed.
    #[error("helper command failed: {0}")]
    Command(String),
    /// Access was denied (e.g. `/dev/uinput` not writable — add the user to the `input` group or
    /// install the udev rule from `docs/`).
    #[error("permission denied: {0}")]
    PermissionDenied(String),
}
