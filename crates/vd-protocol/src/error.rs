//! Error type shared by every decoder/encoder in this crate.

use thiserror::Error;

/// Convenience alias for results produced by this crate.
pub type Result<T, E = ProtocolError> = std::result::Result<T, E>;

/// Every failure a wire-format encoder or decoder can report.
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// The buffer ended before a complete value could be read.
    #[error("unexpected end of data: needed {needed} more byte(s) at offset {offset}")]
    UnexpectedEof {
        /// Offset at which the read started.
        offset: usize,
        /// Number of bytes that were missing.
        needed: usize,
    },
    /// A length prefix was negative or larger than the protocol allows.
    #[error("invalid length {length} for {what}")]
    InvalidLength {
        /// What was being decoded.
        what: &'static str,
        /// The offending length.
        length: i64,
    },
    /// A string payload was not valid UTF-8.
    #[error("invalid UTF-8 string: {0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
    /// A value is too large to be represented by the wire format.
    #[error("value too large for wire format: {0}")]
    TooLarge(&'static str),
    /// An IP address family marker other than 4 (IPv4) or 16 (IPv6) was found.
    #[error("unsupported IP address length {0}")]
    BadAddressLength(u8),
    /// An integrity check (hash) failed.
    #[error("integrity check failed: {0}")]
    Integrity(&'static str),
    /// LZ4 decompression failed.
    #[error("LZ4 decompression failed: {0}")]
    Lz4(#[from] lz4_flex::block::DecompressError),
    /// JSON (settings synchronisation) failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// The encrypted record layer was given bad key material or a corrupt record.
    #[error("crypto error: {0}")]
    Crypto(&'static str),
    /// A packet tag on a media channel is not known.
    #[error("unknown packet tag {0}")]
    UnknownTag(u8),
}
