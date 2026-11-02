//! Settings objects synchronised over the messaging channel.
//!
//! C#: `VirtualDesktop.Net.NetworkSettingsBase<T>` and its three subclasses in
//! `VirtualDesktop.Mobile`.  Synchronisation protocol:
//!
//! * The owner ("server") sends the whole object as compact JSON in a `*SettingsReload`
//!   message when the connection is established.  Serialisation uses Newtonsoft with
//!   `DefaultValueHandling.IgnoreAndPopulate`, so properties equal to their default are omitted
//!   and missing properties are reset to their default on the receiving side.
//! * Afterwards every property setter sends `{"<PropertyName>": <value>}` (defaults *included*)
//!   in a `*SettingsPropertyChanged` message.  Either side may send property changes.
//! * When the connection drops, the client side resets the object to defaults
//!   (`JsonConvert.PopulateObject("{}", ...)`).
//!
//! Ownership: [`SharedStreamerSettings`] is owned by the PC (`InitializeAsServer`);
//! [`SharedUserSettings`] and [`SharedMobileSettings`] are owned by the headset
//! (`InitializeAsClient`).

use serde::{Serialize, de::DeserializeOwned};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::{ProtocolError, Result};
use crate::math::{FovPort, Size2};
use crate::message::{MessageType, NetMessage};
use crate::types::{
    DownloadState, Features, Framerate, HmdType, Runtime, StreamingSource, VRGraphicsQuality, VideoCodec,
};

/// A settings type that is synchronised over the messaging channel.
pub trait SyncedSettings: Serialize + DeserializeOwned + Default + Clone + PartialEq {
    /// Message type carrying the full object.
    const RELOAD: MessageType;
    /// Message type carrying one property.
    const PROPERTY_CHANGED: MessageType;
    /// Whether the PC is the owner of this object.
    const PC_OWNED: bool;
}

/// Serialises `value` to a JSON object omitting properties equal to their default
/// (`DefaultValueHandling.IgnoreAndPopulate`).
///
/// # Arguments
/// * `value` - settings object.
///
/// # Returns
/// The JSON object map.
///
/// # Errors
/// [`ProtocolError::Json`] if serialisation fails (only possible for non-finite floats).
pub fn to_sparse_json<T: Serialize + Default>(value: &T) -> Result<Map<String, Value>> {
    let Value::Object(mut map) = serde_json::to_value(value)? else {
        return Ok(Map::new());
    };
    if let Value::Object(defaults) = serde_json::to_value(T::default())? {
        map.retain(|k, v| defaults.get(k) != Some(v));
    }
    Ok(map)
}

/// Builds the `*SettingsReload` message for `value`.
///
/// # Arguments
/// * `value` - settings object.
///
/// # Returns
/// The message (payload = compact sparse JSON).
///
/// # Errors
/// [`ProtocolError::Json`] on serialisation failure.
pub fn reload_message<T: SyncedSettings>(value: &T) -> Result<NetMessage> {
    let json = serde_json::to_vec(&Value::Object(to_sparse_json(value)?))?;
    Ok(NetMessage::with_payload(T::RELOAD, json))
}

/// Builds a `*SettingsPropertyChanged` message.
///
/// # Arguments
/// * `property` - C# property name (e.g. `"StreamingSource"`).
/// * `value` - new value (serialised with defaults included).
///
/// # Returns
/// The message.
///
/// # Errors
/// [`ProtocolError::Json`] on serialisation failure.
pub fn property_message<T: SyncedSettings, V: Serialize>(property: &str, value: &V) -> Result<NetMessage> {
    let mut map = Map::new();
    map.insert(property.to_owned(), serde_json::to_value(value)?);
    Ok(NetMessage::with_payload(T::PROPERTY_CHANGED, serde_json::to_vec(&Value::Object(map))?))
}

