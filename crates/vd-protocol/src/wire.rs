//! `#[repr(C)]` structures copied byte-for-byte onto the video channel.
//!
//! The original writes these with unsafe pointer casts (`*(T*)ptr = value`), i.e. in the CLR's
//! sequential, naturally aligned, little-endian layout with `bool` = 1 byte (all booleans carry
//! `[MarshalAs(UnmanagedType.I1)]`).  Layouts below were computed from the decompiled field
//! order and are guarded by size/offset assertions; explicit padding fields make the structs
//! `Pod` so they can be cast to and from bytes without `unsafe`.

use bytemuck::{Pod, Zeroable};

use crate::math::{FovPort, Point, Rect, Size2};
use crate::types::{StreamingSource, VideoCodec};

/// Haptic / force-feedback value for one controller.  C#: `_008A._0003` (12 bytes).
///
/// `DesktopStreamer` fills `amplitude` with the gamepad rumble motor speed (`byte / 255`)
/// that the input-injection service writes back into shared memory; `extra` is an `int64`
/// split into two `int32`s by `_0001(long)` (low half → `x`, high half → `y`) whose meaning
/// (likely a duration or sequence) could not be established statically.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct HapticFeedback {
    /// Vibration amplitude, 0.0 – 1.0.
    pub amplitude: f32,
    /// Packed 64-bit companion value (see type docs).
    pub extra: Point,
}

impl HapticFeedback {
    /// Stores a packed 64-bit value into [`HapticFeedback::extra`] the way `_0001(long)` does.
    ///
    /// # Arguments
    /// * `value` - 64-bit value; low 32 bits go to `x`, high 32 bits to `y`.
    pub fn set_extra(&mut self, value: i64) {
        self.extra.x = (value & 0xFFFF_FFFF) as i32;
        self.extra.y = (value >> 32) as i32;
    }
}

/// Per-frame VR render description.  C#: `_0094._0004` (92 bytes).
///
/// Filled by `OculusStreamer` from the driver shared memory; zero for desktop/video frames.
/// The fourteen floats are, by position in the decompiled constructor, pose-prediction and
/// projection parameters followed by the two eye field-of-views; individual meanings are not
/// needed by the desktop path and are therefore kept positional.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct VrFrameInfo {
    /// Three boolean flags (`_0001`..`_0003`).
    pub flags: [u8; 3],
    /// Alignment padding.
    pub _pad: u8,
    /// Fourteen positional float parameters (`_0001`..`_0013`).
    pub params: [f32; 14],
    /// Left-eye field of view.
    pub fov_left: FovPort,
    /// Right-eye field of view.
    pub fov_right: FovPort,
}

/// Header of a primary video frame (packet tag 0 on the video channel).  C#: `_0084_0002._0002` (160 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct FrameHeader {
    /// Capture time in 100 ns ticks (`Stopwatch.GetTimestamp() * 10^7 / Frequency`), divided by
    /// the playback speed for video files.
    pub timestamp: i64,
    /// Index of the most recent headset state packet the frame responds to (used by the headset
    /// to measure motion-to-photon latency).  Negative values force a key frame.
    pub ack_frame_index: i32,
    /// [`StreamingSource`] as a byte.
    pub source: u8,
    /// First flag (`_0001`): for video files this is "is key frame".
    pub flag_a: u8,
    /// Second flag (`_0002`): set by the video path from `MediaReader` (end-of-stream / discontinuity).
    pub flag_b: u8,
    /// Alignment padding.
    pub _pad0: u8,
    /// VR parameter 1 (`_0001`), zero for desktop.
    pub vr_param_a: f32,
    /// VR parameter 2 (`_0002`), zero for desktop.
    pub vr_param_b: f32,
    /// Encoder latency in milliseconds measured by the encoder output thread (`_0003`).
    pub encode_latency_ms: f32,
    /// VR render description (zero for desktop).
    pub vr: VrFrameInfo,
    /// Left controller haptics.
    pub haptic_left: HapticFeedback,
    /// Right controller haptics.
    pub haptic_right: HapticFeedback,
    /// VR render target size (zero for desktop).
    pub render_size: Size2,
    /// Size in bytes of the bitstream that follows the header.
    pub payload_size: i32,
    /// Trailing alignment padding (struct alignment is 8).
    pub _pad1: [u8; 4],
}

