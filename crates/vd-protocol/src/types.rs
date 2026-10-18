//! Enumerations shared by the protocol, the settings objects and the streamer.
//!
//! Newtonsoft.Json serialises .NET enums as **integers** (no `StringEnumConverter` is
//! registered; many enum members are obfuscated so names could not round-trip anyway), so every
//! enum here (de)serialises as its numeric value.  Enums whose value set grows with new headset
//! releases ([`HmdType`], [`Features`]) are modelled as transparent integer newtypes so that
//! unknown values coming from a newer headset app never fail deserialisation.

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

/// What the headset is currently viewing.  C#: `VirtualDesktop.Net.StreamingSource : byte`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum StreamingSource {
    /// The PC desktop (monitor capture).
    #[default]
    Desktop = 0,
    /// A VR application (frames come from the OpenVR/OpenXR/LibOVR drivers).
    VR = 1,
    /// A video file played by the streamer and passed through to the headset decoder.
    Video = 2,
    /// A video stored on the headset itself.
    LocalVideo = 3,
}

impl StreamingSource {
    /// Reports whether the source is streamed by the PC (`Desktop`, `VR` or `Video`).
    ///
    /// C#: extension `_0002()` used by `LoopbackCapture` to decide whether to capture audio.
    ///
    /// # Returns
    /// `true` unless the headset is playing a local video.
    pub fn is_pc_streamed(self) -> bool {
        self != StreamingSource::LocalVideo
    }

    /// Reports whether this is the VR source.
    ///
    /// # Returns
    /// `true` for [`StreamingSource::VR`].
    pub fn is_vr(self) -> bool {
        self == StreamingSource::VR
    }

    /// Converts the wire byte into a source.
    ///
    /// # Arguments
    /// * `v` - raw value.
    ///
    /// # Returns
    /// The source; unknown values map to `Desktop`.
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::VR,
            2 => Self::Video,
            3 => Self::LocalVideo,
            _ => Self::Desktop,
        }
    }
}

/// Video codec selection.  C#: `VirtualDesktop.Net.VideoCodec : int`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
pub enum VideoCodec {
    /// Let the encoder pick (HEVC 10-bit when supported, otherwise H.264).
    #[default]
    Automatic = 0,
    /// H.264 / AVC.
    H264 = 1,
    /// H.265 / HEVC 8-bit.
    HEVC = 2,
    /// VP8 (declared, never produced by the shipped encoders).
    VP8 = 3,
    /// VP9 (declared, never produced by the shipped encoders).
    VP9 = 4,
    /// "H.264+" — H.264 at very high bitrate using a separate bitrate budget.
    H264Plus = 5,
    /// HEVC Main10.
    HEVC10bit = 6,
    /// AV1 8-bit.
    AV1 = 10,
    /// AV1 10-bit.
    AV110bit = 11,
}

impl VideoCodec {
    /// Converts a raw integer into a codec.
    ///
    /// # Arguments
    /// * `v` - raw value.
    ///
    /// # Returns
    /// The codec; unknown values map to `Automatic`.
    pub fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::H264,
            2 => Self::HEVC,
            3 => Self::VP8,
            4 => Self::VP9,
            5 => Self::H264Plus,
            6 => Self::HEVC10bit,
            10 => Self::AV1,
            11 => Self::AV110bit,
            _ => Self::Automatic,
        }
    }

    /// Reports whether this is one of the H.264 variants.  C#: extension `_0001()`.
    ///
    /// # Returns
    /// `true` for `H264` and `H264Plus`.
    pub fn is_h264(self) -> bool {
        matches!(self, Self::H264 | Self::H264Plus)
    }

    /// Reports whether this is one of the HEVC variants.  C#: extension `_0002()`.
    ///
    /// # Returns
    /// `true` for `HEVC` and `HEVC10bit`.
    pub fn is_hevc(self) -> bool {
        matches!(self, Self::HEVC | Self::HEVC10bit)
    }

    /// Reports whether this is one of the AV1 variants.  C#: extension `_0003()`.
    ///
    /// # Returns
    /// `true` for `AV1` and `AV110bit`.
    pub fn is_av1(self) -> bool {
        matches!(self, Self::AV1 | Self::AV110bit)
    }

    /// Reports whether the codec produces 10-bit output.
    ///
    /// # Returns
    /// `true` for `HEVC10bit` and `AV110bit`.
    pub fn is_10bit(self) -> bool {
        matches!(self, Self::HEVC10bit | Self::AV110bit)
    }

    /// Human readable name shown in the UI (`EnumDisplayName`), stored in `StreamerSettings.CodecName`.
    ///
    /// # Returns
    /// Display string.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::H264 => "H.264",
            Self::HEVC => "HEVC",
            Self::VP8 => "VP8",
            Self::VP9 => "VP9",
            Self::H264Plus => "H.264+",
            Self::HEVC10bit => "HEVC 10-bit",
            Self::AV1 => "AV1",
            Self::AV110bit => "AV1 10-bit",
        }
    }
}

