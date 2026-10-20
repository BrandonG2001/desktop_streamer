//! # vd-net
//!
//! Asynchronous (tokio) networking layer of the streamer.
//!
//! | module        | replaces (C#)                                         | purpose                                      |
//! |---------------|-------------------------------------------------------|----------------------------------------------|
//! | [`stream`]    | `_0096_0002._0001` + `NetworkStream`                  | plain / AES record-layer socket halves       |
//! | [`peer`]      | `_0096_0002._0002`                                    | connection state machine, listen / relay     |
//! | [`messaging`] | `VirtualDesktop.Net.NetMessagingClient`               | control-message channel                      |
//! | [`channels`]  | `_0012_0003._0002`, `_009D._0004`, `_001D._0002`      | data / video / audio channels                |
//! | [`registry`]  | `IComputerRegistry` WCF client (`BasicHttpBinding`)   | computer registration with vrdesktop.net     |
//! | [`relay`]     | `_001C._0001`                                         | region and relay-server selection            |
//! | [`discovery`] | `ConnectionManager._0007`, `WOLHelper`                | LAN wake broadcast and Wake-on-LAN           |
//! | [`routing`]   | `TraceRoute`                                          | NAT classification                           |
//! | [`upnp`]      | `UPnPManager` (Open.NAT)                              | router port forwarding                       |
//! | [`adapters`]  | `_0019._0002`                                         | network adapter enumeration                  |

#![warn(missing_docs)]

pub mod adapters;
pub mod channels;
pub mod discovery;
pub mod error;
pub mod link;
pub mod messaging;
pub mod peer;
pub mod registry;
pub mod relay;
pub mod routing;
pub mod stream;
pub mod upnp;

pub use error::{NetError, Result};

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
