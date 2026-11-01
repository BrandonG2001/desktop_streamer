//! Application context shared by every task.  Replaces the static classes `ConnectionManager`,
//! `StreamerManager` and the `SettingsBase<T>.Default` singletons of the original.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::{Notify, mpsc, watch};
use tracing::{info, warn};
use vd_media::codec::{Backend, EncoderCaps, detect_backend, probe_caps};
use vd_media::version::Ffmpeg;
use vd_net::channels::{AudioChannel, DataChannel, VideoChannel};
use vd_net::messaging::MessagingClient;
use vd_net::pairing::DiscoveryRequest;
use vd_protocol::NetMessage;
use vd_protocol::headset_state::HeadsetState;
use vd_protocol::messages::PeerInfo;
use vd_protocol::wire::GazeSample;

use crate::settings::StreamerSettings;
use crate::state::StateHub;

/// The four channels of a session (long-lived; reconnected for every session).
#[derive(Clone)]
pub struct Channels {
    /// Control messages (38810).
    pub messaging: Arc<MessagingClient>,
    /// Headset state / files (38820).
    pub data: Arc<DataChannel>,
    /// Encoded video (38830).
    pub video: Arc<VideoChannel>,
    /// PCM audio (38840).
    pub audio: Arc<AudioChannel>,
}

/// Receivers of everything the headset sends.
pub struct Inbound {
    /// Control messages.
    pub messages: mpsc::UnboundedReceiver<NetMessage>,
    /// Headset states.
    pub states: mpsc::Receiver<HeadsetState>,
    /// Eye-gaze samples.
    pub gaze: mpsc::Receiver<GazeSample>,
    /// Microphone PCM.
    pub mic: mpsc::Receiver<Vec<u8>>,
}

/// FFmpeg installation and selected encoder.
#[derive(Debug, Clone)]
pub struct MediaEnv {
    /// FFmpeg tools.
    pub ffmpeg: Ffmpeg,
    /// Selected encoder backend.
    pub backend: Backend,
    /// Its capabilities.
    pub caps: EncoderCaps,
}

/// Commands for the video-file player task.
#[derive(Debug, Clone)]
pub enum VideoCommand {
    /// Start playing a file.
    Play {
        /// Path relative to the videos root.
        path: String,
        /// Start position (ticks).
        position: i64,
        /// Audio stream (−1 = default).
        audio_stream: i8,
    },
    /// Seek in the current file.
    Seek(i64),
    /// Change the audio stream.
    SetAudioStream(i8),
    /// Stop playback.
    Stop,
}

/// Live streaming figures for the ABOUT tab.  Updated for every frame or headset packet, so they
/// are atomics rather than watch channels (which would wake every task at that rate).
#[derive(Debug, Default)]
pub struct StreamStats {
    /// Desktop frames sent (one per tick, whatever the number of monitors).
    pub frames: AtomicU64,
    /// Video bytes sent, all monitors.
    pub bytes: AtomicU64,
    /// Encode time of the last frame of the main monitor, in microseconds.
    pub encode_us: AtomicU32,
    /// Latency reported by the headset in its state packets, in milliseconds (C#: `Latency`,
    /// the first float of `HeadsetState` × 1000).
    pub latency_ms: AtomicI32,
    /// Encoder of the running stream: "in-process" or "ffmpeg cli" (empty when not streaming).
    pub encoder: Mutex<&'static str>,
}

