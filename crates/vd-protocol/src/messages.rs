//! Typed payloads of individual control messages.
//!
//! Field types were recovered from the IL of the original (the C# decompilation hides them
//! behind identically named obfuscated overloads); see `docs/REVERSE_ENGINEERING.md` §4 for the
//! complete message table.  All integers are big-endian (see [`crate::binary`]).

use std::net::SocketAddr;

use crate::binary::{BeReader, BeWriter};
use crate::error::Result;
use crate::message::{MessageType, NetMessage};
use crate::types::GamePlatform;

/// Peer endpoint relayed by the rendezvous server.  C#: read in `_0096_0002._0002._0011` and
/// `ConnectionManager._0007`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerInfo {
    /// Public endpoint of the headset (used for NAT hole punching on remote sessions).
    pub endpoint: SocketAddr,
    /// Raw boolean; the streamer encrypts the **media** channels (data/video/audio) when this is
    /// **false** (`flag = !reader.ReadBoolean()` is passed as their "encrypt" argument).  The
    /// messaging channel is always encrypted regardless (a constant `true` in the original).
    pub unencrypted: bool,
}

impl PeerInfo {
    /// Decodes the payload.
    ///
    /// # Arguments
    /// * `msg` - a `PeerInfo` message.
    ///
    /// # Returns
    /// The decoded value.
    ///
    /// # Errors
    /// Propagates [`BeReader`] errors.
    pub fn decode(msg: &NetMessage) -> Result<Self> {
        let mut r = msg.reader();
        Ok(Self { endpoint: r.read_endpoint()?, unencrypted: r.read_bool()? })
    }

    /// Encodes the payload (used by tests and by the optional LAN rendezvous emulator).
    ///
    /// # Returns
    /// The message.
    pub fn encode(&self) -> NetMessage {
        let mut w = BeWriter::new();
        w.write_endpoint(self.endpoint);
        w.write_bool(self.unencrypted);
        NetMessage::with_payload(MessageType::PeerInfo, w.into_inner())
    }

    /// Whether the session must be wrapped in the AES record layer.
    ///
    /// # Returns
    /// `!unencrypted`.
    pub fn encrypt(&self) -> bool {
        !self.unencrypted
    }
}

/// Identifier of a game in the library (`VirtualDesktop.Interfaces.GameID`: platform + id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GameId {
    /// Store.
    pub platform: GamePlatform,
    /// Store-specific id (Steam app id, Oculus app id, ...).
    pub id: u64,
}

impl GameId {
    /// Reads `u8 platform, u64 id`.
    ///
    /// # Arguments
    /// * `r` - reader.
    ///
    /// # Returns
    /// The id.
    ///
    /// # Errors
    /// Propagates [`BeReader`] errors; unknown platforms map to `Steam`.
    pub fn read(r: &mut BeReader<'_>) -> Result<Self> {
        let platform = match r.read_u8()? {
            0 => GamePlatform::OculusRift,
            2 => GamePlatform::Viveport,
            _ => GamePlatform::Steam,
        };
        Ok(Self { platform, id: r.read_u64()? })
    }

    /// Writes `u8 platform, u64 id`.
    ///
    /// # Arguments
    /// * `w` - writer.
    pub fn write(&self, w: &mut BeWriter) {
        w.write_u8(self.platform as u8);
        w.write_u64(self.id);
    }
}

/// One entry of the `Games` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameEntry {
    /// Game id.
    pub id: GameId,
    /// Display name.
    pub name: String,
}

/// Encodes the `Games` message (`i16 count`, then `GameId` + `string name` per game).
///
/// # Arguments
/// * `games` - library entries (truncated to `i16::MAX`).
///
/// # Returns
/// The message.
///
/// # Errors
/// [`crate::ProtocolError::TooLarge`] if a name exceeds 32767 bytes.
pub fn encode_games(games: &[GameEntry]) -> Result<NetMessage> {
    NetMessage::build(MessageType::Games, |w| {
        let n = games.len().min(i16::MAX as usize);
        w.write_i16(n as i16);
        for g in &games[..n] {
            g.id.write(w);
            w.write_string(&g.name)?;
        }
        Ok(())
    })
}