/// Builds property-changed messages for every property that differs between `old` and `new`.
///
/// # Arguments
/// * `old` - previous state.
/// * `new` - current state.
///
/// # Returns
/// One message per changed property.
///
/// # Errors
/// [`ProtocolError::Json`] on serialisation failure.
pub fn diff_messages<T: SyncedSettings>(old: &T, new: &T) -> Result<Vec<NetMessage>> {
    let (Value::Object(a), Value::Object(b)) = (serde_json::to_value(old)?, serde_json::to_value(new)?) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (k, v) in b {
        if a.get(&k) != Some(&v) {
            let mut map = Map::new();
            map.insert(k, v);
            out.push(NetMessage::with_payload(T::PROPERTY_CHANGED, serde_json::to_vec(&Value::Object(map))?));
        }
    }
    Ok(out)
}

/// Applies a `*SettingsReload` payload: missing properties take their default value.
///
/// # Arguments
/// * `msg` - the reload message.
///
/// # Returns
/// The new settings object.
///
/// # Errors
/// [`ProtocolError::Json`] / [`ProtocolError::InvalidUtf8`] for malformed payloads.
pub fn apply_reload<T: SyncedSettings>(msg: &NetMessage) -> Result<T> {
    let text = msg.payload_text()?;
    let text = if text.trim().is_empty() { "{}" } else { text.as_str() };
    Ok(serde_json::from_str(text)?)
}

/// Applies a `*SettingsPropertyChanged` payload on top of `current`.
///
/// Unknown property names are ignored (Newtonsoft's default `MissingMemberHandling.Ignore`).
///
/// # Arguments
/// * `current` - current state.
/// * `msg` - the property message.
///
/// # Returns
/// `(new state, names of the properties contained in the message)`.
///
/// # Errors
/// [`ProtocolError::Json`] if the payload is not a JSON object or a value has the wrong type.
pub fn apply_property<T: SyncedSettings>(current: &T, msg: &NetMessage) -> Result<(T, Vec<String>)> {
    let Value::Object(update) = serde_json::from_str::<Value>(&msg.payload_text()?)? else {
        return Err(ProtocolError::Json(serde::de::Error::custom("property change payload is not an object")));
    };
    let Value::Object(mut merged) = serde_json::to_value(current)? else {
        return Err(ProtocolError::Json(serde::de::Error::custom("settings did not serialise to an object")));
    };
    let mut names = Vec::new();
    for (k, v) in update {
        if merged.contains_key(&k) {
            merged.insert(k.clone(), v);
            names.push(k);
        }
    }
    Ok((serde_json::from_value(Value::Object(merged))?, names))
}

fn t() -> bool {
    true
}

/// Settings owned by the PC and mirrored on the headset.  C#: `VirtualDesktop.Mobile.SharedStreamerSettings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[allow(missing_docs)]
pub struct SharedStreamerSettings {
    /// A VR runtime / headset driver is available on the PC.
    #[serde(rename = "HasVR")]
    pub has_vr: bool,
    #[serde(rename = "StreamingSource")]
    pub streaming_source: StreamingSource,
    /// Codec selected by the active encoder.
    #[serde(rename = "ActiveCodec")]
    pub active_codec: VideoCodec,
    #[serde(rename = "AutoAdjustBitrate")]
    pub auto_adjust_bitrate: bool,
    #[serde(rename = "CanSwitchMonitor")]
    pub can_switch_monitor: bool,
    #[serde(rename = "CanLaunchSteamVR", default = "t")]
    pub can_launch_steam_vr: bool,
    #[serde(rename = "ActiveRuntime")]
    pub active_runtime: Runtime,
    #[serde(rename = "AllowMotionExtrapolation", default = "t")]
    pub allow_motion_extrapolation: bool,
    /// GPU name.
    #[serde(rename = "AdapterName")]
    pub adapter_name: Option<String>,
    #[serde(rename = "AdapterIsAMD")]
    pub adapter_is_amd: bool,
    #[serde(rename = "DownloadState")]
    pub download_state: DownloadState,
    #[serde(rename = "DownloadProgress")]
    pub download_progress: f32,
    /// Current keyboard layout name of the PC.
    #[serde(rename = "InputLanguage")]
    pub input_language: Option<String>,
    #[serde(rename = "CanAddMonitor")]
    pub can_add_monitor: bool,
    #[serde(rename = "CanRemoveMonitor")]
    pub can_remove_monitor: bool,
}

