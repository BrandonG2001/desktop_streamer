//! # vd-protocol
//!
//! Pure (I/O free) re-implementation of every wire format used between the
//! *Virtual Desktop Streamer* (PC side) and the Virtual Desktop headset application.
//!
//! Everything in this crate was recovered by reverse engineering
//! `VirtualDesktop.Streamer.exe` v1.34.18 (a SmartAssembly-obfuscated .NET Framework 4.7.1
//! assembly).  The original (obfuscated) type that each Rust item replaces is cited in the
//! item documentation as `C#: <namespace>.<type>` so the mapping can be re-verified against a
//! newer build.  See `docs/REVERSE_ENGINEERING.md` at the repository root for the full write-up.
//!
//! Module overview:
//!
//! | module            | purpose                                                                 |
//! |-------------------|-------------------------------------------------------------------------|
//! | [`binary`]        | big-endian `BinaryReader`/`BinaryWriter` clone used by message payloads |
//! | [`message`]       | [`MessageType`] ids and the 5-byte framed [`NetMessage`]                |
//! | [`messages`]      | typed payloads for individual message ids                               |
//! | [`crypto`]        | AES-128/256-CBC record layer wrapping every TCP channel                 |
//! | [`channels`]      | TCP ports and per-channel packet framing (data / video / audio)         |
//! | [`wire`]          | `#[repr(C)]` structs copied verbatim onto the video channel             |
//! | [`headset_state`] | the 4936-byte LZ4-compressed headset state sent every frame             |
//! | [`nal`]           | Annex-B / length-prefixed H.264/HEVC/AV1 bitstream helpers              |
//! | [`settings`]      | JSON-synchronised shared settings objects                               |
//! | [`hmd`]           | per-headset capability tables (resolutions, bitrates, monitor counts)  |
//! | [`foveation`]     | encoder QP-delta ("emphasis") map generation for foveated encoding      |
//! | [`types`]         | enums shared by all of the above                                        |
//! | [`math`]          | plain-old-data math types (vectors, quaternions, poses, rectangles)     |

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod binary;
pub mod channels;
pub mod crypto;
pub mod error;
pub mod foveation;
pub mod headset_state;
pub mod hmd;
pub mod math;
pub mod message;
pub mod messages;
pub mod nal;
pub mod settings;
pub mod types;
pub mod wire;

pub use error::{ProtocolError, Result};
pub use message::{MessageType, NetMessage};
