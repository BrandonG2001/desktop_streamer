//! Control-message handling.  C#: `ConnectionManager._0007` (the `NetMessage` handler state
//! machine), `NetworkSettingsBase<T>.OnMessage`, `VideoStreamer` and `OculusStreamer`
//! message handlers.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use vd_protocol::messages::Request;
use vd_protocol::settings::{SharedMobileSettings, SharedStreamerSettings, SharedUserSettings, SyncedSettings, apply_property, apply_reload, diff_messages};
use vd_protocol::{MessageType, NetMessage};

use crate::app::{App, VideoCommand};

/// Applies a settings message to the matching shared object.
///
/// # Arguments
/// * `app` - application context.
/// * `msg` - message.
///
/// # Returns
/// `true` if the message was a settings message.
fn handle_settings(app: &App, msg: &NetMessage) -> bool {
    fn apply<T: SyncedSettings>(tx: &tokio::sync::watch::Sender<T>, msg: &NetMessage) {
        let res = if msg.raw_type == T::RELOAD as u8 {
            apply_reload::<T>(msg).map(|v| {
                tx.send_replace(v);
            })
        } else {
            let cur = tx.borrow().clone();
            apply_property(&cur, msg).map(|(v, names)| {
                debug!(?names, "settings property changed");
                tx.send_replace(v);
            })
        };
        if let Err(e) = res {
            warn!(error = %e, "invalid settings message");
        }
    }
    match msg.message_type() {
        Some(MessageType::UserSettingsReload | MessageType::UserSettingsPropertyChanged) => apply::<SharedUserSettings>(&app.state.user, msg),
        Some(MessageType::MobileSettingsReload | MessageType::MobileSettingsPropertyChanged) => {
            apply::<SharedMobileSettings>(&app.state.mobile, msg);
            let m = app.state.mobile.borrow();
            info!(hmd = m.hmd_type.display_name(), resolution = ?(m.resolution.width, m.resolution.height), bandwidth = m.measured_bandwidth, "headset description");
        }
        Some(MessageType::StreamerSettingsReload | MessageType::StreamerSettingsPropertyChanged) => {
            // The headset may change PC-owned properties (e.g. StreamingSource).  Apply them
            // without echoing them back.
            if msg.raw_type == MessageType::StreamerSettingsPropertyChanged as u8 {
                let cur = app.state.streamer.borrow().clone();
                if let Ok((v, _)) = apply_property(&cur, msg) {
                    LAST_SENT.with_value(|last| *last = Some(v.clone()));
                    app.state.streamer.send_replace(v);
                }
            }
        }
        _ => return false,
    }
    true
}

/// Holder of the last `SharedStreamerSettings` sent to the headset (used to compute diffs).
struct LastSent(std::sync::Mutex<Option<SharedStreamerSettings>>);

impl LastSent {
    fn with_value<R>(&self, f: impl FnOnce(&mut Option<SharedStreamerSettings>) -> R) -> R {
        f(&mut self.0.lock().expect("poisoned"))
    }
}

static LAST_SENT: LastSent = LastSent(std::sync::Mutex::new(None));

/// Mirrors local changes of `SharedStreamerSettings` to the headset (`OnPropertyChanged` →
/// `*PropertyChanged` message).
///
/// # Arguments
/// * `app` - application context.
pub async fn sync_streamer_settings(app: Arc<App>) {
    let mut rx = app.state.streamer.subscribe();
    let mut dyn_rx = app.state.dynamic.subscribe();
    let mut was_connected = false;
    loop {
        tokio::select! {
            r = rx.changed() => if r.is_err() { return },
            r = dyn_rx.changed() => {
                if r.is_err() { return }
                // A new session receives a full reload from `connection::on_connected`; diffs
                // start from that snapshot.
                let connected = dyn_rx.borrow().is_connected;
                if connected && !was_connected {
                    LAST_SENT.with_value(|l| *l = Some(app.state.streamer.borrow().clone()));
                }
                was_connected = connected;
                continue;
            }
        }
        if !app.state.dynamic.borrow().is_connected {
            continue;
        }
        let cur = rx.borrow().clone();
        let msgs = LAST_SENT.with_value(|last| {
            let msgs = match last.as_ref() {
                Some(prev) => diff_messages(prev, &cur).unwrap_or_default(),
                None => Vec::new(),
            };
            *last = Some(cur.clone());
            msgs
        });
        for m in msgs {
            app.send(&m).await;
        }
    }
}

