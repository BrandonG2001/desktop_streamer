//! Data, video and audio channels.
//!
//! | type            | C#                 | send (PC → headset)                                  | receive (headset → PC)   |
//! |-----------------|--------------------|------------------------------------------------------|--------------------------|
//! | [`DataChannel`] | `_0012_0003._0002` | bandwidth probe, file / chunk / buffer transfers     | [`HeadsetState`] packets |
//! | [`VideoChannel`]| `_009D._0004`      | frames, secondary frames, stream descriptor, text    | [`GazeSample`]s          |
//! | [`AudioChannel`]| `_001D._0002`      | 48 kHz stereo PCM                                    | 48 kHz mono mic PCM      |

use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, watch};
use tracing::warn;
use vd_protocol::channels::{self as proto, data_tag};
use vd_protocol::headset_state::HeadsetState;
use vd_protocol::nal;
use vd_protocol::wire::{FrameHeader, GazeSample, StreamDescriptor};

use crate::error::Result;
use crate::link::LinkCore;
use crate::peer::{self, ChannelMode, PeerState};
use crate::stream::{SecureReader, SessionKey};

/// Largest inbound media packet accepted (defensive limit).
const MAX_INBOUND: usize = 16 * 1024 * 1024;

macro_rules! common_link_api {
    () => {
        /// Returns the channel state.
        ///
        /// # Returns
        /// The state.
        pub fn state(&self) -> PeerState {
            self.core.state()
        }

        /// Subscribes to state changes.
        ///
        /// # Returns
        /// A watch receiver.
        pub fn subscribe(&self) -> watch::Receiver<PeerState> {
            self.core.subscribe()
        }

        /// Reports whether the channel is connected.
        ///
        /// # Returns
        /// `true` when connected.
        pub fn is_connected(&self) -> bool {
            self.core.is_connected()
        }

        /// Disconnects.  C#: `Reset()`.
        pub async fn reset(&self) {
            self.core.reset().await;
        }
    };
}

/// Headset state + file transfer channel (TCP 38820).
pub struct DataChannel {
    core: Arc<LinkCore>,
    inbound: mpsc::Sender<HeadsetState>,
}

impl DataChannel {
    /// Creates the channel.
    ///
    /// # Returns
    /// The channel and the receiver of decoded headset states (bounded; when the consumer
    /// falls behind, older states are dropped because only the newest one matters).
    pub fn new() -> (Arc<Self>, mpsc::Receiver<HeadsetState>) {
        let (tx, rx) = mpsc::channel(8);
        (Arc::new(Self { core: LinkCore::new("data"), inbound: tx }), rx)
    }

    common_link_api!();

    /// Establishes the channel and starts receiving headset states.
    ///
    /// # Arguments
    /// * `mode` - LAN or remote.
    /// * `key` - session key.
    /// * `encrypt` - encrypt the peer connection.
    ///
    /// # Errors
    /// Connection errors.
    pub async fn establish(&self, mode: &ChannelMode, key: Option<&SessionKey>, encrypt: bool) -> Result<()> {
        self.core.set_state(PeerState::Listening);
        let conn = match peer::establish_channel(mode, key, encrypt).await {
            Ok(c) => c,
            Err(e) => {
                self.core.set_state(PeerState::Disconnected);
                return Err(e);
            }
        };
        let tx = self.inbound.clone();
        self.core.attach(conn, PeerState::Connected, move |reader| Self::receive_loop(reader, tx)).await;
        Ok(())
    }

    /// Receive loop.  C#: `_0012_0003._0002._0001<T>(T)` called by the "ReceiveFrameData Thread".
    ///
    /// # Arguments
    /// * `reader` - channel reader.
    /// * `tx` - output queue.
    ///
    /// # Errors
    /// Socket errors; corrupt packets are logged and skipped only if they fail the hash check
    /// (the original throws and resets on any error, which this port mirrors for framing errors).
    async fn receive_loop(mut reader: SecureReader, tx: mpsc::Sender<HeadsetState>) -> Result<()> {
        let mut size_warned = false;
        loop {
            let header: [u8; HeadsetState::PACKET_HEADER] = reader.read_array().await?;
            let (len, hash) = HeadsetState::parse_packet_header(&header)?;
            let body = reader.read_vec(len).await?;
            let (state, raw_len) = HeadsetState::decode_packet(hash, &body)?;
            // The headset omits trailing tracking sections it does not use (observed: 312 bytes
            // with no hand/body/face/eye tracking); the missing tail is zero-filled.  Only a
            // packet *larger* than the known layout indicates a layout mismatch.
            if raw_len > HeadsetState::SIZE && !size_warned {
                size_warned = true;
                warn!(raw_len, expected = HeadsetState::SIZE, "headset state larger than the reverse-engineered layout");
            } else if raw_len < HeadsetState::SIZE && !size_warned {
                size_warned = true;
                tracing::debug!(raw_len, "partial headset state (unused tracking sections omitted)");
            }
            if tx.try_send(state).is_err() && tx.is_closed() {
                return Ok(());
            }
        }
    }

