//! TCP channels, port numbers and per-channel packet framing.
//!
//! A streaming session uses four TCP connections (all optionally wrapped by
//! [`crate::crypto`]):
//!
//! | channel   | LAN port | relay port            | C# class            | carries                                          |
//! |-----------|----------|-----------------------|---------------------|--------------------------------------------------|
//! | messaging | 38810    | relay (see `vd-net`)  | `NetMessagingClient`| [`NetMessage`](crate::NetMessage) control frames |
//! | data      | 38820    | 38821 + rotation      | `_0012_0003._0002`  | headset state ↑, bandwidth probe / file data ↓   |
//! | video     | 38830    | 38831 + rotation      | `_009D._0004`       | encoded frames ↓, gaze ↑                         |
//! | audio     | 38840    | 38841 + rotation      | `_001D._0002`       | 48 kHz PCM ↓ (speakers) and ↑ (microphone)       |
//!
//! "rotation" is `StreamerSettings.ServerRotation`.  UDP port 38860 is used for an empty LAN
//! broadcast that wakes up headsets after the PC resumes, and as the local port of Wake-on-LAN
//! sends.
//!
//! **Byte order.**  Unlike the control channel, media-channel headers are written with raw
//! pointer stores and are therefore *little-endian*, except where the original explicitly calls
//! `IPAddress.HostToNetworkOrder` (data-channel file transfer headers), which are big-endian.

use crate::error::{ProtocolError, Result};
use crate::wire::{FrameHeader, SecondaryFrameHeader, StreamDescriptor};

/// Messaging channel port.
pub const MESSAGING_PORT: u16 = 38810;
/// Data channel LAN port.
pub const DATA_PORT: u16 = 38820;
/// Video channel LAN port.
pub const VIDEO_PORT: u16 = 38830;
/// Audio channel LAN port.
pub const AUDIO_PORT: u16 = 38840;
/// UDP port used for LAN wake-up broadcasts and as the source port of Wake-on-LAN packets.
pub const BROADCAST_PORT: u16 = 38860;
/// Chunk size used by the original for file streaming on the data channel.
pub const FILE_CHUNK: usize = 81_920;
/// Size of the bandwidth probe sent on the data channel when `AutoAdjustBitrate` is enabled.
pub const BANDWIDTH_PROBE_BYTES: i32 = 5_000_000;

/// Logical channel of a streaming session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Control messages.
    Messaging,
    /// Headset state and file transfer.
    Data,
    /// Encoded video.
    Video,
    /// PCM audio.
    Audio,
}

impl Channel {
    /// All channels in connection order.
    pub const ALL: [Channel; 4] = [Channel::Messaging, Channel::Data, Channel::Video, Channel::Audio];

    /// Returns the LAN listening port of the channel.
    ///
    /// # Returns
    /// The TCP port.
    pub fn lan_port(self) -> u16 {
        match self {
            Channel::Messaging => MESSAGING_PORT,
            Channel::Data => DATA_PORT,
            Channel::Video => VIDEO_PORT,
            Channel::Audio => AUDIO_PORT,
        }
    }

    /// Returns the relay-server port used for remote (internet) sessions.
    ///
    /// # Arguments
    /// * `rotation` - `StreamerSettings.ServerRotation`.
    ///
    /// # Returns
    /// `None` for the messaging channel (its relay port is chosen by the registry), otherwise
    /// `lan_port + 1 + rotation`.
    pub fn relay_port(self, rotation: u16) -> Option<u16> {
        match self {
            Channel::Messaging => None,
            other => Some(other.lan_port() + 1 + rotation),
        }
    }

    /// Whether the original enables `SO_KEEPALIVE`-style settings for this channel's listener
    /// (`base(P_0, _0083: true)` is passed only by the video channel).
    ///
    /// # Returns
    /// `true` for the video channel.
    pub fn is_low_latency(self) -> bool {
        self == Channel::Video
    }
}

/// Packet tags written by the PC on the data channel.
pub mod data_tag {
    /// File header: `i64 BE` total length, then the raw file bytes.
    pub const FILE: u8 = 0;
    /// Streaming chunk of a growing file: `i32 BE` length, then bytes.
    pub const CHUNK: u8 = 2;
    /// One buffer: `i32 BE` length, then bytes.
    pub const BUFFER: u8 = 3;
    /// Single-byte marker (end of stream / keep-alive).
    pub const MARKER: u8 = 4;
}

