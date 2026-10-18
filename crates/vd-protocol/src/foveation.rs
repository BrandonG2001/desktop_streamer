//! Encoder QP-delta ("emphasis") maps for foveated encoding.
//!
//! C#: `_008B._0003._0002(VideoCodec)` (static map, built when the NVENC encoder is created for
//! the VR source) and `_008B._0003._0006()` (dynamic map re-centred on the eye-gaze point).
//!
//! The picture is divided into macro blocks (16 px for H.264, 32 px for HEVC CTUs, 64 or 128 px
//! for AV1 super-blocks, read from the sequence header).  For each block the normalised distance
//! `d` from the focus point (relative to half the block-grid diagonal) selects a QP offset:
//!
//! * static map:  `d <= 0.7` → −6, otherwise `−6 + (d − 0.7) / 0.3 × 12`
//! * gaze map:    `d <= 0.6` → −6, otherwise `−6 + (d − 0.6) / 0.4 × 12`
//!
//! rounded away from zero and clamped to `[-31, 31]`.  Negative values raise quality in the
//! fovea; positive values (up to +6 at the corners) save bits in the periphery.  The same map can
//! be fed to FFmpeg encoders that accept per-block QP offsets (e.g. `AV_FRAME_DATA_REGIONS_OF_INTEREST`).

use crate::types::VideoCodec;

/// Macro-block size the encoder's QP map is expressed in.
///
/// # Arguments
/// * `codec` - output codec.
/// * `av1_superblock_128` - for AV1, whether the sequence header selects 128×128 super-blocks.
///
/// # Returns
/// Block edge length in pixels.
pub fn block_size(codec: VideoCodec, av1_superblock_128: bool) -> u32 {
    if codec.is_h264() {
        16
    } else if codec.is_hevc() {
        32
    } else if av1_superblock_128 {
        128
    } else {
        64
    }
}

/// A QP-delta map in raster order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QpMap {
    /// Blocks per row.
    pub cols: u32,
    /// Block rows.
    pub rows: u32,
    /// Block size in pixels.
    pub block: u32,
    /// `cols * rows` signed QP offsets.
    pub deltas: Vec<i8>,
}

fn delta_for(distance: f32, inner: f32) -> i8 {
    let v = if distance <= inner { -6.0 } else { -6.0 + (distance - inner) / (1.0 - inner) * 12.0 };
    let rounded = v.abs().round().copysign(v);
    rounded.clamp(-31.0, 31.0) as i8
}

fn build(width: u32, height: u32, block: u32, center: (f32, f32), inner: f32) -> QpMap {
    let cols = width.div_ceil(block).max(1);
    let rows = height.div_ceil(block).max(1);
    let radius = (((cols * cols + rows * rows) as f32).sqrt() / 2.0).max(f32::EPSILON);
    let (cx, cy) = (center.0 * cols as f32, center.1 * rows as f32);
    let mut deltas = Vec::with_capacity((cols * rows) as usize);
    for y in 0..rows {
        for x in 0..cols {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            deltas.push(delta_for((dx * dx + dy * dy).sqrt() / radius, inner));
        }
    }
    QpMap { cols, rows, block, deltas }
}

/// Builds the static, image-centred foveation map.
///
/// # Arguments
/// * `width` / `height` - encoded picture size in pixels.
/// * `block` - block size from [`block_size`].
///
/// # Returns
/// The map.
pub fn static_map(width: u32, height: u32, block: u32) -> QpMap {
    build(width, height, block, (0.5, 0.5), 0.7)
}

/// Builds the gaze-centred foveation map.
///
/// # Arguments
/// * `width` / `height` - encoded picture size in pixels.
/// * `block` - block size from [`block_size`].
/// * `gaze` - focus point in normalised `[0, 1]` picture coordinates.
///
/// # Returns
/// The map.
pub fn gaze_map(width: u32, height: u32, block: u32, gaze: (f32, f32)) -> QpMap {
    build(width, height, block, (gaze.0.clamp(0.0, 1.0), gaze.1.clamp(0.0, 1.0)), 0.6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centre_is_sharper_than_corners() {
        let m = static_map(1920, 1080, 16);
        let centre = m.deltas[(m.rows / 2 * m.cols + m.cols / 2) as usize];
        assert_eq!(centre, -6);
        assert!(m.deltas[0] > 0);
    }
}