/// Shared application context.
pub struct App {
    /// Persisted settings.
    pub settings: Mutex<StreamerSettings>,
    /// Shared runtime state.
    pub state: StateHub,
    /// Channels.
    pub channels: Channels,
    /// FFmpeg and encoder, set once detection finishes (`None` until then or if none works).
    pub media: OnceLock<MediaEnv>,
    /// Latest headset frame index (updated for every state packet; kept outside the watch
    /// channels to avoid waking every task at the headset frame rate).
    pub headset_frame: AtomicI32,
    /// `PeerInfo` messages forwarded from the dispatcher to the connection manager.
    pub peer_tx: mpsc::UnboundedSender<PeerInfo>,
    /// Commands for the video-file player.
    pub video_tx: mpsc::UnboundedSender<VideoCommand>,
    /// Set to `true` to shut the application down (`MessageType.Exit`, Ctrl+C).
    pub shutdown: watch::Sender<bool>,
    /// Skip the rendezvous service and listen directly (development mode).
    pub direct_mode: bool,
    /// Wakes the connection manager to re-register (accounts or connection options changed).
    pub reconnect: Notify,
    /// Headsets with an unknown account that searched the LAN, awaiting the "Allow ... to access
    /// this computer?" prompt of the settings window.
    pub pairing_requests: Mutex<VecDeque<DiscoveryRequest>>,
    /// Wakes the settings window when a pairing request is queued.
    pub pairing_notify: Notify,
    /// RSA keys of the headsets already handled (one prompt per headset and run).
    pub paired_headsets: Mutex<HashSet<String>>,
    /// Live streaming figures.
    pub stats: StreamStats,
    /// The Parsec Virtual Display Driver can be opened, so monitors can be added beyond the
    /// physical ones (checked at start-up and whenever the driver is opened).
    pub virtual_available: AtomicBool,
}

/// Receivers created together with the [`App`].
pub struct AppReceivers {
    /// Headset traffic.
    pub inbound: Inbound,
    /// Forwarded `PeerInfo`s.
    pub peers: mpsc::UnboundedReceiver<PeerInfo>,
    /// Video-player commands.
    pub video: mpsc::UnboundedReceiver<VideoCommand>,
}

impl App {
    /// Builds the context.  Media detection runs separately (see [`App::set_media`]) so the
    /// settings window can appear immediately.
    ///
    /// # Arguments
    /// * `settings` - loaded settings.
    /// * `direct_mode` - listen without the rendezvous service.
    ///
    /// # Returns
    /// The shared context and the receivers consumed by the tasks.
    pub fn new(settings: StreamerSettings, direct_mode: bool) -> (Arc<Self>, AppReceivers) {
        let (messaging, messages) = MessagingClient::new();
        let (data, states) = DataChannel::new();
        let (video, gaze) = VideoChannel::new();
        let (audio, mic) = AudioChannel::new();
        let (peer_tx, peers) = mpsc::unbounded_channel();
        let (video_tx, video_rx) = mpsc::unbounded_channel();
        let state = StateHub::new();
        state.streamer.send_modify(|s| s.auto_adjust_bitrate = settings.auto_adjust_bitrate);
        let app = Arc::new(Self {
            settings: Mutex::new(settings),
            state,
            channels: Channels { messaging, data, video, audio },
            media: OnceLock::new(),
            headset_frame: AtomicI32::new(0),
            peer_tx,
            video_tx,
            shutdown: watch::channel(false).0,
            direct_mode,
            reconnect: Notify::new(),
            pairing_requests: Mutex::default(),
            pairing_notify: Notify::new(),
            paired_headsets: Mutex::default(),
            stats: StreamStats::default(),
            virtual_available: AtomicBool::new(false),
        });
        (app, AppReceivers { inbound: Inbound { messages, states, gaze, mic }, peers, video: video_rx })
    }

    /// Records the result of encoder detection and publishes it to the shared state.
    ///
    /// # Arguments
    /// * `media` - detected environment, or `None` when FFmpeg / encoders are unavailable.
    pub fn set_media(&self, media: Option<MediaEnv>) {
        if let Some(m) = &media {
            self.state.dynamic.send_modify(|d| {
                d.adapter_name = format!("{:?} via FFmpeg {}.{}", m.backend, m.ffmpeg.version.major, m.ffmpeg.version.minor);
                d.has_hardware_encoder = m.backend.is_hardware();
            });
            self.state.streamer.send_modify(|s| {
                s.adapter_name = Some(format!("{:?}", m.backend));
                s.adapter_is_amd = m.backend == Backend::Amf;
            });
        }
        if let Some(m) = media {
            let _ = self.media.set(m);
        }
        self.state.dynamic.send_modify(|d| d.media_ready = true);
    }

