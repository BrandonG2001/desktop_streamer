//! Control-channel message framing.
//!
//! C#: `VirtualDesktop.Net.MessageType` and `VirtualDesktop.Net.NetMessage`.
//!
//! Every control message on the messaging channel (TCP port 38810) is framed as:
//!
//! ```text
//! +---------+----------------------+-------------------------+
//! | type u8 | payload length i32BE | payload (length bytes)  |
//! +---------+----------------------+-------------------------+
//! ```
//!
//! The original keeps a pool of eight 256 KiB buffers (carved out of one 2 MiB array) and
//! allocates a dedicated buffer only for messages whose total size exceeds 256 KiB; that is a
//! performance detail and is not reproduced here.  The 5-byte header and big-endian length
//! are reproduced exactly.

use crate::binary::{BeReader, BeWriter};
use crate::error::{ProtocolError, Result};

/// Size of the frame header (`type` + `length`).
pub const HEADER_LEN: usize = 5;

/// Size of the original pooled buffers; larger messages were allocated individually.
pub const POOLED_BUFFER_LEN: usize = 256 * 1024;

/// Upper bound accepted by this port when decoding a frame (defensive; the original had none).
pub const MAX_PAYLOAD_LEN: usize = 64 * 1024 * 1024;

macro_rules! message_types {
    ($( $(#[$doc:meta])* $name:ident = $val:literal ),* $(,)?) => {
        /// Identifier of a control message (first byte of every frame).
        ///
        /// C#: `VirtualDesktop.Net.MessageType : byte`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum MessageType {
            $( $(#[$doc])* $name = $val, )*
        }

        impl MessageType {
            /// Converts a raw type byte into a known message type.
            ///
            /// # Arguments
            /// * `v` - the raw first byte of a frame.
            ///
            /// # Returns
            /// `Some(MessageType)` for known ids, `None` for unknown ones (which the original
            /// would still deliver to handlers that simply ignore them).
            pub fn from_u8(v: u8) -> Option<Self> {
                match v {
                    $( $val => Some(Self::$name), )*
                    _ => None,
                }
            }

            /// Returns the enum member name (what the original logs via `ToString()`).
            ///
            /// # Returns
            /// Static string naming the variant.
            pub fn name(self) -> &'static str {
                match self {
                    $( Self::$name => stringify!($name), )*
                }
            }
        }
    };
}

message_types! {
    /// Peer endpoint relayed by the rendezvous server (`IPEndPoint`, `bool`).
    PeerInfo = 1,
    /// Headset crash report (`string`, `string`).
    CrashCallstack = 3,
    /// Headset asks the streamer to update itself.
    UpdateRequired = 4,
    /// Streamer reply: no update available.
    UpdateUnavailable = 5,
    /// Streamer reply: update being installed.
    UpdateInProgress = 6,
    /// Full `SharedUserSettings` JSON.
    UserSettingsReload = 11,
    /// Single `SharedUserSettings` property JSON.
    UserSettingsPropertyChanged = 12,
    /// Full `SharedMobileSettings` JSON.
    MobileSettingsReload = 13,
    /// Single `SharedMobileSettings` property JSON.
    MobileSettingsPropertyChanged = 14,
    /// Full `SharedStreamerSettings` JSON.
    StreamerSettingsReload = 15,
    /// Single `SharedStreamerSettings` property JSON.
    StreamerSettingsPropertyChanged = 16,
    /// Guardian/boundary geometry changed (VR).
    BoundaryDataChanged = 21,
    /// Hand/body skeleton description changed (VR).
    SkeletonDataChanged = 22,
    /// Start streaming.
    BeginStreaming = 23,
    /// Restart streaming (forces a key frame).
    RestartStreaming = 24,
    /// Switch to the next monitor.
    SwitchMonitor = 25,
    /// Toggle Windows dictation (Win+H).
    ToggleDictation = 26,
    /// Launch SteamVR.
    LaunchStreamVR = 27,
    /// Exit SteamVR.
    ExitSteamVR = 28,
    /// Recenter the VR view.
    Recenter = 29,
    /// Request the game library.
    RequestGames = 30,
    /// Game library (`i16 count`, then per game `u8 platform`, `u64 id`, `string name`).
    Games = 31,
    /// Request thumbnails for games.
    RequestGameThumbnails = 32,
    /// One game thumbnail (`u8 platform`, `u64 id`, `i32 length`, image bytes).
    GameThumbnail = 33,
    /// Launch a game (`u8 platform`, `u64 id`).
    LaunchGame = 34,
    /// Exit a game (`u8 platform`, `u64 id`).
    ExitGame = 35,
    /// A game process exited (`u8 platform`, `u64 id`).
    GameExited = 36,
    /// Ask the headset to switch back to the desktop view.
    ShowDesktop = 40,
    /// Dispose the virtual gamepad.
    DisposeGamepadEmulation = 41,
    /// Add a virtual monitor.
    AddMonitor = 42,
    /// Remove a virtual monitor.
    RemoveMonitor = 43,
    /// Set a monitor's orientation (`i32 index`, `bool portrait`).
    SetMonitorPortrait = 44,
    /// Request a folder listing (`string relative path`).
    RequestVideos = 50,
    /// Folder listing.
    Videos = 51,
    /// Request video thumbnails.
    RequestVideoThumbnails = 52,
    /// One video thumbnail.
    VideoThumbnail = 53,
    /// Play a video (`string path`, `i64 position`, `i8 audio stream`).
    PlayVideo = 54,
    /// Download a video URL (`string`).
    DownloadVideo = 55,
    /// Stop playback.
    StopVideo = 56,
    /// Playback stopped notification.
    VideoStopped = 57,
    /// Seek (`i64 position`).
    SeekVideo = 58,
    /// Cancel a download.
    CancelDownloadVideo = 59,
    /// Audio track list.
    AudioTracks = 60,
    /// Select an audio track (`i8`).
    SetAudioStreamIndex = 61,
    /// Paste a URL from the PC clipboard.
    PasteUrl = 62,
    /// Cancel URL paste.
    CancelPasteUrl = 63,
    /// A video was added to the library (`string`, `i64`, `i64`, `i32`).
    VideoAdded = 64,
    /// Subtitle track list.
    SubtitleTracks = 65,
    /// Select a subtitle track by id (`Guid`).
    SetSubtitleTrackId = 66,
    /// Select an embedded subtitle stream (`i32`).
    SetSubtitleStreamIndex = 67,
    /// Subtitle delay (`i64`).
    SetSubtitleOffsetAmount = 68,
    /// Save a screenshot on the PC desktop (`string name`, `i32 length`, bytes).
    ScreenshotToDesktop = 70,
    /// Exit the streamer.
    Exit = 71,
    /// Enable VR passthrough.
    EnableVRPassthrough = 72,
    /// Disable VR passthrough.
    DisableVRPassthrough = 73,
    /// Toggle VR passthrough.
    ToggleVRPassthrough = 74,
    /// Toggle hand passthrough.
    ToggleHandPassthrough = 75,
    /// Toggle desk passthrough.
    ToggleDeskPassthrough = 76,
    /// Toggle the performance overlay.
    TogglePerformanceOverlay = 77,
    /// Toggle foveated streaming.
    ToggleFoveatedStreaming = 79,
    /// Cycle the PC input language (Win+Space).
    ChangeLanguage = 80,
    /// Disconnect.
    Disconnect = 81,
    /// Request a cursor image (`i64 cursor handle`).
    RequestCursor = 82,
    /// Cursor image (`i64 handle`, `i32 w`, `i32 h`, `i32 hotspot x`, `i32 hotspot y`, PNG bytes).
    Cursor = 83,
    /// Wake computers via Wake-on-LAN (`i32 count`, `count` × `string MAC`).
    WakeUpComputer = 90,
    /// Wake-on-LAN packet sent.
    ComputerAwaken = 91,
    /// Append a line to `Desktop/Log.txt` (`string`).
    Log = 100,
}

/// A framed control message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetMessage {
    /// Raw type byte (kept raw so unknown ids round-trip).
    pub raw_type: u8,
    /// Payload bytes (everything after the 5-byte header).
    pub payload: Vec<u8>,
}

impl NetMessage {
    /// Creates a message with an empty payload.
    ///
    /// # Arguments
    /// * `ty` - message type.
    ///
    /// # Returns
    /// The new message.
    pub fn new(ty: MessageType) -> Self {
        Self { raw_type: ty as u8, payload: Vec::new() }
    }

    /// Creates a message whose payload is produced by `build` writing into a [`BeWriter`].
    ///
    /// # Arguments
    /// * `ty` - message type.
    /// * `build` - closure that encodes the payload.
    ///
    /// # Returns
    /// The new message.
    ///
    /// # Errors
    /// Propagates any error returned by `build`.
    pub fn build(ty: MessageType, build: impl FnOnce(&mut BeWriter) -> Result<()>) -> Result<Self> {
        let mut w = BeWriter::new();
        build(&mut w)?;
        Ok(Self { raw_type: ty as u8, payload: w.into_inner() })
    }

    /// Creates a message with an already-encoded payload.
    ///
    /// # Arguments
    /// * `ty` - message type.
    /// * `payload` - payload bytes.
    ///
    /// # Returns
    /// The new message.
    pub fn with_payload(ty: MessageType, payload: Vec<u8>) -> Self {
        Self { raw_type: ty as u8, payload }
    }

    /// Returns the decoded message type, if known.
    ///
    /// # Returns
    /// `Some(MessageType)` or `None` for unknown ids.
    pub fn message_type(&self) -> Option<MessageType> {
        MessageType::from_u8(self.raw_type)
    }

    /// Returns a reader positioned at the start of the payload.
    ///
    /// # Returns
    /// A [`BeReader`] over the payload.
    pub fn reader(&self) -> BeReader<'_> {
        BeReader::new(&self.payload)
    }

    /// Returns the payload as text (used for the JSON settings messages, which the original
    /// reads through a `StreamReader` with UTF-8 BOM detection).
    ///
    /// # Returns
    /// The payload decoded as UTF-8 with a leading BOM stripped.
    ///
    /// # Errors
    /// [`ProtocolError::InvalidUtf8`] if the payload is not UTF-8.
    pub fn payload_text(&self) -> Result<String> {
        let bytes = self.payload.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&self.payload);
        Ok(String::from_utf8(bytes.to_vec())?)
    }

    /// Encodes the message into a frame (`type`, `i32 BE length`, payload).
    ///
    /// # Returns
    /// The encoded frame.
    ///
    /// # Errors
    /// [`ProtocolError::TooLarge`] if the payload exceeds `i32::MAX` bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let len = i32::try_from(self.payload.len()).map_err(|_| ProtocolError::TooLarge("message payload"))?;
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.push(self.raw_type);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    /// Parses a frame header.
    ///
    /// # Arguments
    /// * `header` - exactly [`HEADER_LEN`] bytes.
    ///
    /// # Returns
    /// `(raw type, payload length)`.
    ///
    /// # Errors
    /// [`ProtocolError::InvalidLength`] for a negative or implausibly large length.
    pub fn parse_header(header: &[u8; HEADER_LEN]) -> Result<(u8, usize)> {
        let len = i32::from_be_bytes([header[1], header[2], header[3], header[4]]);
        if len < 0 || len as usize > MAX_PAYLOAD_LEN {
            return Err(ProtocolError::InvalidLength { what: "message payload", length: i64::from(len) });
        }
        Ok((header[0], len as usize))
    }

    /// Decodes one complete frame from the front of `buf`.
    ///
    /// # Arguments
    /// * `buf` - buffered bytes from the socket.
    ///
    /// # Returns
    /// `Some((message, bytes consumed))` when a full frame is available, otherwise `None`.
    ///
    /// # Errors
    /// Propagates [`NetMessage::parse_header`] errors.
    pub fn decode(buf: &[u8]) -> Result<Option<(Self, usize)>> {
        if buf.len() < HEADER_LEN {
            return Ok(None);
        }
        let header: [u8; HEADER_LEN] = buf[..HEADER_LEN].try_into().expect("length checked");
        let (raw_type, len) = Self::parse_header(&header)?;
        if buf.len() < HEADER_LEN + len {
            return Ok(None);
        }
        let payload = buf[HEADER_LEN..HEADER_LEN + len].to_vec();
        Ok(Some((Self { raw_type, payload }, HEADER_LEN + len)))
    }
}

impl std::fmt::Display for NetMessage {
    /// Formats the message as its type name (or the raw id when unknown) and payload size.
    ///
    /// # Arguments
    /// * `f` - formatter.
    ///
    /// # Returns
    /// The formatter result.
    ///
    /// # Errors
    /// Only formatter errors.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.message_type() {
            Some(t) => write!(f, "{} ({} bytes)", t.name(), self.payload.len()),
            None => write!(f, "MessageType({}) ({} bytes)", self.raw_type, self.payload.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip() {
        let msg = NetMessage::build(MessageType::Log, |w| w.write_string("hi")).unwrap();
        let frame = msg.encode().unwrap();
        assert_eq!(&frame[..5], &[100, 0, 0, 0, 4]);
        let (decoded, used) = NetMessage::decode(&frame).unwrap().unwrap();
        assert_eq!(used, frame.len());
        assert_eq!(decoded, msg);
        assert_eq!(decoded.reader().read_string().unwrap(), "hi");
    }

    #[test]
    fn partial_frame_is_not_decoded() {
        let frame = NetMessage::new(MessageType::Exit).encode().unwrap();
        assert!(NetMessage::decode(&frame[..4]).unwrap().is_none());
    }
}
