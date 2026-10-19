//! The headset → PC state packet (sent on the data channel, TCP 38820, once per headset frame).
//!
//! C#: `_001A._0003` (a `[StructLayout(Sequential)]` class read with `Marshal.PtrToStructure`),
//! 4936 bytes.  It carries the input devices used for desktop control, controller / hand /
//! body / eye / face tracking for VR, and a frame index echoed back in every
//! [`FrameHeader`](crate::wire::FrameHeader).
//!
//! Wire format of one packet (all integers little-endian / native):
//!
//! ```text
//! +-------------------+-------------------------+------------------------------+
//! | compressed len i32| XXH3-64 of decompressed | LZ4 block (compressed len)   |
//! +-------------------+-------------------------+------------------------------+
//! ```
//!
//! Field names were inferred from how the streamer and `VirtualDesktop.Server.exe` use each
//! member; members whose purpose is uncertain keep positional names and say so.

use bytemuck::{Pod, Zeroable};

use crate::error::{ProtocolError, Result};
use crate::math::{Pose, Quat, Vec3};

/// One "pointer" (laser / touch contact) driven by a controller.  C#: `_0003_0002._0003` (10 bytes).
///
/// `VirtualDesktop.Server` turns each of the two pointers into a Windows touch contact at
/// `(x, y)` (`_0008._0002._0001(in, in)`):
/// * `buttons[0] || buttons[1]` — the pointer is in range (hovering); the contact follows `(x, y)`.
/// * `buttons[2] || buttons[3] || buttons[4]` — the contact is down.  `buttons[3]` makes it a
///   secondary (right) press, `buttons[4]` a tertiary (middle) press, otherwise primary (left).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Pod, Zeroable)]
pub struct PointerState {
    /// `[in range, in range, contact, contact (secondary), contact (middle)]`.
    pub buttons: [u8; 5],
    /// Alignment padding.
    pub _pad: u8,
    /// Horizontal position in physical virtual-desktop pixels (the server only divides by its
    /// DPI-virtualisation factor; no monitor origin is added).
    pub x: i16,
    /// Vertical position in physical virtual-desktop pixels.
    pub y: i16,
}

impl PointerState {
    /// Whether the pointer is in range (hovering or touching).
    ///
    /// # Returns
    /// `buttons[0] || buttons[1]`.
    pub fn in_range(&self) -> bool {
        self.buttons[0] != 0 || self.buttons[1] != 0
    }

    /// Whether the contact is down.
    ///
    /// # Returns
    /// `buttons[2] || buttons[3] || buttons[4]`.
    pub fn contact(&self) -> bool {
        self.buttons[2..5].iter().any(|b| *b != 0)
    }
}

/// Eight-key keyboard roll-over state.  C#: `_0083_0002._0004` (explicit layout).
///
/// Each slot is 3 bytes at offsets `0, 3, … 21`: a little-endian `u16` **PC/AT scan code (set 1)**
/// — the high byte is `0xE0` for extended keys — followed by a flags byte
/// (bit 0 = pressed, bit 1 = repeat; C# `_0018._0003`).  The C# struct declares `Size = 24` but
/// its overlapping 4-byte elements make the marshalled size 26; see the module tests.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Pod, Zeroable)]
pub struct KeyboardState {
    /// Raw overlapping slots.
    pub raw: [u8; 26],
}

/// A decoded keyboard slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySlot {
    /// Scan code (set 1); `0xE0xx` for extended keys, `0` for an empty slot.
    pub scan_code: u16,
    /// Slot flags (bit 0 pressed, bit 1 repeat — inferred).
    pub flags: u8,
}

impl KeyboardState {
    /// Number of key slots.
    pub const SLOTS: usize = 8;

    /// Decodes slot `index`.
    ///
    /// # Arguments
    /// * `index` - slot 0..8.
    ///
    /// # Returns
    /// The slot, or `None` if `index` is out of range.
    pub fn slot(&self, index: usize) -> Option<KeySlot> {
        if index >= Self::SLOTS {
            return None;
        }
        let o = index * 3;
        Some(KeySlot { scan_code: u16::from_le_bytes([self.raw[o], self.raw[o + 1]]), flags: self.raw[o + 2] })
    }