/// Saves a screenshot sent by the headset.  C#: `ConnectionManager` `ScreenshotToDesktop` handler.
///
/// # Arguments
/// * `app` - application context.
/// * `name` - file name chosen by the headset.
/// * `data` - image bytes.
fn save_screenshot(app: &App, name: &str, data: &[u8]) {
    let root = app
        .settings
        .lock()
        .expect("poisoned")
        .screenshots_root_path
        .clone()
        .map(PathBuf::from)
        .or_else(dirs::picture_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let file = root.join(std::path::Path::new(name).file_name().unwrap_or_default());
    match std::fs::create_dir_all(&root).and_then(|_| std::fs::write(&file, data)) {
        Ok(()) => info!(path = %file.display(), "screenshot saved"),
        Err(e) => error!(error = %e, "cannot save screenshot"),
    }
}

/// Appends a headset log line to `Desktop/Log.txt` (C#: `MessageType.Log` handler).
///
/// # Arguments
/// * `text` - log text.
fn append_log(text: &str) {
    use std::io::Write;
    let path = dirs::desktop_dir().unwrap_or_else(|| PathBuf::from(".")).join("Log.txt");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{text}");
    }
}

/// Dispatches control messages until the receiver closes.
///
/// # Arguments
/// * `app` - application context.
/// * `rx` - inbound messages.
pub async fn run(app: Arc<App>, mut rx: mpsc::UnboundedReceiver<NetMessage>) {
    while let Some(msg) = rx.recv().await {
        if handle_settings(&app, &msg) {
            continue;
        }
        let req = match Request::decode(&msg) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, %msg, "malformed message");
                continue;
            }
        };
        debug!(?req, "request");
        match req {
            Request::PeerInfo(p) => {
                let _ = app.peer_tx.send(p);
            }
            Request::CrashCallstack(m, stack) => error!(message = %m, %stack, "headset crash report"),
            Request::Simple(MessageType::UpdateRequired) => app.send(&NetMessage::new(MessageType::UpdateUnavailable)).await,
            Request::Simple(MessageType::RestartStreaming | MessageType::BeginStreaming) => app.request_keyframe_all(),
            Request::Simple(MessageType::SwitchMonitor) => {
                app.state.dynamic.send_modify(|d| {
                    if !d.monitors.is_empty() {
                        d.monitor_start_index = (d.monitor_start_index + 1) % d.monitors.len();
                    }
                });
                app.request_keyframe_all();
            }
            Request::Simple(MessageType::Exit) => {
                info!("headset requested exit");
                app.request_shutdown();
            }
            Request::Simple(MessageType::Disconnect) => app.channels.messaging.reset().await,
            Request::Simple(MessageType::DisposeGamepadEmulation) => crate::input_task::release_gamepad(),
            Request::Simple(t @ (MessageType::AddMonitor | MessageType::RemoveMonitor)) => warn!(?t, "virtual monitors are not supported by this port"),
            Request::Simple(t @ (MessageType::ToggleDictation | MessageType::ChangeLanguage)) => debug!(?t, "Windows shell shortcut not supported"),
            Request::SetMonitorPortrait { index, portrait } => warn!(index, portrait, "monitor rotation is not supported by this port"),
            Request::Screenshot { name, data } => save_screenshot(&app, &name, &data),
            Request::WakeUp(macs) => {
                let ok = vd_net::discovery::wake_on_lan(&macs).await.unwrap_or(false);
                if ok {
                    app.send(&NetMessage::new(MessageType::ComputerAwaken)).await;
                }
            }
            Request::RequestCursor(_) => {}
            Request::Log(text) => append_log(&text),
            Request::RequestVideos(path) => crate::videos::send_listing(&app, &path).await,
            Request::PlayVideo { path, position, audio_stream } => {
                let _ = app.video_tx.send(VideoCommand::Play { path, position, audio_stream });
            }
            Request::SeekVideo(pos) => {
                let _ = app.video_tx.send(VideoCommand::Seek(pos));
            }
            Request::SetAudioStream(i) => {
                let _ = app.video_tx.send(VideoCommand::SetAudioStream(i));
            }
            Request::Simple(MessageType::StopVideo) => {
                let _ = app.video_tx.send(VideoCommand::Stop);
            }
            Request::Simple(MessageType::RequestGames) => crate::games::send_games(&app).await,
            Request::LaunchGame(id) => crate::games::launch(&app, id).await,
            Request::ExitGame(id) => debug!(?id, "exit game is not supported by this port"),
            other => debug!(?other, "unhandled message"),
        }
    }
}