/// Encodes a `GameThumbnail` message (`GameId`, `i32 length`, image bytes).
///
/// # Arguments
/// * `id` - game.
/// * `image` - JPEG/PNG bytes.
///
/// # Returns
/// The message.
pub fn encode_game_thumbnail(id: GameId, image: &[u8]) -> NetMessage {
    let mut w = BeWriter::with_capacity(13 + image.len());
    id.write(&mut w);
    w.write_i32(image.len() as i32);
    w.write_bytes(image);
    NetMessage::with_payload(MessageType::GameThumbnail, w.into_inner())
}

/// Encodes `GameExited` (`GameId`).
///
/// # Arguments
/// * `id` - the game that exited.
///
/// # Returns
/// The message.
pub fn encode_game_exited(id: GameId) -> NetMessage {
    let mut w = BeWriter::new();
    id.write(&mut w);
    NetMessage::with_payload(MessageType::GameExited, w.into_inner())
}

/// Cursor image reply.  C#: `_0008._0002._0001(long)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorImage {
    /// Cursor handle the headset asked for (`RequestCursor`).
    pub handle: i64,
    /// Image width.
    pub width: i32,
    /// Image height.
    pub height: i32,
    /// Hotspot X.
    pub hotspot_x: i32,
    /// Hotspot Y.
    pub hotspot_y: i32,
    /// PNG-encoded 32-bit ARGB image (written until end of message; no length prefix).
    pub png: Vec<u8>,
}

impl CursorImage {
    /// Encodes the `Cursor` message.
    ///
    /// # Returns
    /// The message.
    pub fn encode(&self) -> NetMessage {
        let mut w = BeWriter::with_capacity(24 + self.png.len());
        w.write_i64(self.handle);
        w.write_i32(self.width);
        w.write_i32(self.height);
        w.write_i32(self.hotspot_x);
        w.write_i32(self.hotspot_y);
        w.write_bytes(&self.png);
        NetMessage::with_payload(MessageType::Cursor, w.into_inner())
    }

    /// Decodes a `Cursor` message.
    ///
    /// # Arguments
    /// * `msg` - the message.
    ///
    /// # Returns
    /// The cursor.
    ///
    /// # Errors
    /// Propagates [`BeReader`] errors.
    pub fn decode(msg: &NetMessage) -> Result<Self> {
        let mut r = msg.reader();
        Ok(Self {
            handle: r.read_i64()?,
            width: r.read_i32()?,
            height: r.read_i32()?,
            hotspot_x: r.read_i32()?,
            hotspot_y: r.read_i32()?,
            png: r.read_to_end().to_vec(),
        })
    }
}

/// Decoded request from the headset that the streamer acts on.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// `PeerInfo`.
    PeerInfo(PeerInfo),
    /// `CrashCallstack(message, callstack)`.
    CrashCallstack(String, String),
    /// `SetMonitorPortrait(index, portrait)`.
    SetMonitorPortrait {
        /// Monitor index.
        index: i32,
        /// Portrait orientation.
        portrait: bool,
    },
    /// `ScreenshotToDesktop(file name, image bytes)`.
    Screenshot {
        /// File name.
        name: String,
        /// Image bytes.
        data: Vec<u8>,
    },
    /// `WakeUpComputer(MAC addresses)`.
    WakeUp(Vec<String>),
    /// `RequestCursor(handle)`.
    RequestCursor(i64),
    /// `Log(text)`.
    Log(String),
    /// `LaunchGame(id)`.
    LaunchGame(GameId),
    /// `ExitGame(id)`.
    ExitGame(GameId),
    /// `PlayVideo(path, position, audio stream)`.
    PlayVideo {
        /// Path relative to the videos root (`/` separators).
        path: String,
        /// Start position in 100 ns ticks.
        position: i64,
        /// Audio stream index (−1 = default).
        audio_stream: i8,
    },
    /// `SeekVideo(position)`.
    SeekVideo(i64),
    /// `SetAudioStreamIndex(index)`.
    SetAudioStream(i8),
    /// `DownloadVideo(url)`.
    DownloadVideo(String),
    /// `SetSubtitleTrackId(guid)`.
    SetSubtitleTrack([u8; 16]),
    /// `SetSubtitleStreamIndex(index)`.
    SetSubtitleStream(i32),
    /// `SetSubtitleOffsetAmount(ticks)`.
    SetSubtitleOffset(i64),
    /// `RequestVideos(relative folder)`.
    RequestVideos(String),
    /// A message without payload (or whose payload this port ignores).
    Simple(MessageType),
    /// Unknown message id.
    Unknown(u8),
}