    /// Returns every non-empty slot.
    ///
    /// # Returns
    /// Iterator over occupied slots.
    pub fn pressed(&self) -> impl Iterator<Item = KeySlot> + '_ {
        (0..Self::SLOTS).filter_map(|i| self.slot(i)).filter(|s| s.scan_code != 0)
    }

    /// Writes slot `index` (used by tests and by headset emulators).
    ///
    /// # Arguments
    /// * `index` - slot 0..8 (out of range is ignored).
    /// * `slot` - value to store.
    pub fn set_slot(&mut self, index: usize, slot: KeySlot) {
        if index < Self::SLOTS {
            let o = index * 3;
            self.raw[o..o + 2].copy_from_slice(&slot.scan_code.to_le_bytes());
            self.raw[o + 2] = slot.flags;
        }
    }
}

/// Mouse emulation state ("mouse mode" on the headset).  C#: `_0003_0002._0002` (20 bytes).
///
/// The streamer overwrites the whole [`InputState`] in shared memory with every headset packet,
/// and the server's 1 ms injection loop (`_0008._0002._0001()`) then:
/// * ignores motion, presses and the wheel unless `buttons[0]` (mouse mode active) is set, and
///   zeroes `x`/`y` when it is clear; releases are still delivered when it turns off;
/// * edge-triggers `buttons[1..6]` = left, middle, right, X1, X2 against the previous state;
/// * moves the cursor by `round(x), round(y)` pixels **every millisecond** (the values persist
///   until the next packet, so they are a velocity in px/ms), or — when
///   [`ControllerButtons::absolute_mouse`] is set — to the absolute position `(x, y)` in
///   primary-monitor pixels;
/// * injects `wheel_x * 8` (`MOUSEEVENTF_HWHEEL`) and `wheel_y * 8` (`MOUSEEVENTF_WHEEL`) once
///   and zeroes both (they are per-packet deltas).  While a laser pointer is in range they
///   become touch-scroll input instead.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct MouseState {
    /// `[active, left, middle, right, x1, x2]`.
    pub buttons: [u8; 6],
    /// Alignment padding.
    pub _pad0: [u8; 2],
    /// Horizontal velocity (px/ms) or absolute X.
    pub x: f32,
    /// Vertical velocity (px/ms, positive = down) or absolute Y.
    pub y: f32,
    /// Horizontal wheel delta in 1/15 notch units (× 8 = `WHEEL_DELTA` units; positive = right).
    pub wheel_x: i8,
    /// Vertical wheel delta in 1/15 notch units (× 8 = `WHEEL_DELTA` units; positive = up).
    pub wheel_y: i8,
    /// Alignment padding.
    pub _pad1: [u8; 2],
}

impl MouseState {
    /// Index of the "mouse mode active" flag in [`MouseState::buttons`].
    pub const ACTIVE: usize = 0;

    /// Whether mouse mode is active.
    ///
    /// # Returns
    /// `buttons[0] != 0`.
    pub fn active(&self) -> bool {
        self.buttons[Self::ACTIVE] != 0
    }
}

/// Xbox-style gamepad state.  C#: `_0098_0002._0002` (14 bytes) — identical to `XINPUT_GAMEPAD`
/// with an extra trailing byte.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Pod, Zeroable)]
pub struct GamepadState {
    /// [`crate::types::gamepad_buttons`] bit set.
    pub buttons: u16,
    /// Left trigger 0..255.
    pub left_trigger: u8,
    /// Right trigger 0..255.
    pub right_trigger: u8,
    /// Left stick X (−32768..32767).
    pub thumb_lx: i16,
    /// Left stick Y.
    pub thumb_ly: i16,
    /// Right stick X.
    pub thumb_rx: i16,
    /// Right stick Y.
    pub thumb_ry: i16,
    /// Extra byte (`_0003`): presence / user index — not needed for injection.
    pub extra: u8,
    /// Alignment padding.
    pub _pad: u8,
}

/// VR controller buttons for controller-to-gamepad emulation.  C#: `_0003_0002._0001` (18 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Pod, Zeroable)]
pub struct ControllerButtons {
    /// [`crate::types::controller_buttons`] bit set.
    pub buttons: u16,
    /// [`crate::types::controller_touch`] bit set.
    pub touches: u16,
    /// Analog values 0..255: `[left trigger, right trigger, left grip, right grip]` (inferred order).
    pub analog: [u8; 4],
    /// Thumbsticks `[lx, ly, rx, ry]`.
    pub thumbsticks: [i16; 4],
    /// Boolean `_0001`: the server treats [`MouseState::x`]/[`MouseState::y`] as an absolute
    /// position instead of a velocity while it is set.
    pub absolute_mouse: u8,
    /// Alignment padding.
    pub _pad: u8,
}

