//! Per-device capability tables.
//!
//! C#: extension methods on `Xenko.VR.HmdType` in `_008B._0001` and `_0006_0002._0007`, plus the
//! bitrate helpers of `SharedMobileSettings`.  The original's control flow was flattened by the
//! obfuscator; the tables below were reconstructed branch by branch from the raw decompilation.

use crate::types::{HmdType, VRGraphicsQuality, VideoCodec};

/// Reports whether the client is a flat (non-VR) device: phone, tablet, TV or Android desktop.
///
/// C#: `_008B._0001._0001(this HmdType) : bool` — `Unknown < hmd < Quest`.
///
/// # Arguments
/// * `hmd` - device type.
///
/// # Returns
/// `true` for iPhone/iPad/Apple TV/Android phone/tablet/TV/desktop.
pub fn is_flat_device(hmd: HmdType) -> bool {
    hmd > HmdType::UNKNOWN && hmd < HmdType::QUEST
}

/// Reports whether the device supports a 72 Hz default VR refresh (all but Vive Focus 3 / XR Elite).
///
/// C#: `_0006_0002._0007._0001(this HmdType)`, used by `SharedMobileSettings.VRFramerate`
/// (`Default` → 72 Hz when `true`, 90 Hz otherwise).
///
/// # Arguments
/// * `hmd` - device type.
///
/// # Returns
/// `false` only for the two HTC headsets.
pub fn defaults_to_72hz(hmd: HmdType) -> bool {
    hmd != HmdType::VIVE_FOCUS3 && hmd != HmdType::VIVE_XR_ELITE
}

/// Effective VR frame rate for a `Framerate` user preference.
///
/// # Arguments
/// * `hmd` - device type.
/// * `preference` - `SharedUserSettings.VRFramerate` as an integer (0 = default).
///
/// # Returns
/// Frames per second.
pub fn vr_framerate(hmd: HmdType, preference: i32) -> i32 {
    if preference != 0 {
        preference
    } else if defaults_to_72hz(hmd) {
        72
    } else {
        90
    }
}

/// Reports whether the device is the XREAL Aura (`_008B._0001._0002`, special-cased by the VR path).
///
/// # Arguments
/// * `hmd` - device type.
///
/// # Returns
/// `true` for [`HmdType::XREAL_AURA`].
pub fn is_xreal(hmd: HmdType) -> bool {
    hmd == HmdType::XREAL_AURA
}

/// "Optimal" virtual-monitor resolution used when `SharedUserSettings.UseOptimalResolution` is set.
///
/// C#: `SharedMobileSettings.GetOptimalDesktopResolution` → flat devices report their own
/// `Resolution`; headsets use `_008B._0001._0001(this HmdType, out int, out int)`.
///
/// # Arguments
/// * `hmd` - device type.
/// * `reported` - `SharedMobileSettings.Resolution` (used for flat devices).
///
/// # Returns
/// `(width, height)`.
pub fn optimal_desktop_resolution(hmd: HmdType, reported: (i32, i32)) -> (i32, i32) {
    if is_flat_device(hmd) {
        return reported;
    }
    match hmd {
        HmdType::IPHONE | HmdType::ANDROID_PHONE => (1280, 720),
        HmdType::IPAD => (1920, 1440),
        HmdType::ANDROID_TABLET => (1920, 1200),
        HmdType::APPLE_TV | HmdType::ANDROID_TV | HmdType::ANDROID_DESKTOP => (1920, 1080),
        HmdType::QUEST3
        | HmdType::QUEST3S
        | HmdType::PICO4_ULTRA
        | HmdType::PICO_1011
        | HmdType::GALAXY_XR
        | HmdType::XREAL_AURA
        | HmdType::PLAY_FOR_DREAM_MR
        | HmdType::STEAM_FRAME => (2560, 1440),
        _ => (1920, 1080),
    }
}

/// Maximum resolution of each streamed monitor.
///
/// C#: `SharedMobileSettings.GetMaxDesktopResolution(monitorCount)` →
/// `_008B._0001._0001(this HmdType, int, out int, out int)`.
///
/// # Arguments
/// * `hmd` - device type.
/// * `monitor_count` - number of monitors streamed simultaneously.
///
/// # Returns
/// `(width, height)`.
pub fn max_desktop_resolution(hmd: HmdType, monitor_count: i32) -> (i32, i32) {
    let multi = monitor_count > 1;
    match hmd {
        HmdType::QUEST => (1920, 1080),
        HmdType::QUEST3
        | HmdType::QUEST3S
        | HmdType::PICO4_ULTRA
        | HmdType::PICO_1011
        | HmdType::GALAXY_XR
        | HmdType::XREAL_AURA
        | HmdType::PLAY_FOR_DREAM_MR
        | HmdType::STEAM_FRAME => {
            if multi {
                (2560, 1440)
            } else {
                (3840, 2160)
            }
        }
        HmdType::IPHONE | HmdType::ANDROID_PHONE => (2560, 1440),
        HmdType::IPAD | HmdType::APPLE_TV | HmdType::ANDROID_TABLET | HmdType::ANDROID_TV | HmdType::ANDROID_DESKTOP => (3840, 2160),
        _ => {
            if multi {
                (1920, 1080)
            } else {
                (2560, 1440)
            }
        }
    }
}

