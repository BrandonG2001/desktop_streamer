//! Application context shared by every task.  Replaces the static classes `ConnectionManager`,
//! `StreamerManager` and the `SettingsBase<T>.Default` singletons of the original.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, watch};
use tracing::{info, warn};
use vd_media::codec::{Backend, EncoderCaps, detect_backend, probe_caps};
use vd_media::version::Ffmpeg;
use vd_net::channels::{AudioChannel, DataChannel, VideoChannel};
use vd_net::messaging::MessagingClient;
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

/// Shared application context.
pub struct App {
    /// Persisted settings.
    pub settings: Mutex<StreamerSettings>,
    /// Shared runtime state.
    pub state: StateHub,
    /// Channels.
    pub channels: Channels,
    /// FFmpeg/encoder (None when FFmpeg is unavailable — video streaming is then disabled).
    pub media: Option<MediaEnv>,
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
    /// Builds the context.
    ///
    /// # Arguments
    /// * `settings` - loaded settings.
    /// * `media` - detected media environment.
    /// * `direct_mode` - listen without the rendezvous service.
    ///
    /// # Returns
    /// The shared context and the receivers consumed by the tasks.
    pub fn new(settings: StreamerSettings, media: Option<MediaEnv>, direct_mode: bool) -> (Arc<Self>, AppReceivers) {
        let (messaging, messages) = MessagingClient::new();
        let (data, states) = DataChannel::new();
        let (video, gaze) = VideoChannel::new();
        let (audio, mic) = AudioChannel::new();
        let (peer_tx, peers) = mpsc::unbounded_channel();
        let (video_tx, video_rx) = mpsc::unbounded_channel();
        let state = StateHub::new();
        if let Some(m) = &media {
            state.dynamic.send_modify(|d| {
                d.adapter_name = format!("{:?} via FFmpeg {}.{}", m.backend, m.ffmpeg.version.major, m.ffmpeg.version.minor);
                d.has_hardware_encoder = m.backend.is_hardware();
            });
            state.streamer.send_modify(|s| {
                s.adapter_name = Some(format!("{:?}", m.backend));
                s.adapter_is_amd = m.backend == Backend::Amf;
            });
        }
        state.streamer.send_modify(|s| s.auto_adjust_bitrate = settings.auto_adjust_bitrate);
        let app = Arc::new(Self {
            settings: Mutex::new(settings),
            state,
            channels: Channels { messaging, data, video, audio },
            media,
            headset_frame: AtomicI32::new(0),
            peer_tx,
            video_tx,
            shutdown: watch::channel(false).0,
            direct_mode,
        });
        (app, AppReceivers { inbound: Inbound { messages, states, gaze, mic }, peers, video: video_rx })
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