    /// Sends the bandwidth probe.  C#: `ConnectionManager._0001()._0001(AutoAdjustBitrate ? 5000000 : 0)`.
    ///
    /// # Arguments
    /// * `random_bytes` - probe size (0 when auto-bitrate is disabled).
    ///
    /// # Errors
    /// Send errors.
    pub async fn send_bandwidth_probe(&self, random_bytes: i32) -> Result<()> {
        self.core.send(&proto::encode_bandwidth_probe(random_bytes)).await
    }

    /// Streams a complete file (header with total length, then raw bytes).
    ///
    /// # Arguments
    /// * `file` - async reader positioned at the start.
    /// * `total_len` - number of bytes that will be sent.
    ///
    /// # Errors
    /// File or socket errors.
    pub async fn send_file<R: tokio::io::AsyncRead + Unpin>(&self, mut file: R, total_len: i64) -> Result<()> {
        self.core.send(&proto::encode_file_header(total_len)).await?;
        let mut buf = vec![0u8; proto::FILE_CHUNK];
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                return Ok(());
            }
            self.core.send(&buf[..n]).await?;
        }
    }

    /// Sends one buffer packet (tag 3).
    ///
    /// # Arguments
    /// * `data` - payload.
    ///
    /// # Errors
    /// Send errors.
    pub async fn send_buffer(&self, data: &[u8]) -> Result<()> {
        self.core.send(&proto::encode_data_packet(data_tag::BUFFER, data)?).await
    }

    /// Sends one chunk of a growing file (tag 2).
    ///
    /// # Arguments
    /// * `data` - payload.
    ///
    /// # Errors
    /// Send errors.
    pub async fn send_chunk(&self, data: &[u8]) -> Result<()> {
        self.core.send(&proto::encode_data_packet(data_tag::CHUNK, data)?).await
    }

    /// Sends the single-byte marker (tag 4).
    ///
    /// # Errors
    /// Send errors.
    pub async fn send_marker(&self) -> Result<()> {
        self.core.send(&[data_tag::MARKER]).await
    }
}

/// Encoded video channel (TCP 38830).
pub struct VideoChannel {
    core: Arc<LinkCore>,
    inbound: mpsc::Sender<GazeSample>,
}

impl VideoChannel {
    /// Creates the channel.
    ///
    /// # Returns
    /// The channel and the receiver of gaze samples.
    pub fn new() -> (Arc<Self>, mpsc::Receiver<GazeSample>) {
        let (tx, rx) = mpsc::channel(8);
        (Arc::new(Self { core: LinkCore::new("video"), inbound: tx }), rx)
    }

    common_link_api!();

    /// Establishes the channel and starts receiving gaze samples.
    ///
    /// # Arguments
    /// * `mode` - LAN or remote.
    /// * `key` - session key.
    /// * `encrypt` - encrypt the peer connection.
    ///
    /// # Errors
    /// Connection errors.
    pub async fn establish(&self, mode: &ChannelMode, key: Option<&SessionKey>, encrypt: bool) -> Result<()> {
        self.core.set_state(PeerState::Listening);
        let conn = match peer::establish_channel(mode, key, encrypt).await {
            Ok(c) => c,
            Err(e) => {
                self.core.set_state(PeerState::Disconnected);
                return Err(e);
            }
        };
        let tx = self.inbound.clone();
        self.core
            .attach(conn, PeerState::Connected, move |mut reader| async move {
                loop {
                    let len = proto::parse_le_length(reader.read_array().await?, MAX_INBOUND)?;
                    let body = reader.read_vec(len).await?;
                    if body.len() >= std::mem::size_of::<GazeSample>() {
                        let gaze: GazeSample = bytemuck::pod_read_unaligned(&body[..16]);
                        let _ = tx.try_send(gaze);
                    }
                }
            })
            .await;
        Ok(())
    }