/// Maximum resolution of a passed-through video file.
///
/// C#: `_008B._0001._0002(this HmdType, int framerate, out int, out int)`.
///
/// # Arguments
/// * `hmd` - device type.
///
/// # Returns
/// `(width, height)`: 5760×2880 up to the original Quest, 8192×4096 for newer devices.
pub fn max_video_resolution(hmd: HmdType) -> (i32, i32) {
    if hmd <= HmdType::QUEST { (5760, 2880) } else { (8192, 4096) }
}

/// Maximum number of simultaneously streamed monitors.
///
/// C#: `_008B._0001._0001(this HmdType) : int`.
///
/// # Arguments
/// * `hmd` - device type.
///
/// # Returns
/// 1, 2 or 3.
pub fn max_monitors(hmd: HmdType) -> i32 {
    if is_flat_device(hmd) || hmd == HmdType::QUEST {
        return 1;
    }
    match hmd {
        HmdType::QUEST3
        | HmdType::QUEST3S
        | HmdType::PICO4_ULTRA
        | HmdType::PICO_1011
        | HmdType::GALAXY_XR
        | HmdType::XREAL_AURA
        | HmdType::PLAY_FOR_DREAM_MR
        | HmdType::STEAM_FRAME => 3,
        _ => 2,
    }
}

/// Linear interpolation used by the bitrate curves.  C#: `_008B._0001._0001(float, float, float)`.
///
/// # Arguments
/// * `min` - value at `t = 0`.
/// * `max` - value at `t = 1`.
/// * `t` - position.
///
/// # Returns
/// `(1 - t) * min + t * max`.
pub fn lerp(min: f32, max: f32, t: f32) -> f32 {
    (1.0 - t) * min + t * max
}

/// Rounds to three decimals, away from zero (`Math.Round(x, 3, MidpointRounding.AwayFromZero)`).
///
/// # Arguments
/// * `v` - value.
///
/// # Returns
/// The rounded value.
pub fn round3(v: f32) -> f32 {
    let scaled = f64::from(v) * 1000.0;
    (scaled.abs().round().copysign(scaled) / 1000.0) as f32
}

/// Minimum desktop bitrate (bit/s).  C#: `_008B._0001._0002(this HmdType)`.
///
/// # Arguments
/// * `hmd` - device type.
///
/// # Returns
/// 2 Mbit/s for flat devices, 4 Mbit/s for headsets.
pub fn min_desktop_bitrate(hmd: HmdType) -> i32 {
    if is_flat_device(hmd) { 2_000_000 } else { 4_000_000 }
}

/// Maximum desktop bitrate (bit/s).  C#: `_008B._0001._0003(this HmdType)`.
///
/// # Arguments
/// * `hmd` - device type.
///
/// # Returns
/// 40 Mbit/s for the original Quest, 120 Mbit/s otherwise.
pub fn max_desktop_bitrate(hmd: HmdType) -> i32 {
    if hmd == HmdType::QUEST { 40_000_000 } else { 120_000_000 }
}

/// Desktop bitrate for a bitrate-limit slider value.  C#: `_008B._0001._0001(this HmdType, float)`.
///
/// # Arguments
/// * `hmd` - device type.
/// * `limit` - `SharedUserSettings.DesktopBitrateLimit` in `[0, 1]`.
///
/// # Returns
/// Bitrate in bit/s.
pub fn desktop_bitrate_for_limit(hmd: HmdType, limit: f32) -> i32 {
    lerp(min_desktop_bitrate(hmd) as f32, max_desktop_bitrate(hmd) as f32, round3(limit)) as i32
}

/// Effective desktop bitrate.  C#: `SharedMobileSettings.UpdateDesktopBitrate`.
///
/// # Arguments
/// * `hmd` - device type.
/// * `limit` - `SharedUserSettings.DesktopBitrateLimit`.
/// * `measured_bandwidth` - `SharedMobileSettings.MeasuredBandwidth` (bit/s).
///
/// # Returns
/// `min(measured, curve(limit))`.
pub fn desktop_bitrate(hmd: HmdType, limit: f32, measured_bandwidth: i32) -> i32 {
    measured_bandwidth.min(desktop_bitrate_for_limit(hmd, limit))
}

/// Minimum VR bitrate (bit/s).  C#: `_008B._0001._0004(this HmdType)` = 10 Mbit/s.
///
/// # Arguments
/// * `_hmd` - device type (unused by the original).
///
/// # Returns
/// 10 000 000.
pub fn min_vr_bitrate(_hmd: HmdType) -> i32 {
    10_000_000
}