/// Projection of a played video.  C#: `VirtualDesktop.Net.VideoProjection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
#[allow(missing_docs)]
pub enum VideoProjection {
    #[default]
    Unknown = 0,
    Standard,
    HalfSBS,
    FullSBS,
    OverUnder,
    Anaglyph,
    Sphere,
    Sphere3D,
    Dome,
    Dome3D,
    Fisheye,
    Fisheye3D,
    Cube,
    Cube3D,
}

/// NAT classification produced by the traceroute probe.  C#: `VirtualDesktop.Net.RoutingStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
pub enum RoutingStatus {
    /// The PC has a public address (first hop is public).
    #[default]
    PublicIP = 0,
    /// Behind one NAT.
    NAT = 1,
    /// Behind two NATs.
    DoubleNAT = 2,
    /// Behind carrier-grade NAT (100.64.0.0/10 on the path).
    CGNAT = 3,
}

/// Coarse network link classification.  C#: `VirtualDesktop.Net.NetworkConnectionType`
/// (member names are obfuscated; meanings inferred from `_0019._0002._0001(IEnumerable)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
pub enum NetworkConnectionType {
    /// No usable adapter.
    #[default]
    None = 0,
    /// Wireless adapter in use (PC on Wi-Fi).
    Wireless = 1,
    /// Wired adapter slower than 1 Gbit/s.
    Ethernet = 2,
    /// Wired gigabit adapter.
    GigabitEthernet = 3,
}

/// Rendezvous/relay region.  C#: `VirtualDesktop.Interfaces.ServerRegion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
pub enum ServerRegion {
    /// North America (west).
    AmericaWest = 0,
    /// North America (central) — chosen for UTC offsets between −12 h and −2 h.
    #[default]
    AmericaCentral = 1,
    /// Europe — chosen for every other UTC offset.
    Europe = 2,
}

/// Headset / client device model.  C#: `Xenko.VR.HmdType` (member names obfuscated; the
/// `EnumDisplayName` attributes give the meaning).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HmdType(pub i32);

#[allow(missing_docs)]
impl HmdType {
    pub const UNKNOWN: Self = Self(-1);
    pub const IPHONE: Self = Self(10);
    pub const IPAD: Self = Self(20);
    pub const APPLE_TV: Self = Self(25);
    pub const ANDROID_PHONE: Self = Self(30);
    pub const ANDROID_TABLET: Self = Self(40);
    pub const ANDROID_TV: Self = Self(45);
    pub const ANDROID_DESKTOP: Self = Self(50);
    pub const QUEST: Self = Self(259);
    pub const QUEST2: Self = Self(320);
    pub const QUEST_PRO: Self = Self(400);
    pub const VIVE_FOCUS3: Self = Self(600);
    pub const VIVE_XR_ELITE: Self = Self(650);
    pub const PICO_NEO3: Self = Self(700);
    pub const PICO_NEO3_LINK: Self = Self(701);
    pub const PICO4: Self = Self(800);
    pub const QUEST3: Self = Self(1000);
    pub const QUEST3S: Self = Self(1001);
    pub const PICO4_ULTRA: Self = Self(1010);
    /// Undocumented member following Pico 4 Ultra (no display name in the binary).
    pub const PICO_1011: Self = Self(1011);
    pub const GALAXY_XR: Self = Self(1100);
    pub const XREAL_AURA: Self = Self(1200);
    pub const PLAY_FOR_DREAM_MR: Self = Self(1400);
    pub const STEAM_FRAME: Self = Self(1500);

    /// Display name from the `EnumDisplayName` attribute.
    ///
    /// # Returns
    /// The marketing name, or `"Unknown"`.
    pub fn display_name(self) -> &'static str {
        match self.0 {
            10 => "iPhone",
            20 => "iPad",
            25 => "Apple TV",
            30 => "Android Phone",
            40 => "Android Tablet",
            45 => "Android TV",
            50 => "Android Desktop",
            259 => "Oculus Quest",
            320 => "Oculus Quest 2",
            400 => "Meta Quest Pro",
            600 => "Vive Focus 3",
            650 => "Vive XR Elite",
            700 => "Pico Neo 3",
            701 => "Pico Neo 3 Link",
            800 => "Pico 4",
            1000 => "Meta Quest 3",
            1001 => "Meta Quest 3S",
            1010 => "Pico 4 Ultra",
            1100 => "Samsung Galaxy XR",
            1200 => "XREAL Aura",
            1400 => "Play for Dream MR",
            1500 => "Steam Frame",
            _ => "Unknown",
        }
    }
}