/// Packet tags written by the PC on the video channel.
pub mod video_tag {
    /// Primary frame: [`FrameHeader`](crate::wire::FrameHeader) + bitstream.
    pub const FRAME: u8 = 0;
    /// Secondary-monitor frame: [`SecondaryFrameHeader`](crate::wire::SecondaryFrameHeader) + bitstream.
    pub const SECONDARY_FRAME: u8 = 1;
    /// Stream description: [`StreamDescriptor`](crate::wire::StreamDescriptor).
    pub const DESCRIPTOR: u8 = 2;
    /// UTF-8 text (subtitles): `i32 LE` length + bytes.
    pub const TEXT: u8 = 3;
}

/// Encodes the bandwidth probe sent first on the data channel.  C#: `_0012_0003._0002._0001(int)`.
///
/// # Arguments
/// * `random_bytes` - number of random bytes (0 disables the probe but still sends the length).
///
/// # Returns
/// `i32 BE length` followed by `random_bytes` random bytes.
pub fn encode_bandwidth_probe(random_bytes: i32) -> Vec<u8> {
    use rand::RngCore;
    let n = random_bytes.max(0) as usize;
    let mut out = Vec::with_capacity(4 + n);
    out.extend_from_slice(&random_bytes.max(0).to_be_bytes());
    let start = out.len();
    out.resize(start + n, 0);
    rand::thread_rng().fill_bytes(&mut out[start..]);
    out
}

/// Encodes the data-channel file header.  C#: `_0012_0003._0002._0001(Stream, CancellationToken)`.
///
/// # Arguments
/// * `total_len` - total number of file bytes that will follow.
///
/// # Returns
/// The 9-byte header (`tag 0`, `i64 BE`).
pub fn encode_file_header(total_len: i64) -> [u8; 9] {
    let mut out = [0u8; 9];
    out[0] = data_tag::FILE;
    out[1..].copy_from_slice(&total_len.to_be_bytes());
    out
}

/// Encodes a data-channel chunk or buffer packet (tags 2 / 3).
///
/// # Arguments
/// * `tag` - [`data_tag::CHUNK`] or [`data_tag::BUFFER`].
/// * `data` - payload.
///
/// # Returns
/// `tag`, `i32 BE length`, payload.
///
/// # Errors
/// [`ProtocolError::TooLarge`] if `data` exceeds `i32::MAX` bytes.
pub fn encode_data_packet(tag: u8, data: &[u8]) -> Result<Vec<u8>> {
    let len = i32::try_from(data.len()).map_err(|_| ProtocolError::TooLarge("data packet"))?;
    let mut out = Vec::with_capacity(5 + data.len());
    out.push(tag);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(data);
    Ok(out)
}

/// Encodes a primary video frame packet (tag 0).
///
/// # Arguments
/// * `header` - frame header; its `payload_size` is overwritten with `bitstream.len()`.
/// * `bitstream` - encoded access unit, already in the format the headset requested
///   (Annex-B or length-prefixed, see [`crate::nal`]).
///
/// # Returns
/// The packet bytes.
///
/// # Errors
/// [`ProtocolError::TooLarge`] if the bitstream exceeds `i32::MAX` bytes.
pub fn encode_video_frame(mut header: FrameHeader, bitstream: &[u8]) -> Result<Vec<u8>> {
    header.payload_size = i32::try_from(bitstream.len()).map_err(|_| ProtocolError::TooLarge("video frame"))?;
    let mut out = Vec::with_capacity(1 + FrameHeader::SIZE + bitstream.len());
    out.push(video_tag::FRAME);
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(bitstream);
    Ok(out)
}

/// Encodes a secondary-monitor video frame packet (tag 1).
///
/// # Arguments
/// * `timestamp` - capture time (100 ns ticks).
/// * `monitor_index` - monitor/encoder index (≥ 1).
/// * `bitstream` - encoded access unit.
///
/// # Returns
/// The packet bytes.
///
/// # Errors
/// [`ProtocolError::TooLarge`] if the bitstream exceeds `i32::MAX` bytes.
pub fn encode_secondary_frame(timestamp: i64, monitor_index: i32, bitstream: &[u8]) -> Result<Vec<u8>> {
    let header = SecondaryFrameHeader {
        timestamp,
        monitor_index,
        payload_size: i32::try_from(bitstream.len()).map_err(|_| ProtocolError::TooLarge("video frame"))?,
    };
    let mut out = Vec::with_capacity(1 + SecondaryFrameHeader::SIZE + bitstream.len());
    out.push(video_tag::SECONDARY_FRAME);
    out.extend_from_slice(bytemuck::bytes_of(&header));
    out.extend_from_slice(bitstream);
    Ok(out)
}

