//! Keyboard shortcuts.  C#: `BindingSettings` (`BindingSettings.json`), `BindingAction` and
//! `BindingShortcut`.
//!
//! Each action has a system-wide hotkey (default `Win + Shift + <letter>`).  Most of them send a
//! message to the headset; `SwitchMonitor` and `ToggleVRMode` act on the streamer itself.  The
//! hotkeys are registered by [`crate::hotkeys`] while the settings window's event loop runs.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use vd_protocol::{MessageType, NetMessage};
use vd_protocol::types::StreamingSource;

use crate::app::App;
use crate::settings::StreamerSettings;

/// WPF `ModifierKeys` bits, as stored by the original.
pub mod modifiers {
    /// Alt.
    pub const ALT: u32 = 1;
    /// Ctrl.
    pub const CONTROL: u32 = 2;
    /// Shift.
    pub const SHIFT: u32 = 4;
    /// Windows / Super.
    pub const WINDOWS: u32 = 8;
}

/// Keys a shortcut can use: `(WinForms Keys / virtual-key code, label)`.
pub const KEYS: &[(u32, &str)] = &[
    (0x41, "A"), (0x42, "B"), (0x43, "C"), (0x44, "D"), (0x45, "E"), (0x46, "F"), (0x47, "G"), (0x48, "H"), (0x49, "I"),
    (0x4A, "J"), (0x4B, "K"), (0x4C, "L"), (0x4D, "M"), (0x4E, "N"), (0x4F, "O"), (0x50, "P"), (0x51, "Q"), (0x52, "R"),
    (0x53, "S"), (0x54, "T"), (0x55, "U"), (0x56, "V"), (0x57, "W"), (0x58, "X"), (0x59, "Y"), (0x5A, "Z"),
    (0x30, "0"), (0x31, "1"), (0x32, "2"), (0x33, "3"), (0x34, "4"), (0x35, "5"), (0x36, "6"), (0x37, "7"), (0x38, "8"), (0x39, "9"),
    (0x70, "F1"), (0x71, "F2"), (0x72, "F3"), (0x73, "F4"), (0x74, "F5"), (0x75, "F6"), (0x76, "F7"), (0x77, "F8"), (0x78, "F9"),
    (0x79, "F10"), (0x7A, "F11"), (0x7B, "F12"),
];

/// Label of a key code.
///
/// # Arguments
/// * `code` - virtual-key code.
///
/// # Returns
/// The label from [`KEYS`], or the code in hex.
pub fn key_label(code: u32) -> String {
    KEYS.iter().find(|(c, _)| *c == code).map(|(_, l)| (*l).to_owned()).unwrap_or_else(|| format!("0x{code:02X}"))
}

/// An action that can be bound to a hotkey.  C#: `BindingAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[allow(missing_docs)]
pub enum BindingAction {
    SwitchMonitor,
    #[serde(rename = "ToggleVRMode")]
    ToggleVrMode,
    #[serde(rename = "ToggleVRPassthrough")]
    ToggleVrPassthrough,
    #[serde(rename = "EnableVRPassthrough")]
    EnableVrPassthrough,
    #[serde(rename = "DisableVRPassthrough")]
    DisableVrPassthrough,
    ToggleHandPassthrough,
    ToggleDeskPassthrough,
    TogglePerformanceOverlay,
    ToggleFoveatedStreaming,
}

impl BindingAction {
    /// Every action in display order.
    pub const ALL: [BindingAction; 9] = [
        Self::SwitchMonitor,
        Self::ToggleVrMode,
        Self::ToggleVrPassthrough,
        Self::EnableVrPassthrough,
        Self::DisableVrPassthrough,
        Self::ToggleHandPassthrough,
        Self::ToggleDeskPassthrough,
        Self::TogglePerformanceOverlay,
        Self::ToggleFoveatedStreaming,
    ];

