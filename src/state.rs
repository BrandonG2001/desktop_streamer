//! Shared runtime state.
//!
//! C#: `SettingsBase<SharedStreamerSettings|SharedUserSettings|SharedMobileSettings>.Default` and
//! `SettingsBase<DynamicSettings>.Default` are global singletons whose `…Changed` events drive
//! the streamer.  Here each object lives in a `tokio::sync::watch` channel so tasks can await
//! changes; [`StateHub`] bundles them.

use std::sync::Arc;

use tokio::sync::watch;
use vd_platform::display::MonitorInfo;
use vd_protocol::settings::{SharedMobileSettings, SharedStreamerSettings, SharedUserSettings};
use vd_protocol::types::{NetworkConnectionType, RoutingStatus, StreamingSource};

/// Connection progress shown by the notification-area icon.  C#: `ConnectionManager` state
/// (`_001C_0002._0003`) behind `UpdateNotifyIconAndText`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionStatus {
    /// Not registered (no account, or between retries).
    #[default]
    Disconnected,
    /// Registering with vrdesktop.net / connecting to the relay.
    Connecting,
    /// Registered; waiting for a headset to connect.
    Waiting,
    /// A headset is connected.
    Connected,
}

/// Runtime-only state.  C#: `VirtualDesktop.Streamer.DynamicSettings`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DynamicState {
    /// All four channels connected (`IsConnected`).
    pub is_connected: bool,
    /// Available monitors (`Monitors`).
    pub monitors: Vec<MonitorInfo>,
    /// First monitor to stream (`MonitorStartIndex`, advanced by `SwitchMonitor`).
    pub monitor_start_index: usize,
    /// Incremented whenever a key frame must be produced (`ForceKeyFrame = true`).
    pub keyframe_requests: u64,
    /// Latest headset frame index (echoed in frame headers).
    pub last_headset_frame: i32,
    /// Measured motion-to-photon latency reported in the UI (`Latency`).
    pub latency_ms: i32,
    /// GPU / encoder description (`AdapterName`).
    pub adapter_name: String,
    /// A hardware encoder is available (`HasHardwareEncoder`).
    pub has_hardware_encoder: bool,
    /// Link type (`ConnectionType`).
    pub connection_type: NetworkConnectionType,
    /// NAT classification (`RoutingStatus`).
    pub routing_status: RoutingStatus,
    /// Session is encrypted (`IsLocalTrafficEncrypted`).
    pub encrypted: bool,
    /// Connection progress (tray icon).
    pub status: ConnectionStatus,
    /// Encoder detection has finished (successfully or not).
    pub media_ready: bool,
    /// Where headset microphone audio goes while passthrough runs (`None` when off), or
    /// `Some("")` when passthrough is on but no virtual microphone is available.
    pub microphone: Option<String>,
}

/// All shared state of the application.
#[derive(Clone)]
pub struct StateHub {
    /// PC-owned settings mirrored to the headset.
    pub streamer: Arc<watch::Sender<SharedStreamerSettings>>,
    /// Headset-owned user preferences.
    pub user: Arc<watch::Sender<SharedUserSettings>>,
    /// Headset-owned device description.
    pub mobile: Arc<watch::Sender<SharedMobileSettings>>,
    /// Runtime state.
    pub dynamic: Arc<watch::Sender<DynamicState>>,
}

impl StateHub {
    /// Creates the hub with default values.
    ///
    /// # Returns
    /// The hub.
    pub fn new() -> Self {
        Self {
            streamer: Arc::new(watch::channel(SharedStreamerSettings::default()).0),
            user: Arc::new(watch::channel(SharedUserSettings::default()).0),
            mobile: Arc::new(watch::channel(SharedMobileSettings::default()).0),
            dynamic: Arc::new(watch::channel(DynamicState::default()).0),
        }
    }

    /// Requests a key frame from every encoder (`DynamicSettings.ForceKeyFrame = true`).
    pub fn request_keyframe(&self) {
        self.dynamic.send_modify(|d| d.keyframe_requests += 1);
    }

    /// Current streaming source.
    ///
    /// # Returns
    /// `SharedStreamerSettings.StreamingSource`.
    pub fn streaming_source(&self) -> StreamingSource {
        self.streamer.borrow().streaming_source
    }

    /// Resets the headset-owned objects when the session ends (the original repopulates them
    /// from `"{}"` in `NetworkSettingsBase.OnStatusChanged`).
    pub fn reset_headset_state(&self) {
        self.user.send_replace(SharedUserSettings::default());
        self.mobile.send_replace(SharedMobileSettings::default());
        self.dynamic.send_modify(|d| {
            d.is_connected = false;
            d.last_headset_frame = 0;
        });
    }
}

impl Default for StateHub {
    /// See [`StateHub::new`].
    ///
    /// # Returns
    /// The hub.
    fn default() -> Self {
        Self::new()
    }
}