/// Every input device of the headset.  C#: `_001F._0004` (104 bytes).  This is also the first
/// member of the `VirtualDesktop.InputInjection` shared memory read by the injection service.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct InputState {
    /// Two pointers (left / right controller).
    pub pointers: [PointerState; 2],
    /// Keyboard roll-over state.
    pub keyboard: KeyboardState,
    /// Alignment padding.
    pub _pad0: [u8; 2],
    /// Mouse / wheel.
    pub mouse: MouseState,
    /// Emulated gamepad.
    pub gamepad: GamepadState,
    /// Raw controller buttons.
    pub controllers: ControllerButtons,
    /// [`crate::types::GamepadModel`] byte.
    pub gamepad_model: u8,
    /// Alignment padding.
    pub _pad1: [u8; 3],
}

/// Tracked VR controller pose.  C#: `_009E._0002` (68 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct ControllerPose {
    /// Orientation.
    pub orientation: Quat,
    /// Position.
    pub position: Vec3,
    /// Linear velocity.
    pub linear_velocity: Vec3,
    /// Angular velocity.
    pub angular_velocity: Vec3,
    /// Fourth vector (`_0004`; acceleration in the OVR pose-state layout this mirrors).
    pub linear_acceleration: Vec3,
    /// Tracking flags (`_0010_0003._0002`: bit 0 orientation valid, bit 1 position valid).
    pub tracking_flags: u8,
    /// Signed byte (`_0001`): battery percentage (inferred).
    pub battery: i8,
    /// Alignment padding.
    pub _pad: [u8; 2],
}

/// One hand-skeleton joint.  C#: `_009E_0002._0003` (56 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct HandJoint {
    /// Joint pose.
    pub pose: Pose,
    /// Joint radius in metres.
    pub radius: f32,
    /// Linear velocity.
    pub linear_velocity: Vec3,
    /// Angular velocity.
    pub angular_velocity: Vec3,
}

/// Hand aim / pinch state.  C#: `_000E._0006` (56 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct HandAim {
    /// Status bits (`int64`).
    pub status: i64,
    /// Aim pose.
    pub pose: Pose,
    /// Pinch strengths `[index, middle, ring, little]` (inferred).
    pub pinch: [f32; 4],
    /// Trailing alignment padding.
    pub _pad: [u8; 4],
}

/// Hand tracking.  C#: `_001E_0002._0002` (3032 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct HandsState {
    /// Left hand tracked.
    pub left_active: u8,
    /// Right hand tracked.
    pub right_active: u8,
    /// Alignment padding.
    pub _pad0: [u8; 2],
    /// Left hand joints (OpenXR `XR_HAND_JOINT_COUNT_EXT` = 26).
    pub left_joints: [HandJoint; 26],
    /// Right hand joints.
    pub right_joints: [HandJoint; 26],
    /// Alignment padding.
    pub _pad1: [u8; 4],
    /// Left aim state.
    pub left_aim: HandAim,
    /// Right aim state.
    pub right_aim: HandAim,
}

/// One body-tracking joint.  C#: `_0001_0003._0001` (40 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct BodyJoint {
    /// Location / validity flags (`ulong`).
    pub flags: u64,
    /// Joint pose.
    pub pose: Pose,
    /// Trailing alignment padding.
    pub _pad: [u8; 4],
}

/// Body tracking.  C#: `_009D._0002` (1288 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct BodyState {
    /// Body tracking active.
    pub active: u8,
    /// Second flag (`_0002`).
    pub flag: u8,
    /// Alignment padding.
    pub _pad: [u8; 2],
    /// Overall confidence.
    pub confidence: f32,
    /// 32 joints.
    pub joints: [BodyJoint; 32],
}

/// Eye tracking.  C#: `_0095_0002._0003` (68 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct EyesState {
    /// Left eye valid.
    pub left_valid: u8,
    /// Right eye valid.
    pub right_valid: u8,
    /// Alignment padding.
    pub _pad: [u8; 2],
    /// Left gaze pose.
    pub left: Pose,
    /// Right gaze pose.
    pub right: Pose,
    /// Left confidence.
    pub left_confidence: f32,
    /// Right confidence.
    pub right_confidence: f32,
}

