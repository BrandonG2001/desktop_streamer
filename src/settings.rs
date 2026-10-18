//! Persisted streamer settings.  C#: `VirtualDesktop.Streamer.StreamerSettings`
//! (`SerializedSettingsBase<T>` → `%ProgramData%\Virtual Desktop\StreamerSettings.json`).
//!
//! Property names and defaults follow the original so the JSON stays recognisable.  Two
//! deliberate differences:
//!
//! * the original protects account names and the computer id with DPAPI
//!   (`ProtectedData.Protect(…, LocalMachine)` with an obfuscated entropy string); this port
//!   stores them in plain text in a file only readable by the owner, because DPAPI does not
//!   exist on Linux;
//! * port-specific options (capture source, render node, …) are added under a `Port` object.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vd_protocol::types::{AudioStreaming, GamepadEmulation, Os, OpenXrRuntime, Platform, VideoCodec};

fn t() -> bool {
    true
}

/// Port-specific options (not present in the original).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PortOptions {
    /// Capture source: `auto`, `x11`, `kmsgrab`, `ddagrab`, `gdigrab`, `avfoundation`, `test`.
    #[serde(rename = "CaptureSource")]
    pub capture_source: String,
    /// Extra FFmpeg input arguments when `capture_source` is `custom`.
    #[serde(rename = "CustomCaptureArgs")]
    pub custom_capture_args: Vec<String>,
    /// DRM render node for VA-API / QSV.
    #[serde(rename = "RenderNode")]
    pub render_node: String,
    /// DRM card for `kmsgrab`.
    #[serde(rename = "DrmCard")]
    pub drm_card: String,
    /// X11 display for `x11grab` (default `$DISPLAY`).
    #[serde(rename = "X11Display")]
    pub x11_display: String,
    /// Encoder backend override (`nvenc`, `amf`, `qsv`, `vaapi`, `videotoolbox`, `mf`, `v4l2m2m`, `software`).
    #[serde(rename = "EncoderBackend")]
    pub encoder_backend: Option<String>,
    /// Allow libx264 when no hardware encoder works.
    #[serde(rename = "AllowSoftwareEncoder", default = "t")]
    pub allow_software_encoder: bool,
    /// Operating system reported to the registry (the service only knows Windows/MacOS).
    #[serde(rename = "ReportedOS")]
    pub reported_os: Os,
    /// Reported streamer version (must be recent enough for the service to accept it).
    #[serde(rename = "ReportedVersion")]
    pub reported_version: String,
    /// Key-frame interval of the CLI encoder backend, in seconds.
    #[serde(rename = "KeyframeIntervalSeconds")]
    pub keyframe_interval_seconds: u32,
    /// Create PulseAudio/PipeWire virtual devices.
    #[serde(rename = "ManageAudioDevices", default = "t")]
    pub manage_audio_devices: bool,
}

impl Default for PortOptions {
    /// Returns the defaults.
    ///
    /// # Returns
    /// Options suitable for a typical desktop.
    fn default() -> Self {
        Self {
            capture_source: "auto".into(),
            custom_capture_args: Vec::new(),
            render_node: "/dev/dri/renderD128".into(),
            drm_card: "/dev/dri/card0".into(),
            x11_display: String::new(),
            encoder_backend: None,
            allow_software_encoder: true,
            reported_os: Os::Windows,
            reported_version: "1.34.18.0".into(),
            keyframe_interval_seconds: 2,
            manage_audio_devices: true,
        }
    }
}

/// Persisted settings.  C#: `StreamerSettings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamerSettings {
    /// Relay port rotation (0–5).
    #[serde(rename = "ServerRotation")]
    pub server_rotation: u16,
    /// Computer id reported to the registry (plain text; see module docs).
    #[serde(rename = "ComputerID")]
    pub computer_id: Option<String>,
    /// Accounts by platform member name (`"OculusQuest": ["user"]`).
    #[serde(rename = "Accounts")]
    pub accounts: BTreeMap<String, Vec<String>>,
    #[serde(rename = "AllowRemoteConnections", default = "t")]
    #[allow(missing_docs)]
    pub allow_remote_connections: bool,
    #[serde(rename = "EncryptLocalTraffic")]
    #[allow(missing_docs)]
    pub encrypt_local_traffic: bool,
    #[serde(rename = "AutoAdjustBitrate", default = "t")]
    #[allow(missing_docs)]
    pub auto_adjust_bitrate: bool,
    #[serde(rename = "UseVirtualAudioDriver", default = "t")]
    #[allow(missing_docs)]
    pub use_virtual_audio_driver: bool,
    #[serde(rename = "AudioStreaming")]
    #[allow(missing_docs)]
    pub audio_streaming: AudioStreaming,
    #[serde(rename = "UseTouchInput", default = "t")]
    #[allow(missing_docs)]
    pub use_touch_input: bool,
    #[serde(rename = "PreferredCodec")]
    #[allow(missing_docs)]
    pub preferred_codec: VideoCodec,
    #[serde(rename = "EnableAQ", default = "t")]
    #[allow(missing_docs)]
    pub enable_aq: bool,
    #[serde(rename = "EnableYuv444", default = "t")]
    #[allow(missing_docs)]
    pub enable_yuv444: bool,
    #[serde(rename = "Enable2Pass")]
    #[allow(missing_docs)]
    pub enable_2pass: bool,
    #[serde(rename = "GamepadEmulation")]
    #[allow(missing_docs)]
    pub gamepad_emulation: GamepadEmulation,
    #[serde(rename = "OpenXRRuntime")]
    #[allow(missing_docs)]
    pub openxr_runtime: OpenXrRuntime,
    /// Name of the last connected headset.
    #[serde(rename = "DeviceName")]
    pub device_name: Option<String>,
    /// Display name of the active codec.
    #[serde(rename = "CodecName")]
    pub codec_name: Option<String>,
    /// Root folder of the video library.
    #[serde(rename = "VideosRootPath")]
    pub videos_root_path: Option<String>,
    /// Folder where headset screenshots are saved.
    #[serde(rename = "ScreenshotsRootPath")]
    pub screenshots_root_path: Option<String>,
    /// Number of monitors to stream.
    #[serde(rename = "MonitorCount")]
    pub monitor_count: i32,
    /// Raise process priority while streaming (`BoostGamePriority`).
    #[serde(rename = "BoostGamePriority")]
    pub boost_priority: bool,
    /// Port-specific options.
    #[serde(rename = "Port")]
    pub port: PortOptions,
}