impl FrameHeader {
    /// Size of the header on the wire.
    pub const SIZE: usize = 160;

    /// Builds a desktop frame header.  C#: `new _0084_0002._0002(long, int, StreamingSource, ref haptic, ref haptic)`.
    ///
    /// # Arguments
    /// * `timestamp` - capture time in 100 ns ticks.
    /// * `ack_frame_index` - last headset state index received.
    /// * `source` - streaming source (normally `Desktop`).
    /// * `haptic_left` / `haptic_right` - rumble feedback to forward.
    ///
    /// # Returns
    /// The header with `payload_size` = 0 (set it once the bitstream size is known).
    pub fn desktop(timestamp: i64, ack_frame_index: i32, source: StreamingSource, haptic_left: HapticFeedback, haptic_right: HapticFeedback) -> Self {
        Self { timestamp, ack_frame_index, source: source as u8, haptic_left, haptic_right, ..Self::default() }
    }

    /// Builds a video-file frame header.  C#: `new _0084_0002._0002(long, StreamingSource, bool, bool, int)`.
    ///
    /// # Arguments
    /// * `timestamp` - presentation time (already divided by playback speed).
    /// * `key_frame` - whether the packet is a key frame.
    /// * `flag_b` - second flag reported by the media reader.
    /// * `payload_size` - packet size in bytes.
    ///
    /// # Returns
    /// The header.
    pub fn video(timestamp: i64, key_frame: bool, flag_b: bool, payload_size: i32) -> Self {
        Self {
            timestamp,
            source: StreamingSource::Video as u8,
            flag_a: u8::from(key_frame),
            flag_b: u8::from(flag_b),
            payload_size,
            ..Self::default()
        }
    }

    /// Returns the header bytes exactly as sent on the wire.
    ///
    /// # Returns
    /// A 160-byte slice.
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }

    /// Parses a header from wire bytes (unaligned input allowed).
    ///
    /// # Arguments
    /// * `bytes` - at least [`FrameHeader::SIZE`] bytes.
    ///
    /// # Returns
    /// `Some(header)` or `None` if `bytes` is too short.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        bytes.get(..Self::SIZE).map(bytemuck::pod_read_unaligned)
    }
}

/// Header of a frame of a secondary monitor (packet tag 1).  C#: `_008C._0005` (16 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Pod, Zeroable)]
pub struct SecondaryFrameHeader {
    /// Capture time in 100 ns ticks (same clock as the primary monitor's frame).
    pub timestamp: i64,
    /// Zero-based monitor (encoder) index, ≥ 1.
    pub monitor_index: i32,
    /// Bitstream size in bytes.
    pub payload_size: i32,
}

impl SecondaryFrameHeader {
    /// Size on the wire.
    pub const SIZE: usize = 16;
}

/// Description of the active video stream(s) (packet tag 2).  C#: `_001F._0005` (88 bytes).
///
/// Up to three monitors are described.  For VR, `view_count` is 2 (or 4 when the foveated
/// inset views are enabled) and only the first entry is used.  For video files the first
/// rectangle's `x`/`y` carry the high/low 32 bits of a 64-bit value (duration).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct StreamDescriptor {
    /// [`StreamingSource`] byte.
    pub source: u8,
    /// Alignment padding.
    pub _pad0: [u8; 3],
    /// [`VideoCodec`] value actually selected by the encoder.
    pub codec: i32,
    /// Frame rate (desktop: `min(display refresh, DesktopFramerate)`).
    pub framerate: i32,
    /// Encoded size of view/monitor 0.
    pub size0: Size2,
    /// Number of views / monitors.
    pub view_count: i32,
    /// Source rectangle of monitor 0 in virtual-desktop coordinates.
    pub rect0: Rect,
    /// Encoded size of monitor 1.
    pub size1: Size2,
    /// Source rectangle of monitor 1.
    pub rect1: Rect,
    /// Encoded size of monitor 2.
    pub size2: Size2,
    /// Source rectangle of monitor 2.
    pub rect2: Rect,
}

