//! Plain-old-data math types matching the memory layout of the Xenko / SharpDX types used by the
//! original (`Xenko.Core.Mathematics.*`, `VirtualDesktop.Interfaces.FovPort`).
//!
//! All types are `#[repr(C)]`, contain only 4-byte scalars and implement [`bytemuck::Pod`], so
//! they can be embedded directly in the wire structs of [`crate::wire`] and
//! [`crate::headset_state`].  JSON field names follow the C# public field names because some of
//! these types are part of the synchronised settings objects.

use bytemuck::{Pod, Zeroable};
use serde::{Deserialize, Serialize};

/// 2-component float vector.  C#: `Xenko.Core.Mathematics.Vector2` (8 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct Vec2 {
    #[serde(rename = "X")]
    pub x: f32,
    #[serde(rename = "Y")]
    pub y: f32,
}

/// 3-component float vector.  C#: `Xenko.Core.Mathematics.Vector3` (12 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct Vec3 {
    #[serde(rename = "X")]
    pub x: f32,
    #[serde(rename = "Y")]
    pub y: f32,
    #[serde(rename = "Z")]
    pub z: f32,
}

/// 4-component float vector.  C#: `Xenko.Core.Mathematics.Vector4` (16 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct Vec4 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

/// Rotation quaternion (x, y, z, w).  C#: `Xenko.Core.Mathematics.Quaternion` (16 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct Quat {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Default for Quat {
    /// Returns the identity rotation (the original initialises poses with `Quaternion.Identity`).
    ///
    /// # Returns
    /// `(0, 0, 0, 1)`.
    fn default() -> Self {
        Self { x: 0.0, y: 0.0, z: 0.0, w: 1.0 }
    }
}

/// Rigid transform.  C#: `Xenko.Core.Mathematics.Pose` = `{ Quaternion Orientation; Vector3 Position; }` (28 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct Pose {
    /// Orientation.
    pub orientation: Quat,
    /// Position in metres.
    pub position: Vec3,
}

/// Integer size.  C#: `Xenko.Core.Mathematics.Size2` (8 bytes).  JSON: `{"Width":..,"Height":..}`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Pod, Zeroable, Serialize, Deserialize)]
pub struct Size2 {
    /// Width in pixels.
    #[serde(rename = "Width")]
    pub width: i32,
    /// Height in pixels.
    #[serde(rename = "Height")]
    pub height: i32,
}

impl Size2 {
    /// Creates a size.
    ///
    /// # Arguments
    /// * `width` - width in pixels.
    /// * `height` - height in pixels.
    ///
    /// # Returns
    /// The size.
    pub const fn new(width: i32, height: i32) -> Self {
        Self { width, height }
    }

    /// Scales the size down (preserving aspect ratio) so that `width × height <= max_pixels`.
    ///
    /// C#: `SharpDX.Size2._0001(int)` extension used by `DesktopStreamer` to clamp a monitor to the
    /// headset's maximum desktop resolution.  Dimensions are rounded down to even numbers because
    /// every hardware encoder requires 4:2:0-compatible sizes.
    ///
    /// # Arguments
    /// * `max_pixels` - pixel budget (`max_width * max_height`).
    ///
    /// # Returns
    /// The (possibly unchanged) clamped size.
    pub fn clamp_to_pixels(self, max_pixels: i64) -> Self {
        let pixels = i64::from(self.width) * i64::from(self.height);
        if pixels <= max_pixels || pixels == 0 {
            return Self::new(self.width & !1, self.height & !1);
        }
        let scale = (max_pixels as f64 / pixels as f64).sqrt();
        let w = ((f64::from(self.width) * scale) as i32) & !1;
        let h = ((f64::from(self.height) * scale) as i32) & !1;
        Self::new(w.max(2), h.max(2))
    }
}

/// Integer point.  C#: `Xenko.Core.Mathematics.Point` (8 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Pod, Zeroable)]
#[allow(missing_docs)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// Integer rectangle.  C#: `Xenko.Core.Mathematics.Rectangle` = `{ X, Y, Width, Height }` (16 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Pod, Zeroable)]
#[allow(missing_docs)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Field of view as tangents of the half angles.  C#: `VirtualDesktop.Interfaces.FovPort` (16 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable, Serialize, Deserialize)]
pub struct FovPort {
    /// Tangent of the upward half angle.
    #[serde(rename = "UpTan")]
    pub up_tan: f32,
    /// Tangent of the downward half angle.
    #[serde(rename = "DownTan")]
    pub down_tan: f32,
    /// Tangent of the leftward half angle.
    #[serde(rename = "LeftTan")]
    pub left_tan: f32,
    /// Tangent of the rightward half angle.
    #[serde(rename = "RightTan")]
    pub right_tan: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_dotnet() {
        assert_eq!(std::mem::size_of::<Pose>(), 28);
        assert_eq!(std::mem::size_of::<FovPort>(), 16);
        assert_eq!(std::mem::size_of::<Rect>(), 16);
    }

    #[test]
    fn clamp_keeps_aspect() {
        let s = Size2::new(3840, 2160).clamp_to_pixels(2560 * 1440);
        assert!(i64::from(s.width) * i64::from(s.height) <= 2560 * 1440);
        assert_eq!(s.width % 2, 0);
    }
}