impl Request {
    /// Decodes an incoming control message into a [`Request`].
    ///
    /// Settings messages are *not* decoded here (see [`crate::settings`]); they come back as
    /// [`Request::Simple`].
    ///
    /// # Arguments
    /// * `msg` - incoming message.
    ///
    /// # Returns
    /// The decoded request.
    ///
    /// # Errors
    /// Propagates [`BeReader`] errors for truncated payloads.
    pub fn decode(msg: &NetMessage) -> Result<Self> {
        let Some(ty) = msg.message_type() else {
            return Ok(Self::Unknown(msg.raw_type));
        };
        let mut r = msg.reader();
        Ok(match ty {
            MessageType::PeerInfo => Self::PeerInfo(PeerInfo::decode(msg)?),
            MessageType::CrashCallstack => Self::CrashCallstack(r.read_string()?, r.read_string()?),
            MessageType::SetMonitorPortrait => Self::SetMonitorPortrait { index: r.read_i32()?, portrait: r.read_bool()? },
            MessageType::ScreenshotToDesktop => {
                let name = r.read_string()?;
                let len = r.read_i32()?.max(0) as usize;
                Self::Screenshot { name, data: r.read_bytes(len)?.to_vec() }
            }
            MessageType::WakeUpComputer => {
                let n = r.read_i32()?.max(0);
                let mut macs = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    macs.push(r.read_string()?);
                }
                Self::WakeUp(macs)
            }
            MessageType::RequestCursor => Self::RequestCursor(r.read_i64()?),
            MessageType::Log => Self::Log(r.read_string()?),
            MessageType::LaunchGame => Self::LaunchGame(GameId::read(&mut r)?),
            MessageType::ExitGame => Self::ExitGame(GameId::read(&mut r)?),
            MessageType::PlayVideo => Self::PlayVideo { path: r.read_string()?, position: r.read_i64()?, audio_stream: r.read_i8()? },
            MessageType::SeekVideo => Self::SeekVideo(r.read_i64()?),
            MessageType::SetAudioStreamIndex => Self::SetAudioStream(r.read_i8()?),
            MessageType::DownloadVideo => Self::DownloadVideo(r.read_string()?),
            MessageType::SetSubtitleTrackId => Self::SetSubtitleTrack(r.read_guid()?),
            MessageType::SetSubtitleStreamIndex => Self::SetSubtitleStream(r.read_i32()?),
            MessageType::SetSubtitleOffsetAmount => Self::SetSubtitleOffset(r.read_i64()?),
            MessageType::RequestVideos => Self::RequestVideos(r.read_string()?),
            other => Self::Simple(other),
        })
    }
}

/// Flags of a video-library entry (C# `_0093._0001`).
pub mod video_entry_flags {
    /// The entry is a directory.
    pub const DIRECTORY: i32 = 1;
    /// The entry (or one of its parents) is hidden.
    pub const HIDDEN: i32 = 4;
}

/// One entry of the `Videos` listing.  C#: `_0083_0002._0003(string, long, DateTime, flags)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoEntry {
    /// Path relative to the videos root with `/` separators.
    pub path: String,
    /// File size in bytes (0 for directories).
    pub size: i64,
    /// Last write time as .NET `DateTime.Ticks` (100 ns since 0001-01-01).
    pub modified_ticks: i64,
    /// [`video_entry_flags`] bit set.
    pub flags: i32,
}

/// Offset between the .NET epoch (0001-01-01) and the Unix epoch in 100 ns ticks.
pub const DOTNET_UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;

/// Converts a `SystemTime` to .NET `DateTime.Ticks`.
///
/// # Arguments
/// * `t` - time.
///
/// # Returns
/// Ticks (saturating for times before 1970).
pub fn system_time_to_ticks(t: std::time::SystemTime) -> i64 {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => DOTNET_UNIX_EPOCH_TICKS + (d.as_nanos() / 100) as i64,
        Err(_) => DOTNET_UNIX_EPOCH_TICKS,
    }
}