impl Default for HmdType {
    /// Returns [`HmdType::UNKNOWN`], the `DefaultValue` of `SharedMobileSettings.HmdType`.
    ///
    /// # Returns
    /// `HmdType(-1)`.
    fn default() -> Self {
        Self::UNKNOWN
    }
}

/// Headset feature bit-set.  C#: `VirtualDesktop.Interfaces.Features` (`[Flags]`, members
/// obfuscated).  Only the bits whose meaning could be inferred from their use are named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Features(pub u32);

impl Features {
    /// Bit `0x4000` (`_0015`): headset can decode AV1 (checked before selecting AV1 in NVENC).
    pub const AV1_DECODE: u32 = 0x4000;
    /// Bit `0x20000` (`_0018`): headset can decode 4:4:4 chroma (gates `EnableYuv444`).
    pub const YUV444_DECODE: u32 = 0x20000;
    /// Bit `0x40000` (`_0019`): headset can decode 10-bit / HEVC (checked before HEVC selection).
    pub const HEVC_DECODE: u32 = 0x40000;

    /// Tests whether every bit in `mask` is set.
    ///
    /// # Arguments
    /// * `mask` - bits to test.
    ///
    /// # Returns
    /// `true` when all bits are present.
    pub fn contains(self, mask: u32) -> bool {
        self.0 & mask == mask
    }
}

/// VR frame-rate preference.  C#: `VirtualDesktop.Mobile.Framerate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
#[allow(missing_docs)]
pub enum Framerate {
    #[default]
    Default = 0,
    Framerate24 = 24,
    Framerate30 = 30,
    Framerate60 = 60,
    Framerate72 = 72,
    Framerate80 = 80,
    Framerate90 = 90,
    Framerate96 = 96,
    Framerate100 = 100,
    Framerate120 = 120,
}

/// VR render quality preset.  C#: `VirtualDesktop.Mobile.VRGraphicsQuality`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
#[allow(missing_docs)]
pub enum VRGraphicsQuality {
    Potato = -1,
    Low = 0,
    #[default]
    Medium = 1,
    High = 2,
    Ultra = 3,
    Godlike = 4,
    Monster = 5,
}

/// Active VR runtime.  C#: `VirtualDesktop.Mobile.Runtime` (members obfuscated).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
pub enum Runtime {
    /// No VR runtime active.
    #[default]
    None = 0,
    /// SteamVR (OpenVR driver).
    SteamVR = 1,
    /// Virtual Desktop's own OpenXR runtime.
    VDXR = 2,
    /// VDXR plus the Oculus compatibility layer.
    VDXRPlusOC = 3,
    /// SteamVR's OpenXR runtime.
    SteamXR = 4,
}

/// Video download state.  C#: `VirtualDesktop.Mobile.DownloadState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
#[allow(missing_docs)]
pub enum DownloadState {
    #[default]
    None = 0,
    CanPaste,
    RetrievingInfo,
    Downloading,
}

/// Account platform.  C#: `VirtualDesktop.Interfaces.Platform`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
#[allow(missing_docs)]
pub enum Platform {
    Oculus = 0,
    /// Displayed as "Meta".
    OculusQuest = 1,
    /// Displayed as "Viveport".
    Vive = 2,
    Pico = 3,
    Google = 5,
    PlayForDream = 6,
    Apple = 7,
    Steam = 8,
}

impl Platform {
    /// Name used by the DataContract serializer and as the key of `StreamerSettings.Accounts`.
    ///
    /// # Returns
    /// The C# member name.
    pub fn member_name(self) -> &'static str {
        match self {
            Self::Oculus => "Oculus",
            Self::OculusQuest => "OculusQuest",
            Self::Vive => "Vive",
            Self::Pico => "Pico",
            Self::Google => "Google",
            Self::PlayForDream => "PlayForDream",
            Self::Apple => "Apple",
            Self::Steam => "Steam",
        }
    }

    /// Parses a C# member name (case-insensitive) or a friendly alias (`meta`, `viveport`, ...).
    ///
    /// # Arguments
    /// * `s` - name to parse.
    ///
    /// # Returns
    /// The platform, or `None` if unknown.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "oculus" | "rift" => Self::Oculus,
            "oculusquest" | "meta" | "quest" => Self::OculusQuest,
            "vive" | "viveport" => Self::Vive,
            "pico" => Self::Pico,
            "google" => Self::Google,
            "playfordream" | "pfd" => Self::PlayForDream,
            "apple" => Self::Apple,
            "steam" => Self::Steam,
            _ => return None,
        })
    }
}

/// Store a game belongs to.  C#: `VirtualDesktop.Interfaces.GamePlatform : byte`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum GamePlatform {
    OculusRift = 0,
    Steam = 1,
    Viveport = 2,
}