    /// Sends a primary frame.  C#: `_009D._0004._0001(ref _0084_0002._0002, IntPtr, bool)`.
    ///
    /// # Arguments
    /// * `header` - frame header (payload size is filled in).
    /// * `annex_b` - Annex-B access unit from the encoder.
    /// * `use_annex_b` - `SharedMobileSettings.UseAnnexB`; when `false` start codes are replaced by
    ///   4-byte big-endian lengths.
    ///
    /// # Errors
    /// Send errors.
    pub async fn send_frame(&self, header: FrameHeader, annex_b: &[u8], use_annex_b: bool) -> Result<()> {
        let pkt = if use_annex_b {
            proto::encode_video_frame(header, annex_b)?
        } else {
            proto::encode_video_frame(header, &nal::annex_b_to_length_prefixed(annex_b))?
        };
        self.core.send(&pkt).await
    }

    /// Sends a secondary-monitor frame (tag 1).
    ///
    /// # Arguments
    /// * `timestamp` - capture time.
    /// * `monitor_index` - monitor index ≥ 1.
    /// * `annex_b` - access unit.
    /// * `use_annex_b` - see [`VideoChannel::send_frame`].
    ///
    /// # Errors
    /// Send errors.
    pub async fn send_secondary_frame(&self, timestamp: i64, monitor_index: i32, annex_b: &[u8], use_annex_b: bool) -> Result<()> {
        let pkt = if use_annex_b {
            proto::encode_secondary_frame(timestamp, monitor_index, annex_b)?
        } else {
            proto::encode_secondary_frame(timestamp, monitor_index, &nal::annex_b_to_length_prefixed(annex_b))?
        };
        self.core.send(&pkt).await
    }

    /// Sends the stream descriptor (tag 2).  C#: `_009D._0004._0001(ref _001F._0005)`.
    ///
    /// # Arguments
    /// * `descriptor` - stream description.
    ///
    /// # Errors
    /// Send errors.
    pub async fn send_descriptor(&self, descriptor: &StreamDescriptor) -> Result<()> {
        self.core.send(&proto::encode_descriptor(descriptor)).await
    }

    /// Sends subtitle text (tag 3).  C#: `_009D._0004._0001(string)`.
    ///
    /// # Arguments
    /// * `text` - UTF-8 text (empty clears the subtitle).
    ///
    /// # Errors
    /// Send errors.
    pub async fn send_text(&self, text: &str) -> Result<()> {
        self.core.send(&proto::encode_text(text)?).await
    }
}

/// PCM audio channel (TCP 38840).
pub struct AudioChannel {
    core: Arc<LinkCore>,
    inbound: mpsc::Sender<Vec<u8>>,
}

impl AudioChannel {
    /// Creates the channel.
    ///
    /// # Returns
    /// The channel and the receiver of microphone PCM buffers (48 kHz mono s16le).
    pub fn new() -> (Arc<Self>, mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel(64);
        (Arc::new(Self { core: LinkCore::new("audio"), inbound: tx }), rx)
    }

    common_link_api!();

    /// Establishes the channel and starts receiving microphone audio.
    ///
    /// # Arguments
    /// * `mode` - LAN or remote.
    /// * `key` - session key.
    /// * `encrypt` - encrypt the peer connection.
    ///
    /// # Errors
    /// Connection errors.
    pub async fn establish(&self, mode: &ChannelMode, key: Option<&SessionKey>, encrypt: bool) -> Result<()> {
        self.core.set_state(PeerState::Listening);
        let conn = match peer::establish_channel(mode, key, encrypt).await {
            Ok(c) => c,
            Err(e) => {
                self.core.set_state(PeerState::Disconnected);
                return Err(e);
            }
        };
        let tx = self.inbound.clone();
        self.core
            .attach(conn, PeerState::Connected, move |mut reader| async move {
                loop {
                    let len = proto::parse_le_length(reader.read_array().await?, MAX_INBOUND)?;
                    let pcm = reader.read_vec(len).await?;
                    if tx.try_send(pcm).is_err() && tx.is_closed() {
                        return Ok(());
                    }
                }
            })
            .await;
        Ok(())
    }

    /// Sends one PCM packet.  C#: `_001D._0002._0001(IntPtr, int)`.
    ///
    /// # Arguments
    /// * `pcm` - 48 kHz stereo s16le samples (normally 1152 bytes).
    ///
    /// # Errors
    /// Send errors.
    pub async fn send_pcm(&self, pcm: &[u8]) -> Result<()> {
        self.core.send(&proto::encode_audio(pcm)?).await
    }
}