impl StreamDescriptor {
    /// Size on the wire.
    pub const SIZE: usize = 88;

    /// Maximum number of monitors the descriptor can describe.
    pub const MAX_MONITORS: usize = 3;

    /// Creates a descriptor for a desktop stream.
    ///
    /// # Arguments
    /// * `codec` - selected codec.
    /// * `framerate` - stream frame rate.
    /// * `monitors` - `(encoded size, source rect)` for each monitor (extra entries are ignored).
    ///
    /// # Returns
    /// The descriptor with `view_count = min(monitors.len(), 3)`.
    pub fn desktop(codec: VideoCodec, framerate: i32, monitors: &[(Size2, Rect)]) -> Self {
        let mut d = Self { source: StreamingSource::Desktop as u8, codec: codec as i32, framerate, ..Self::default() };
        d.view_count = monitors.len().min(Self::MAX_MONITORS) as i32;
        for (i, (size, rect)) in monitors.iter().take(Self::MAX_MONITORS).enumerate() {
            d.set_monitor(i, *size, *rect);
        }
        d
    }

    /// Sets the size and rectangle of monitor `index`.
    ///
    /// # Arguments
    /// * `index` - 0, 1 or 2 (other values are ignored).
    /// * `size` - encoded size.
    /// * `rect` - source rectangle.
    pub fn set_monitor(&mut self, index: usize, size: Size2, rect: Rect) {
        match index {
            0 => (self.size0, self.rect0) = (size, rect),
            1 => (self.size1, self.rect1) = (size, rect),
            2 => (self.size2, self.rect2) = (size, rect),
            _ => {}
        }
    }

    /// Returns the codec field as a [`VideoCodec`].
    ///
    /// # Returns
    /// The codec.
    pub fn video_codec(&self) -> VideoCodec {
        VideoCodec::from_i32(self.codec)
    }

    /// Returns the descriptor bytes as sent on the wire.
    ///
    /// # Returns
    /// An 88-byte slice.
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }
}

/// Eye-gaze sample received from the headset on the video channel.  The original reads an
/// `int32` length followed by two `Vector2`s (left/right gaze points in normalised `[0,1]`
/// image coordinates) and feeds them to the encoder's dynamic foveation map.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct GazeSample {
    /// Left-eye gaze centre.
    pub left: crate::math::Vec2,
    /// Right-eye gaze centre.
    pub right: crate::math::Vec2,
}

const _: () = {
    assert!(std::mem::size_of::<HapticFeedback>() == 12);
    assert!(std::mem::size_of::<VrFrameInfo>() == 92);
    assert!(std::mem::size_of::<FrameHeader>() == FrameHeader::SIZE);
    assert!(std::mem::offset_of!(FrameHeader, vr) == 0x1c);
    assert!(std::mem::offset_of!(FrameHeader, haptic_left) == 0x78);
    assert!(std::mem::offset_of!(FrameHeader, render_size) == 0x90);
    assert!(std::mem::offset_of!(FrameHeader, payload_size) == 0x98);
    assert!(std::mem::size_of::<SecondaryFrameHeader>() == SecondaryFrameHeader::SIZE);
    assert!(std::mem::size_of::<StreamDescriptor>() == StreamDescriptor::SIZE);
    assert!(std::mem::offset_of!(StreamDescriptor, rect2) == 0x48);
    assert!(std::mem::size_of::<GazeSample>() == 16);
};