    /// Text shown in the BINDINGS tab.
    ///
    /// # Returns
    /// The label.
    pub fn label(self) -> &'static str {
        match self {
            Self::SwitchMonitor => "Switch monitor",
            Self::ToggleVrMode => "Toggle VR mode",
            Self::ToggleVrPassthrough => "Toggle VR passthrough",
            Self::EnableVrPassthrough => "Enable VR passthrough",
            Self::DisableVrPassthrough => "Disable VR passthrough",
            Self::ToggleHandPassthrough => "Toggle hand passthrough",
            Self::ToggleDeskPassthrough => "Toggle desk passthrough",
            Self::TogglePerformanceOverlay => "Toggle performance overlay",
            Self::ToggleFoveatedStreaming => "Toggle foveated streaming",
        }
    }

    /// Default shortcut (the `[_0018_0002._0001(Keys, ModifierKeys)]` attribute of each member).
    ///
    /// # Returns
    /// `Win + Shift + <letter>`.
    pub fn default_shortcut(self) -> BindingShortcut {
        let key = match self {
            Self::SwitchMonitor => b'A',
            Self::ToggleVrMode => b'D',
            Self::ToggleVrPassthrough => b'C',
            Self::EnableVrPassthrough => b'Z',
            Self::DisableVrPassthrough => b'X',
            Self::ToggleHandPassthrough => b'H',
            Self::ToggleDeskPassthrough => b'K',
            Self::TogglePerformanceOverlay => b'O',
            Self::ToggleFoveatedStreaming => b'G',
        };
        BindingShortcut { key_code: u32::from(key), modifiers: modifiers::SHIFT | modifiers::WINDOWS }
    }

    /// Headset message sent by this action, if it is a plain message.
    ///
    /// # Returns
    /// The message type, or `None` for actions handled locally.
    fn message(self) -> Option<MessageType> {
        Some(match self {
            Self::ToggleVrPassthrough => MessageType::ToggleVRPassthrough,
            Self::EnableVrPassthrough => MessageType::EnableVRPassthrough,
            Self::DisableVrPassthrough => MessageType::DisableVRPassthrough,
            Self::ToggleHandPassthrough => MessageType::ToggleHandPassthrough,
            Self::ToggleDeskPassthrough => MessageType::ToggleDeskPassthrough,
            Self::TogglePerformanceOverlay => MessageType::TogglePerformanceOverlay,
            Self::ToggleFoveatedStreaming => MessageType::ToggleFoveatedStreaming,
            Self::SwitchMonitor | Self::ToggleVrMode => return None,
        })
    }
}

/// A key plus modifiers.  C#: `BindingShortcut`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingShortcut {
    /// Virtual-key code (`System.Windows.Forms.Keys`).
    #[serde(rename = "KeyCode")]
    pub key_code: u32,
    /// [`modifiers`] bit set (`System.Windows.Input.ModifierKeys`).
    #[serde(rename = "ModifierKeys")]
    pub modifiers: u32,
}

impl BindingShortcut {
    /// Human-readable form, e.g. `Win + Shift + A`.
    ///
    /// # Returns
    /// The text.
    pub fn display(&self) -> String {
        let mut parts = Vec::new();
        for (bit, name) in [(modifiers::WINDOWS, if cfg!(windows) { "Win" } else { "Super" }), (modifiers::CONTROL, "Ctrl"), (modifiers::ALT, "Alt"), (modifiers::SHIFT, "Shift")] {
            if self.modifiers & bit != 0 {
                parts.push(name.to_owned());
            }
        }
        parts.push(key_label(self.key_code));
        parts.join(" + ")
    }
}

/// Persisted shortcuts.  C#: `BindingSettings` (`SerializedSettingsBase<BindingSettings>`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BindingSettings {
    /// Customised shortcuts (actions not listed use their default).
    #[serde(rename = "Bindings")]
    pub bindings: BTreeMap<BindingAction, BindingShortcut>,
    /// Register the hotkeys at all.
    #[serde(rename = "HotKeysEnabled")]
    pub hotkeys_enabled: bool,
}