    /// Applies settings edited in the settings window: stores them, mirrors the fields the
    /// headset sees, and wakes the connection manager when connection options changed.
    ///
    /// # Arguments
    /// * `new` - the edited settings.
    ///
    /// # Returns
    /// The previous settings.
    pub fn update_settings(&self, new: StreamerSettings) -> StreamerSettings {
        let old = std::mem::replace(&mut *self.settings.lock().expect("poisoned"), new.clone());
        if old.monitor_count != new.monitor_count
            || old.port.virtual_monitors != new.port.virtual_monitors
            || old.port.virtual_monitor_resolution != new.port.virtual_monitor_resolution
        {
            // Re-plan the virtual monitors and the desktop stream with the new layout.
            self.state.dynamic.send_modify(|_| {});
        }
        if old.auto_adjust_bitrate != new.auto_adjust_bitrate {
            self.state.streamer.send_modify(|s| s.auto_adjust_bitrate = new.auto_adjust_bitrate);
        }
        // C#: AllowRemoteConnectionsChanged restarts the rendezvous with the new option.
        if old.allow_remote_connections != new.allow_remote_connections && !self.state.dynamic.borrow().is_connected {
            self.reconnect.notify_one();
        }
        old
    }

    /// Drops the current session (if any) and re-registers with the new accounts.
    /// C#: `ConnectionManager` restart after `OnSaveClick`.
    pub fn restart_connection(&self) {
        self.reconnect.notify_one();
    }

    /// Latest headset frame index.
    ///
    /// # Returns
    /// The index.
    pub fn headset_frame(&self) -> i32 {
        self.headset_frame.load(Ordering::Relaxed)
    }

    /// Requests application shutdown.
    pub fn request_shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    /// Reports whether shutdown was requested.
    ///
    /// # Returns
    /// `true` after [`App::request_shutdown`].
    pub fn is_shutting_down(&self) -> bool {
        *self.shutdown.borrow()
    }

    /// Persists the settings, logging failures.
    pub fn save_settings(&self) {
        let s = self.settings.lock().expect("poisoned").clone();
        if let Err(e) = s.save() {
            warn!(error = %e, "failed to save settings");
        }
    }

    /// Sends a control message if the messaging channel is connected (errors are logged, as the
    /// original silently drops messages when disconnected).
    ///
    /// # Arguments
    /// * `msg` - message to send.
    pub async fn send(&self, msg: &NetMessage) {
        if let Err(e) = self.channels.messaging.send(msg).await {
            tracing::debug!(error = %e, %msg, "message not sent");
        }
    }
}

/// Detects FFmpeg and the best working encoder.
///
/// # Arguments
/// * `settings` - settings (backend override, render node, software fallback).
///
/// # Returns
/// The environment, or `None` if FFmpeg is missing or no encoder works.
pub async fn detect_media(settings: &StreamerSettings) -> Option<MediaEnv> {
    let ffmpeg = match Ffmpeg::discover().await {
        Ok(f) => f,
        Err(e) => {
            warn!(error = %e, "FFmpeg not available; video streaming disabled");
            return None;
        }
    };
    let node = settings.port.render_node.clone();
    let forced = settings.port.encoder_backend.as_deref().and_then(|b| match b.to_ascii_lowercase().as_str() {
        "nvenc" => Some(Backend::Nvenc),
        "amf" => Some(Backend::Amf),
        "qsv" => Some(Backend::Qsv),
        "vaapi" => Some(Backend::Vaapi),
        "videotoolbox" => Some(Backend::VideoToolbox),
        "mf" | "mediafoundation" => Some(Backend::MediaFoundation),
        "v4l2m2m" => Some(Backend::V4l2m2m),
        "software" | "x264" => Some(Backend::Software),
        _ => None,
    });
    let found = match forced {
        Some(b) => {
            let caps = probe_caps(&ffmpeg, b, &node).await;
            caps.h264.then_some((b, caps))
        }
        None => detect_backend(&ffmpeg, &node, settings.port.allow_software_encoder).await,
    };
    match found {
        Some((backend, caps)) => {
            info!(?backend, ?caps, version = ?ffmpeg.version, "encoder selected");
            Some(MediaEnv { ffmpeg, backend, caps })
        }
        None => {
            warn!("no working video encoder found; video streaming disabled");
            None
        }
    }
}