impl Default for StreamerSettings {
    /// Returns the `[DefaultValue]` defaults of the C# class.
    ///
    /// # Returns
    /// Default settings.
    fn default() -> Self {
        Self {
            server_rotation: 0,
            computer_id: None,
            accounts: BTreeMap::new(),
            allow_remote_connections: true,
            encrypt_local_traffic: false,
            auto_adjust_bitrate: true,
            use_virtual_audio_driver: true,
            audio_streaming: AudioStreaming::HeadsetOnly,
            use_touch_input: true,
            preferred_codec: VideoCodec::Automatic,
            enable_aq: true,
            enable_yuv444: true,
            enable_2pass: false,
            gamepad_emulation: GamepadEmulation::Automatic,
            openxr_runtime: OpenXrRuntime::Automatic,
            device_name: None,
            codec_name: None,
            videos_root_path: None,
            screenshots_root_path: None,
            monitor_count: 1,
            boost_priority: false,
            port: PortOptions::default(),
        }
    }
}

impl StreamerSettings {
    /// Directory holding the settings file.
    ///
    /// Windows: `%ProgramData%\Virtual Desktop` would collide with the original; this port uses
    /// `%APPDATA%\Virtual Desktop Streamer (Rust)`.  Linux: `$XDG_CONFIG_HOME/virtual-desktop-streamer`.
    /// The `VD_SETTINGS_DIR` environment variable overrides both (tests, multiple instances).
    ///
    /// # Returns
    /// The directory path.
    pub fn directory() -> PathBuf {
        if let Some(dir) = std::env::var_os("VD_SETTINGS_DIR") {
            return PathBuf::from(dir);
        }
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        if cfg!(windows) { base.join("Virtual Desktop Streamer (Rust)") } else { base.join("virtual-desktop-streamer") }
    }

    /// Path of the settings file.
    ///
    /// # Returns
    /// `<directory>/StreamerSettings.json`.
    pub fn path() -> PathBuf {
        Self::directory().join("StreamerSettings.json")
    }

    /// Loads the settings, falling back to defaults when the file is missing or invalid
    /// (C#: `JsonSettingsBase.Reload` → `PopulateObject("{}")` on error).
    ///
    /// # Returns
    /// The settings.
    pub fn load() -> Self {
        match std::fs::read_to_string(Self::path()) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "invalid settings file, using defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Saves the settings (indented JSON, owner-only permissions on Unix).
    ///
    /// # Errors
    /// I/O errors creating the directory or writing the file.
    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(Self::directory())?;
        let path = Self::path();
        std::fs::write(&path, serde_json::to_string_pretty(self).map_err(std::io::Error::other)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    /// Accounts as registry access tokens.
    ///
    /// # Returns
    /// One token per stored account.
    pub fn access_tokens(&self) -> Vec<vd_net::registry::PlatformAccessToken> {
        self.accounts
            .iter()
            .filter_map(|(p, names)| Platform::parse(p).map(|pl| (pl, names)))
            .flat_map(|(pl, names)| names.iter().map(move |n| vd_net::registry::PlatformAccessToken::new(pl, n.clone())))
            .collect()
    }

    /// Adds an account.  C#: `AddAccountWindow` → `StreamerSettings.Accounts`.
    ///
    /// # Arguments
    /// * `platform` - store platform.
    /// * `name` - user name.
    pub fn add_account(&mut self, platform: Platform, name: &str) {
        let list = self.accounts.entry(platform.member_name().to_owned()).or_default();
        if !list.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            list.push(name.to_owned());
        }
    }

    /// Removes an account.
    ///
    /// # Arguments
    /// * `platform` - store platform.
    /// * `name` - user name.
    ///
    /// # Returns
    /// `true` if an account was removed.
    pub fn remove_account(&mut self, platform: Platform, name: &str) -> bool {
        let Some(list) = self.accounts.get_mut(platform.member_name()) else { return false };
        let before = list.len();
        list.retain(|n| !n.eq_ignore_ascii_case(name));
        before != list.len()
    }

    /// Parses `ReportedVersion` into `(major, minor, build, revision)`.
    ///
    /// # Returns
    /// The version tuple (`1.34.18.0` if malformed).
    pub fn reported_version(&self) -> (i32, i32, i32, i32) {
        let mut it = self.port.reported_version.split('.').map(|p| p.parse::<i32>().unwrap_or(0));
        match (it.next(), it.next(), it.next(), it.next()) {
            (Some(a), Some(b), Some(c), d) => (a, b, c, d.unwrap_or(0)),
            _ => (1, 34, 18, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_original_style_json() {
        let json = r#"{"ServerRotation":3,"PreferredCodec":6,"MonitorCount":2,"Accounts":{"OculusQuest":["me"]}}"#;
        let s: StreamerSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.server_rotation, 3);
        assert_eq!(s.preferred_codec, VideoCodec::HEVC10bit);
        assert!(s.allow_remote_connections);
        assert_eq!(s.access_tokens().len(), 1);
    }
}