impl Default for SharedStreamerSettings {
    /// Returns the `[DefaultValue]`-annotated defaults of the C# class.
    ///
    /// # Returns
    /// The default object.
    fn default() -> Self {
        Self {
            has_vr: false,
            streaming_source: StreamingSource::Desktop,
            active_codec: VideoCodec::Automatic,
            auto_adjust_bitrate: false,
            can_switch_monitor: false,
            can_launch_steam_vr: true,
            active_runtime: Runtime::None,
            allow_motion_extrapolation: true,
            adapter_name: None,
            adapter_is_amd: false,
            download_state: DownloadState::None,
            download_progress: 0.0,
            input_language: None,
            can_add_monitor: false,
            can_remove_monitor: false,
        }
    }
}

impl SyncedSettings for SharedStreamerSettings {
    const RELOAD: MessageType = MessageType::StreamerSettingsReload;
    const PROPERTY_CHANGED: MessageType = MessageType::StreamerSettingsPropertyChanged;
    const PC_OWNED: bool = true;
}

/// User preferences owned by the headset.  C#: `VirtualDesktop.Mobile.SharedUserSettings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[allow(missing_docs)]
pub struct SharedUserSettings {
    #[serde(rename = "VRFramerate")]
    pub vr_framerate: Framerate,
    #[serde(rename = "VRGraphicsQuality")]
    pub vr_graphics_quality: VRGraphicsQuality,
    /// Desktop bitrate slider in `[0, 1]`.
    #[serde(rename = "DesktopBitrateLimit")]
    pub desktop_bitrate_limit: f32,
    /// VR bitrate slider in `[0, 1]`.
    #[serde(rename = "VRBitrateLimit")]
    pub vr_bitrate_limit: f32,
    #[serde(rename = "Gamma")]
    pub gamma: f32,
    /// Resize the PC desktop to the headset's optimal resolution.
    #[serde(rename = "UseOptimalResolution")]
    pub use_optimal_resolution: bool,
    #[serde(rename = "EmulateGamepad")]
    pub emulate_gamepad: bool,
    #[serde(rename = "MicPassthrough")]
    pub mic_passthrough: bool,
    #[serde(rename = "MicVolume")]
    pub mic_volume: f32,
    #[serde(rename = "IncreaseVideoNominalRange")]
    pub increase_video_nominal_range: bool,
    #[serde(rename = "ForwardTrackingData")]
    pub forward_tracking_data: bool,
    #[serde(rename = "EmulateTrackers")]
    pub emulate_trackers: bool,
    #[serde(rename = "EmulateIndexControllers")]
    pub emulate_index_controllers: bool,
    /// Add an extra virtual display while streaming.
    #[serde(rename = "ExtraDisplay")]
    pub extra_display: bool,
    #[serde(rename = "UseMultiModal")]
    pub use_multi_modal: bool,
}

impl Default for SharedUserSettings {
    /// Returns the `[DefaultValue]`-annotated defaults of the C# class.
    ///
    /// # Returns
    /// The default object.
    fn default() -> Self {
        Self {
            vr_framerate: Framerate::Default,
            vr_graphics_quality: VRGraphicsQuality::Medium,
            desktop_bitrate_limit: 0.17,
            vr_bitrate_limit: 0.36,
            gamma: 1.0,
            use_optimal_resolution: true,
            emulate_gamepad: true,
            mic_passthrough: true,
            mic_volume: 0.85,
            increase_video_nominal_range: false,
            forward_tracking_data: false,
            emulate_trackers: false,
            emulate_index_controllers: false,
            extra_display: false,
            use_multi_modal: false,
        }
    }
}

impl SyncedSettings for SharedUserSettings {
    const RELOAD: MessageType = MessageType::UserSettingsReload;
    const PROPERTY_CHANGED: MessageType = MessageType::UserSettingsPropertyChanged;
    const PC_OWNED: bool = false;
}