impl Default for BindingSettings {
    /// Every action on its default shortcut, hotkeys enabled.
    ///
    /// # Returns
    /// The defaults.
    fn default() -> Self {
        Self { bindings: BindingAction::ALL.iter().map(|a| (*a, a.default_shortcut())).collect(), hotkeys_enabled: true }
    }
}

impl BindingSettings {
    /// Settings file path (next to `StreamerSettings.json`).
    ///
    /// # Returns
    /// The path.
    pub fn path() -> PathBuf {
        StreamerSettings::directory().join("BindingSettings.json")
    }

    /// Loads the shortcuts, falling back to the defaults.
    ///
    /// # Returns
    /// The settings.
    pub fn load() -> Self {
        let mut s: Self = std::fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        for a in BindingAction::ALL {
            s.bindings.entry(a).or_insert_with(|| a.default_shortcut());
        }
        s
    }

    /// Writes the shortcuts.
    ///
    /// # Errors
    /// I/O errors creating the directory or writing the file.
    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(StreamerSettings::directory())?;
        std::fs::write(Self::path(), serde_json::to_string_pretty(self).map_err(std::io::Error::other)?)
    }

    /// Shortcut of an action.
    ///
    /// # Arguments
    /// * `action` - the action.
    ///
    /// # Returns
    /// Its shortcut (default if unset).
    pub fn get(&self, action: BindingAction) -> BindingShortcut {
        self.bindings.get(&action).copied().unwrap_or_else(|| action.default_shortcut())
    }

    /// Restores every default (C#: `ResetToDefaults`).
    pub fn reset_to_defaults(&mut self) {
        *self = Self::default();
    }
}

/// Minimum time between two VR-mode toggles (C#: `_toggleVRTimer`, 1000 ms).
const VR_TOGGLE_COOLDOWN: Duration = Duration::from_secs(1);

/// Executes a hotkey action.  C#: `BindingSettings.OnHotKeyPressed`.
///
/// # Arguments
/// * `app` - application context.
/// * `action` - the triggered action.
/// * `last_vr_toggle` - time of the previous VR-mode toggle (debounce state).
pub async fn perform(app: &App, action: BindingAction, last_vr_toggle: &mut Option<Instant>) {
    debug!(?action, "hotkey");
    match action {
        BindingAction::SwitchMonitor => crate::dispatch::switch_monitor(app),
        BindingAction::ToggleVrMode => {
            if !app.state.streamer.borrow().has_vr || last_vr_toggle.is_some_and(|t| t.elapsed() < VR_TOGGLE_COOLDOWN) {
                return;
            }
            *last_vr_toggle = Some(Instant::now());
            app.state.streamer.send_modify(|s| {
                s.streaming_source = if s.streaming_source == StreamingSource::VR { StreamingSource::Desktop } else { StreamingSource::VR };
            });
        }
        other => {
            if !app.state.dynamic.borrow().is_connected {
                return;
            }
            if let Some(ty) = other.message() {
                app.send(&NetMessage::new(ty)).await;
            } else {
                warn!(?other, "unhandled hotkey action");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_original_json_and_fills_defaults() {
        let json = r#"{"Bindings":{"ToggleVRMode":{"KeyCode":70,"ModifierKeys":6}},"HotKeysEnabled":true}"#;
        let mut s: BindingSettings = serde_json::from_str(json).unwrap();
        for a in BindingAction::ALL {
            s.bindings.entry(a).or_insert_with(|| a.default_shortcut());
        }
        assert_eq!(s.get(BindingAction::ToggleVrMode).display(), "Ctrl + Shift + F");
        assert_eq!(s.get(BindingAction::SwitchMonitor), BindingShortcut { key_code: 0x41, modifiers: modifiers::SHIFT | modifiers::WINDOWS });
    }
}