/// Maximum VR bitrate (bit/s) per codec.  C#: `_008B._0001._0001(this HmdType, VideoCodec)`.
///
/// # Arguments
/// * `hmd` - device type.
/// * `codec` - `H264Plus` uses its own, higher ceiling.
///
/// # Returns
/// The ceiling.
pub fn max_vr_bitrate(hmd: HmdType, codec: VideoCodec) -> i32 {
    let plus = codec == VideoCodec::H264Plus;
    match hmd {
        HmdType::QUEST => 100_000_000,
        HmdType::QUEST2 | HmdType::QUEST_PRO | HmdType::VIVE_FOCUS3 | HmdType::VIVE_XR_ELITE | HmdType::PICO_NEO3 | HmdType::PICO_NEO3_LINK | HmdType::PICO4 => {
            if plus { 400_000_000 } else { 150_000_000 }
        }
        HmdType::QUEST3 | HmdType::QUEST3S => {
            if plus { 500_000_000 } else { 200_000_000 }
        }
        HmdType::PICO4_ULTRA | HmdType::PICO_1011 | HmdType::GALAXY_XR | HmdType::XREAL_AURA | HmdType::PLAY_FOR_DREAM_MR | HmdType::STEAM_FRAME => {
            if plus { 600_000_000 } else { 200_000_000 }
        }
        _ => 40_000_000,
    }
}

/// Effective VR bitrate.  C#: `SharedMobileSettings.UpdateVRBitrates`.
///
/// # Arguments
/// * `hmd` - device type.
/// * `limit` - `SharedUserSettings.VRBitrateLimit`.
/// * `codec` - codec family (`HEVC` for the default curve, `H264Plus`, `AV1`).
/// * `measured_bandwidth` - measured bandwidth (bit/s).
///
/// # Returns
/// `min(measured, lerp(min, max(codec), round3(limit)))`.
pub fn vr_bitrate(hmd: HmdType, limit: f32, codec: VideoCodec, measured_bandwidth: i32) -> i32 {
    let curve = lerp(min_vr_bitrate(hmd) as f32, max_vr_bitrate(hmd, codec) as f32, round3(limit)) as i32;
    measured_bandwidth.min(curve)
}

/// Maximum supported bandwidth of the link (`_008B._0001._0005`): 85 Mbit/s for the original
/// Quest, 200 Mbit/s otherwise.
///
/// # Arguments
/// * `hmd` - device type.
///
/// # Returns
/// Bit/s.
pub fn max_link_bandwidth(hmd: HmdType) -> i32 {
    if hmd == HmdType::QUEST { 85_000_000 } else { 200_000_000 }
}

/// Per-eye VR render resolution for a quality preset (`_008B._0001._0001(this HmdType, VRGraphicsQuality, out, out)`).
///
/// Only the presets that could be reconstructed unambiguously are tabulated; other headsets fall
/// back to the Quest 2 table.  The VR path is not ported yet (see `docs/REVERSE_ENGINEERING.md`).
///
/// # Arguments
/// * `hmd` - device type.
/// * `quality` - preset.
///
/// # Returns
/// `(width, height)` per eye.
pub fn vr_render_resolution(hmd: HmdType, quality: VRGraphicsQuality) -> (i32, i32) {
    use VRGraphicsQuality::*;
    if hmd == HmdType::QUEST {
        return match quality {
            Potato | Low => (1536, 1728),
            Medium => (1824, 2016),
            _ => (2208, 2400),
        };
    }
    if hmd == HmdType::XREAL_AURA {
        return match quality {
            Potato | Low => (1664, 1024),
            Medium | High => (2240, 1408),
            Ultra => (2624, 1664),
            Godlike => (3072, 1920),
            Monster => (3648, 2304),
        };
    }
    if hmd == HmdType::PLAY_FOR_DREAM_MR {
        return match quality {
            Potato | Low => (1824, 1728),
            Medium => (2208, 2016),
            High => (2688, 2496),
            _ => (2880, 2688),
        };
    }
    match quality {
        Potato => (1344, 1440),
        Low | Medium => (2112, 2304),
        High | Ultra => (2688, 2880),
        Godlike | Monster => (3648, 3936),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_desktop_bitrate_quest3() {
        // DesktopBitrateLimit default 0.17 -> 4M + 0.17 * 116M = 23.72 Mbit/s.
        assert!((desktop_bitrate_for_limit(HmdType::QUEST3, 0.17) - 23_720_000).abs() < 64);
        assert_eq!(desktop_bitrate(HmdType::QUEST3, 0.17, 10_000_000), 10_000_000);
    }

    #[test]
    fn monitor_tables() {
        assert_eq!(max_monitors(HmdType::QUEST3), 3);
        assert_eq!(max_monitors(HmdType::QUEST2), 2);
        assert_eq!(max_monitors(HmdType::IPHONE), 1);
        assert_eq!(max_desktop_resolution(HmdType::QUEST3, 1), (3840, 2160));
        assert_eq!(max_desktop_resolution(HmdType::QUEST2, 2), (1920, 1080));
    }
}