/// Device description owned by the headset.  C#: `VirtualDesktop.Mobile.SharedMobileSettings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[allow(missing_docs)]
pub struct SharedMobileSettings {
    #[serde(rename = "HmdType")]
    pub hmd_type: HmdType,
    /// Native display resolution of the device.
    #[serde(rename = "Resolution")]
    pub resolution: Size2,
    /// Requested monitor orientation (`null` = leave as is).
    #[serde(rename = "IsPortrait")]
    pub is_portrait: Option<bool>,
    #[serde(rename = "IPD")]
    pub ipd: f32,
    #[serde(rename = "FovLeft")]
    pub fov_left: FovPort,
    #[serde(rename = "FovRight")]
    pub fov_right: FovPort,
    #[serde(rename = "LeftHanded")]
    pub left_handed: bool,
    /// Bandwidth measured with the data-channel probe (bit/s).
    #[serde(rename = "MeasuredBandwidth")]
    pub measured_bandwidth: i32,
    #[serde(rename = "IsVideoPaused")]
    pub is_video_paused: bool,
    #[serde(rename = "UseSpacewarp")]
    pub use_spacewarp: bool,
    #[serde(rename = "DesktopFramerate")]
    pub desktop_framerate: i32,
    #[serde(rename = "VideosFolderPath")]
    pub videos_folder_path: String,
    #[serde(rename = "Features")]
    pub features: Features,
    #[serde(rename = "VideoPlaybackSpeed")]
    pub video_playback_speed: f32,
    /// `true`: send Annex-B bitstreams; `false`: 4-byte length prefixes.
    #[serde(rename = "UseAnnexB")]
    pub use_annex_b: bool,
    #[serde(rename = "FoveatedStreaming")]
    pub foveated_streaming: bool,
    #[serde(rename = "RefreshRate")]
    pub refresh_rate: i32,
}

impl Default for SharedMobileSettings {
    /// Returns the `[DefaultValue]`-annotated defaults of the C# class.
    ///
    /// # Returns
    /// The default object.
    fn default() -> Self {
        Self {
            hmd_type: HmdType::UNKNOWN,
            resolution: Size2::default(),
            is_portrait: None,
            ipd: 0.0635,
            fov_left: FovPort::default(),
            fov_right: FovPort::default(),
            left_handed: false,
            measured_bandwidth: 0,
            is_video_paused: false,
            use_spacewarp: false,
            desktop_framerate: 60,
            videos_folder_path: String::new(),
            features: Features::default(),
            video_playback_speed: 1.0,
            use_annex_b: true,
            foveated_streaming: false,
            refresh_rate: 0,
        }
    }
}

impl SyncedSettings for SharedMobileSettings {
    const RELOAD: MessageType = MessageType::MobileSettingsReload;
    const PROPERTY_CHANGED: MessageType = MessageType::MobileSettingsPropertyChanged;
    const PC_OWNED: bool = false;
}

impl SharedMobileSettings {
    /// Effective desktop bitrate (the C# `[JsonIgnore] DesktopBitrate` property).
    ///
    /// # Arguments
    /// * `user` - user settings providing `DesktopBitrateLimit`.
    ///
    /// # Returns
    /// Bit/s; when no bandwidth has been measured yet the curve value is returned.
    pub fn desktop_bitrate(&self, user: &SharedUserSettings) -> i32 {
        let curve = crate::hmd::desktop_bitrate_for_limit(self.hmd_type, user.desktop_bitrate_limit);
        if self.measured_bandwidth > 0 { self.measured_bandwidth.min(curve) } else { curve }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_omits_defaults() {
        let s = SharedStreamerSettings { streaming_source: StreamingSource::Video, ..Default::default() };
        let msg = reload_message(&s).unwrap();
        assert_eq!(msg.payload_text().unwrap(), r#"{"StreamingSource":2}"#);
        let back: SharedStreamerSettings = apply_reload(&msg).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn property_change_round_trip() {
        let msg = property_message::<SharedMobileSettings, _>("HmdType", &HmdType::QUEST3).unwrap();
        let (s, names) = apply_property(&SharedMobileSettings::default(), &msg).unwrap();
        assert_eq!(s.hmd_type, HmdType::QUEST3);
        assert_eq!(names, ["HmdType"]);
    }
}