/// Encodes the `Videos` folder listing (`i16 count`, then path/size/ticks/flags per entry).
///
/// # Arguments
/// * `entries` - directory entries.
///
/// # Returns
/// The message.
///
/// # Errors
/// [`crate::ProtocolError::TooLarge`] if a path exceeds 32767 bytes.
pub fn encode_videos(entries: &[VideoEntry]) -> Result<NetMessage> {
    NetMessage::build(MessageType::Videos, |w| {
        let n = entries.len().min(i16::MAX as usize);
        w.write_i16(n as i16);
        for e in &entries[..n] {
            w.write_string(&e.path)?;
            w.write_i64(e.size);
            w.write_i64(e.modified_ticks);
            w.write_i32(e.flags);
        }
        Ok(())
    })
}

/// Encodes `VideoAdded` (a single entry, same field layout as a `Videos` entry).
///
/// # Arguments
/// * `entry` - the new file.
///
/// # Returns
/// The message.
///
/// # Errors
/// [`crate::ProtocolError::TooLarge`] if the path exceeds 32767 bytes.
pub fn encode_video_added(entry: &VideoEntry) -> Result<NetMessage> {
    NetMessage::build(MessageType::VideoAdded, |w| {
        w.write_string(&entry.path)?;
        w.write_i64(entry.size);
        w.write_i64(entry.modified_ticks);
        w.write_i32(entry.flags);
        Ok(())
    })
}

/// An audio track of the playing video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    /// Track title / language.
    pub name: String,
    /// Channel count.
    pub channels: u8,
    /// Stream index passed back in `SetAudioStreamIndex`.
    pub index: i8,
}

/// Encodes `AudioTracks` (`u8 count`, then `string name`, `u8 channels`, `i8 index`).
///
/// # Arguments
/// * `tracks` - tracks (truncated to 255).
///
/// # Returns
/// The message.
///
/// # Errors
/// [`crate::ProtocolError::TooLarge`] if a name exceeds 32767 bytes.
pub fn encode_audio_tracks(tracks: &[AudioTrack]) -> Result<NetMessage> {
    NetMessage::build(MessageType::AudioTracks, |w| {
        let n = tracks.len().min(255);
        w.write_u8(n as u8);
        for t in &tracks[..n] {
            w.write_string(&t.name)?;
            w.write_u8(t.channels);
            w.write_i8(t.index);
        }
        Ok(())
    })
}

/// A subtitle track (embedded stream or side-car `.srt`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleTrack {
    /// Kind byte (0 = embedded stream, 1 = external file — inferred).
    pub kind: u8,
    /// Track id used by `SetSubtitleTrackId`.
    pub id: [u8; 16],
    /// Title.
    pub title: String,
    /// Language.
    pub language: String,
    /// Embedded stream index (or −1).
    pub stream_index: i32,
}

/// Encodes `SubtitleTracks` (`u8 count`, then kind/guid/title/language/index per track).
///
/// # Arguments
/// * `tracks` - tracks (truncated to 255).
///
/// # Returns
/// The message.
///
/// # Errors
/// [`crate::ProtocolError::TooLarge`] if a string exceeds 32767 bytes.
pub fn encode_subtitle_tracks(tracks: &[SubtitleTrack]) -> Result<NetMessage> {
    NetMessage::build(MessageType::SubtitleTracks, |w| {
        let n = tracks.len().min(255);
        w.write_u8(n as u8);
        for t in &tracks[..n] {
            w.write_u8(t.kind);
            w.write_guid(&t.id);
            w.write_string(&t.title)?;
            w.write_string(&t.language)?;
            w.write_i32(t.stream_index);
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_video_decodes() {
        let msg = NetMessage::build(MessageType::PlayVideo, |w| {
            w.write_string("movies/a.mkv")?;
            w.write_i64(42);
            w.write_i8(-1);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            Request::decode(&msg).unwrap(),
            Request::PlayVideo { path: "movies/a.mkv".into(), position: 42, audio_stream: -1 }
        );
    }

    #[test]
    fn peer_info_round_trip() {
        let p = PeerInfo { endpoint: "10.0.0.5:5000".parse().unwrap(), unencrypted: false };
        assert_eq!(PeerInfo::decode(&p.encode()).unwrap(), p);
        assert!(p.encrypt());
    }
}