/// Encodes a stream-descriptor packet (tag 2).
///
/// # Arguments
/// * `descriptor` - stream description.
///
/// # Returns
/// The 89-byte packet.
pub fn encode_descriptor(descriptor: &StreamDescriptor) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + StreamDescriptor::SIZE);
    out.push(video_tag::DESCRIPTOR);
    out.extend_from_slice(descriptor.as_bytes());
    out
}

/// Encodes a text packet (tag 3), used for subtitles.
///
/// # Arguments
/// * `text` - UTF-8 text.
///
/// # Returns
/// `tag 3`, `i32 LE length`, bytes.
///
/// # Errors
/// [`ProtocolError::TooLarge`] if `text` exceeds `i32::MAX` bytes.
pub fn encode_text(text: &str) -> Result<Vec<u8>> {
    let len = i32::try_from(text.len()).map_err(|_| ProtocolError::TooLarge("text packet"))?;
    let mut out = Vec::with_capacity(5 + text.len());
    out.push(video_tag::TEXT);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(text.as_bytes());
    Ok(out)
}

/// Encodes one audio packet (both directions): `i32 LE length` + PCM bytes.
///
/// # Arguments
/// * `pcm` - interleaved signed 16-bit little-endian samples.
///
/// # Returns
/// The packet bytes.
///
/// # Errors
/// [`ProtocolError::TooLarge`] if `pcm` exceeds `i32::MAX` bytes.
pub fn encode_audio(pcm: &[u8]) -> Result<Vec<u8>> {
    let len = i32::try_from(pcm.len()).map_err(|_| ProtocolError::TooLarge("audio packet"))?;
    let mut out = Vec::with_capacity(4 + pcm.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(pcm);
    Ok(out)
}

/// Parses the little-endian `i32` length prefix used by audio and gaze packets.
///
/// # Arguments
/// * `prefix` - four bytes.
/// * `limit` - maximum accepted length.
///
/// # Returns
/// The length.
///
/// # Errors
/// [`ProtocolError::InvalidLength`] for negative values or values above `limit`.
pub fn parse_le_length(prefix: [u8; 4], limit: usize) -> Result<usize> {
    let len = i32::from_le_bytes(prefix);
    if len < 0 || len as usize > limit {
        return Err(ProtocolError::InvalidLength { what: "media packet", length: i64::from(len) });
    }
    Ok(len as usize)
}

/// Audio format constants shared by the loopback and microphone paths.
pub mod audio {
    /// Sample rate of both directions.
    pub const SAMPLE_RATE: u32 = 48_000;
    /// Channels of PC → headset audio (stereo).
    pub const SPEAKER_CHANNELS: u16 = 2;
    /// Channels of headset → PC microphone audio (mono).
    pub const MIC_CHANNELS: u16 = 1;
    /// Bytes per PC → headset packet (288 stereo frames = 6 ms).  C#: `LoopbackCapture` (1152).
    pub const SPEAKER_PACKET_BYTES: usize = 1152;
    /// Bytes per microphone read (960 mono frames = 20 ms).  C#: `Microphone` (1920).
    pub const MIC_PACKET_BYTES: usize = 1920;
    /// Gain applied to captured PC audio (`GainSource.Volume = 0.9`).
    pub const SPEAKER_GAIN: f32 = 0.9;

    /// Microphone gain from `SharedUserSettings.MicVolume` (`MathUtil.Lerp(0.1, 2.0, volume)`).
    ///
    /// # Arguments
    /// * `mic_volume` - user setting in `[0, 1]`.
    ///
    /// # Returns
    /// Linear gain.
    pub fn mic_gain(mic_volume: f32) -> f32 {
        0.1 + (2.0 - 0.1) * mic_volume.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_ports() {
        assert_eq!(Channel::Video.relay_port(0), Some(38831));
        assert_eq!(Channel::Audio.relay_port(2), Some(38843));
        assert_eq!(Channel::Messaging.relay_port(0), None);
    }

    #[test]
    fn video_frame_sets_payload_size() {
        let pkt = encode_video_frame(FrameHeader::default(), &[1, 2, 3]).unwrap();
        assert_eq!(pkt.len(), 1 + 160 + 3);
        let header = FrameHeader::from_bytes(&pkt[1..]).unwrap();
        assert_eq!(header.payload_size, 3);
    }
}
