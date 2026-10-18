//! Error type of the networking layer.

use thiserror::Error;

/// Convenience alias.
pub type Result<T, E = NetError> = std::result::Result<T, E>;

/// Networking failures.
#[derive(Debug, Error)]
pub enum NetError {
    /// Socket I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A wire format could not be encoded or decoded.
    #[error("protocol error: {0}")]
    Protocol(#[from] vd_protocol::ProtocolError),
    /// The peer closed the connection.
    #[error("connection closed by peer")]
    Closed,
    /// An operation did not complete in time.
    #[error("timed out: {0}")]
    Timeout(&'static str),
    /// The listening port is already in use (WSAEADDRINUSE 10048 / EADDRINUSE).
    #[error("address already in use: {0}")]
    AddrInUse(std::io::Error),
    /// Access to the port was denied (WSAEACCES 10013 / EACCES) — typically a firewall, VPN or
    /// anti-virus product ("Anti-virus or VPN software is preventing connections to your PC").
    #[error("access denied binding socket: {0}")]
    AccessDenied(std::io::Error),
    /// The operation requires a connected peer.
    #[error("expecting a connected client")]
    NotConnected,
    /// The rendezvous server sent an unexpected message.
    #[error("expected PeerInfo message, got {0}")]
    UnexpectedMessage(String),
    /// HTTP / SOAP request failed.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    /// SOAP fault or malformed SOAP response.
    #[error("SOAP error: {0}")]
    Soap(String),
    /// UPnP gateway error.
    #[error("UPnP error: {0}")]
    Upnp(String),
}

impl NetError {
    /// Classifies a bind/connect error the way the original does (10048 → port in use, 10013 →
    /// access denied).
    ///
    /// # Arguments
    /// * `e` - the I/O error.
    ///
    /// # Returns
    /// The specific variant, or [`NetError::Io`].
    pub fn from_bind(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::AddrInUse => NetError::AddrInUse(e),
            std::io::ErrorKind::PermissionDenied => NetError::AccessDenied(e),
            _ => NetError::Io(e),
        }
    }
}