/// Face tracking.  C#: `_0095._0002` (292 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct FaceState {
    /// Face tracking active.
    pub active: u8,
    /// Second flag.
    pub flag: u8,
    /// Alignment padding.
    pub _pad: [u8; 2],
    /// 70 expression weights (Meta `XR_FB_face_tracking2` order; see `FaceExpressions` in the original).
    pub weights: [f32; 70],
    /// Two confidence values (lower / upper face).
    pub confidence: [f32; 2],
}

/// The complete headset state.  C#: `_001A._0003`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct HeadsetState {
    /// Monotonic headset frame index, echoed in [`crate::wire::FrameHeader::ack_frame_index`].
    pub frame_index: i32,
    /// Float `_0001` (timing value, inferred: display time / latency).
    pub value_a: f32,
    /// Float `_0002` (timing value, inferred).
    pub value_b: f32,
    /// Input devices.
    pub input: InputState,
    /// Tracking-space flags (`_0011_0003._0004`: 0, 1, 2, 4).
    pub tracking_flags: u8,
    /// Alignment padding.
    pub _pad0: [u8; 3],
    /// Left and right controller poses.
    pub controllers: [ControllerPose; 2],
    /// Hand tracking.
    pub hands: HandsState,
    /// Body tracking.
    pub body: BodyState,
    /// Eye tracking.
    pub eyes: EyesState,
    /// Face tracking.
    pub face: FaceState,
}

impl HeadsetState {
    /// Size of the struct as computed from the decompiled layout.
    pub const SIZE: usize = 4936;

    /// Size of the 12-byte packet header (`i32` compressed length + `u64` XXH3).
    pub const PACKET_HEADER: usize = 12;

    /// Returns an all-zero state with identity rotations, as the C# constructor does.
    ///
    /// # Returns
    /// The default state.
    pub fn new() -> Self {
        let mut s: Self = Zeroable::zeroed();
        for c in &mut s.controllers {
            c.orientation = Quat::default();
        }
        s
    }

    /// Decodes a state from decompressed bytes.
    ///
    /// Shorter input (an older headset with a smaller struct) is zero-extended and longer input
    /// is truncated, mirroring `Marshal.PtrToStructure` over a fixed-size buffer.
    ///
    /// # Arguments
    /// * `bytes` - decompressed struct bytes.
    ///
    /// # Returns
    /// The decoded state.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut buf = [0u8; Self::SIZE];
        let n = bytes.len().min(Self::SIZE);
        buf[..n].copy_from_slice(&bytes[..n]);
        bytemuck::pod_read_unaligned(&buf)
    }

    /// Returns the state as raw bytes.
    ///
    /// # Returns
    /// A 4936-byte slice.
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }

    /// Decodes one packet body (everything after the 12-byte header).
    ///
    /// # Arguments
    /// * `expected_hash` - XXH3-64 from the packet header.
    /// * `compressed` - LZ4 block.
    ///
    /// # Returns
    /// `(state, decompressed length)`; a decompressed length other than [`HeadsetState::SIZE`]
    /// indicates a headset build with a different struct layout and should be logged.
    ///
    /// # Errors
    /// * [`ProtocolError::Lz4`] if decompression fails.
    /// * [`ProtocolError::Integrity`] if the XXH3 hash does not match (the original throws `IOException`).
    pub fn decode_packet(expected_hash: u64, compressed: &[u8]) -> Result<(Self, usize)> {
        let mut out = vec![0u8; Self::SIZE * 2];
        let n = lz4_flex::block::decompress_into(compressed, &mut out)?;
        if xxhash_rust::xxh3::xxh3_64(&out[..n]) != expected_hash {
            return Err(ProtocolError::Integrity("headset state XXH3 mismatch"));
        }
        Ok((Self::from_bytes(&out[..n]), n))
    }

    /// Parses the 12-byte packet header.
    ///
    /// # Arguments
    /// * `header` - the first 12 bytes of a packet.
    ///
    /// # Returns
    /// `(compressed length, XXH3-64)`.
    ///
    /// # Errors
    /// [`ProtocolError::InvalidLength`] for negative or oversized lengths (the original throws
    /// `IOException` when the length exceeds its receive buffer).
    pub fn parse_packet_header(header: &[u8; 12]) -> Result<(usize, u64)> {
        let len = i32::from_le_bytes(header[0..4].try_into().expect("4 bytes"));
        let hash = u64::from_le_bytes(header[4..12].try_into().expect("8 bytes"));
        if len < 0 || len as usize > lz4_flex::block::get_maximum_output_size(Self::SIZE * 2) {
            return Err(ProtocolError::InvalidLength { what: "headset state packet", length: i64::from(len) });
        }
        Ok((len as usize, hash))
    }

    /// Encodes a complete packet (header + LZ4 block); used by tests and headset emulators.
    ///
    /// # Returns
    /// The packet bytes.
    pub fn encode_packet(&self) -> Vec<u8> {
        let raw = self.as_bytes();
        let compressed = lz4_flex::block::compress(raw);
        let mut out = Vec::with_capacity(Self::PACKET_HEADER + compressed.len());
        out.extend_from_slice(&(compressed.len() as i32).to_le_bytes());
        out.extend_from_slice(&xxhash_rust::xxh3::xxh3_64(raw).to_le_bytes());
        out.extend_from_slice(&compressed);
        out
    }
}