/// Emulated gamepad model.  C#: `VirtualDesktop.Interfaces.GamepadModel : byte`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum GamepadModel {
    #[default]
    Xbox360 = 0,
    DualShock4 = 1,
}

/// Operating system reported to the registry.  C#: `VirtualDesktop.Interfaces.OS`.
///
/// The service only knows Windows and macOS; a Linux host reports `Windows` so that the headset
/// app enables desktop *and* VR features (configurable in the streamer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
#[allow(missing_docs)]
pub enum Os {
    #[default]
    Windows = 0,
    MacOS = 1,
}

impl Os {
    /// Member name used by the DataContract serializer.
    ///
    /// # Returns
    /// `"Windows"` or `"MacOS"`.
    pub fn member_name(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::MacOS => "MacOS",
        }
    }
}

/// OpenXR runtime preference.  C#: `VirtualDesktop.Interfaces.OpenXRRuntime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
#[allow(missing_docs)]
pub enum OpenXrRuntime {
    #[default]
    Automatic = 0,
    SteamVR = 1,
    VDXR = 2,
}

/// Where PC audio is played.  C#: `VirtualDesktop.Streamer.AudioStreaming`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
pub enum AudioStreaming {
    /// "Computer only" — no audio is streamed.
    Disabled = 0,
    /// "VR headset only" — PC speakers are muted while streaming.
    #[default]
    HeadsetOnly = 1,
    /// "VR headset & computer".
    HeadsetAndComputer = 2,
}

/// Gamepad emulation mode.  C#: `VirtualDesktop.Streamer.GamepadEmulation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
#[allow(missing_docs)]
pub enum GamepadEmulation {
    #[default]
    Automatic = 0,
    Xbox = 1,
    Dualshock = 2,
}

/// Bit flags of the Xbox-style gamepad state.  C#: `VirtualDesktop.Interfaces.GamepadButtonFlags : short`
/// (identical to `XINPUT_GAMEPAD_*`).
#[allow(missing_docs)]
pub mod gamepad_buttons {
    pub const DPAD_UP: u16 = 0x0001;
    pub const DPAD_DOWN: u16 = 0x0002;
    pub const DPAD_LEFT: u16 = 0x0004;
    pub const DPAD_RIGHT: u16 = 0x0008;
    pub const START: u16 = 0x0010;
    pub const BACK: u16 = 0x0020;
    pub const LEFT_THUMB: u16 = 0x0040;
    pub const RIGHT_THUMB: u16 = 0x0080;
    pub const LEFT_SHOULDER: u16 = 0x0100;
    pub const RIGHT_SHOULDER: u16 = 0x0200;
    pub const GUIDE: u16 = 0x0400;
    pub const A: u16 = 0x1000;
    pub const B: u16 = 0x2000;
    pub const X: u16 = 0x4000;
    pub const Y: u16 = 0x8000;
}

/// Bit flags of VR controller buttons.  C#: `VirtualDesktop.Interfaces.ControllerButtonFlags : short`.
#[allow(missing_docs)]
pub mod controller_buttons {
    pub const A: u16 = 0x0001;
    pub const B: u16 = 0x0002;
    pub const X: u16 = 0x0004;
    pub const Y: u16 = 0x0008;
    pub const R_THUMB: u16 = 0x0010;
    pub const R_TRIGGER: u16 = 0x0020;
    pub const R_GRIP: u16 = 0x0040;
    pub const L_THUMB: u16 = 0x0080;
    pub const L_TRIGGER: u16 = 0x0100;
    pub const L_GRIP: u16 = 0x0200;
    pub const ENTER: u16 = 0x0400;
    pub const BACK: u16 = 0x0800;
}

/// Bit flags of VR controller capacitive touch.  C#: `VirtualDesktop.Interfaces.ControllerTouchFlags : short`.
#[allow(missing_docs)]
pub mod controller_touch {
    pub const A: u16 = 0x0001;
    pub const B: u16 = 0x0002;
    pub const X: u16 = 0x0004;
    pub const Y: u16 = 0x0008;
    pub const R_THUMB: u16 = 0x0010;
    pub const R_TRIGGER: u16 = 0x0020;
    pub const R_THUMB_REST: u16 = 0x0040;
    pub const L_THUMB: u16 = 0x0080;
    pub const L_TRIGGER: u16 = 0x0100;
    pub const L_THUMB_REST: u16 = 0x0200;
    pub const R_INDEX_POINTING: u16 = 0x0400;
    pub const R_THUMB_UP: u16 = 0x0800;
    pub const L_INDEX_POINTING: u16 = 0x1000;
    pub const L_THUMB_UP: u16 = 0x2000;
}