impl Default for HeadsetState {
    /// See [`HeadsetState::new`].
    ///
    /// # Returns
    /// The default state.
    fn default() -> Self {
        Self::new()
    }
}

/// Layout of the `VirtualDesktop.InputInjection` shared memory.  C#: `_0012_0002._0001` (108 bytes).
///
/// The streamer copies [`InputState`] in; the injection service writes the gamepad rumble motor
/// speeds back into `rumble`, which the streamer forwards to the headset as haptics.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct InputInjectionShared {
    /// Latest input state.
    pub input: InputState,
    /// `[left motor, right motor]` rumble speed 0..255 (C# `_0005_0003._0002`).
    pub rumble: [u8; 2],
    /// Trailing alignment padding.
    pub _pad: [u8; 2],
}

const _: () = {
    assert!(std::mem::size_of::<PointerState>() == 10);
    assert!(std::mem::size_of::<MouseState>() == 20);
    assert!(std::mem::size_of::<GamepadState>() == 14);
    assert!(std::mem::size_of::<ControllerButtons>() == 18);
    assert!(std::mem::size_of::<InputState>() == 104);
    assert!(std::mem::offset_of!(InputState, mouse) == 0x30);
    assert!(std::mem::offset_of!(InputState, gamepad) == 0x44);
    assert!(std::mem::offset_of!(InputState, controllers) == 0x52);
    assert!(std::mem::size_of::<ControllerPose>() == 68);
    assert!(std::mem::size_of::<HandsState>() == 3032);
    assert!(std::mem::size_of::<BodyState>() == 1288);
    assert!(std::mem::size_of::<EyesState>() == 68);
    assert!(std::mem::size_of::<FaceState>() == 292);
    assert!(std::mem::offset_of!(HeadsetState, controllers) == 0x78);
    assert!(std::mem::offset_of!(HeadsetState, hands) == 0x100);
    assert!(std::mem::offset_of!(HeadsetState, body) == 0xcd8);
    assert!(std::mem::offset_of!(HeadsetState, eyes) == 0x11e0);
    assert!(std::mem::offset_of!(HeadsetState, face) == 0x1224);
    assert!(std::mem::size_of::<HeadsetState>() == HeadsetState::SIZE);
    assert!(std::mem::size_of::<InputInjectionShared>() == 108);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_round_trip() {
        let mut s = HeadsetState::new();
        s.frame_index = 42;
        s.input.keyboard.set_slot(0, KeySlot { scan_code: 0x1E, flags: 1 });
        let pkt = s.encode_packet();
        let header: [u8; 12] = pkt[..12].try_into().unwrap();
        let (len, hash) = HeadsetState::parse_packet_header(&header).unwrap();
        let (decoded, n) = HeadsetState::decode_packet(hash, &pkt[12..12 + len]).unwrap();
        assert_eq!(n, HeadsetState::SIZE);
        assert_eq!(decoded.frame_index, 42);
        assert_eq!(decoded.input.keyboard.pressed().next().unwrap().scan_code, 0x1E);
    }

    #[test]
    fn corrupted_packet_is_rejected() {
        let pkt = HeadsetState::new().encode_packet();
        let (len, hash) = HeadsetState::parse_packet_header(&pkt[..12].try_into().unwrap()).unwrap();
        assert!(HeadsetState::decode_packet(hash ^ 1, &pkt[12..12 + len]).is_err());
    }
}
