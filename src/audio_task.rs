//! Audio streaming.  C#: `VirtualDesktop.Streamer.LoopbackCapture` (PC → headset) and
//! `VirtualDesktop.Streamer.Microphone` (headset → PC).
//!
//! Audio runs while a session is connected, `StreamerSettings.AudioStreaming` is not `Disabled`
//! and the PC is the audio source (desktop or VR; video files carry their own audio).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{info, warn};
use vd_platform::audio::{AudioRouting, LoopbackCapture, MicrophonePlayback};
use vd_protocol::channels::audio::mic_gain;
use vd_protocol::types::{AudioStreaming, StreamingSource};

use crate::app::App;

fn wanted(app: &App) -> bool {
    let mode = app.settings.lock().expect("poisoned").audio_streaming;
    app.state.dynamic.borrow().is_connected && mode != AudioStreaming::Disabled && matches!(app.state.streaming_source(), StreamingSource::Desktop | StreamingSource::VR)
}

/// Audio task.
///
/// # Arguments
/// * `app` - application context.
/// * `mic_rx` - microphone PCM from the audio channel.
pub async fn run(app: Arc<App>, mut mic_rx: mpsc::Receiver<Vec<u8>>) {
    let mut dyn_rx = app.state.dynamic.subscribe();
    let mut str_rx = app.state.streamer.subscribe();
    let mut warned = false;
    while !app.is_shutting_down() {
        if !wanted(&app) {
            // Drain microphone audio that arrives outside a session.
            tokio::select! {
                _ = dyn_rx.changed() => {}
                _ = str_rx.changed() => {}
                _ = mic_rx.recv() => {}
            }
            continue;
        }
        let (headset_only, manage) = {
            let s = app.settings.lock().expect("poisoned");
            (s.audio_streaming == AudioStreaming::HeadsetOnly && s.use_virtual_audio_driver, s.port.manage_audio_devices)
        };
        let mic_on = app.state.user.borrow().mic_passthrough;
        let routing = if manage {
            match AudioRouting::setup(headset_only, mic_on).await {
                Ok(r) => Some(r),
                Err(e) => {
                    if !warned {
                        warn!(error = %e, "audio device routing unavailable");
                    }
                    None
                }
            }
        } else {
            None
        };
        let capture_dev = routing.as_ref().and_then(|r| r.capture_device.clone());
        let mut capture = match LoopbackCapture::start(capture_dev.as_deref()) {
            Ok(c) => Some(c),
            Err(e) => {
                if !warned {
                    warn!(error = %e, "PC audio capture unavailable");
                }
                None
            }
        };
        let mut mic = routing.as_ref().and_then(|r| r.mic_sink.as_deref()).and_then(|sink| MicrophonePlayback::start(sink).map_err(|e| warn!(error = %e, "virtual microphone unavailable")).ok());
        warned = true;
        if capture.is_none() && mic.is_none() {
            if let Some(r) = routing {
                r.restore().await;
            }
            // Nothing works on this platform; wait for the session to end.
            while wanted(&app) {
                tokio::select! {
                    _ = dyn_rx.changed() => {}
                    _ = str_rx.changed() => {}
                    _ = mic_rx.recv() => {}
                }
            }
            continue;
        }
        info!(capture = capture.is_some(), mic = mic.is_some(), "audio streaming started");
        loop {
            tokio::select! {
                pkt = async { match capture.as_mut() { Some(c) => c.next().await, None => std::future::pending().await } } => {
                    match pkt {
                        Some(p) => { let _ = app.channels.audio.send_pcm(&p).await; }
                        None => { tokio::time::sleep(Duration::from_millis(200)).await; break; }
                    }
                }
                m = mic_rx.recv() => {
                    let Some(pcm) = m else { break };
                    if let Some(player) = mic.as_mut() {
                        let gain = mic_gain(app.state.user.borrow().mic_volume);
                        if player.write(&pcm, gain).await.is_err() {
                            mic = None;
                        }
                    }
                }
                _ = dyn_rx.changed() => if !wanted(&app) { break },
                _ = str_rx.changed() => if !wanted(&app) { break },
            }
        }
        if let Some(c) = capture.as_mut() {
            c.stop().await;
        }
        if let Some(m) = mic.as_mut() {
            m.stop().await;
        }
        if let Some(r) = routing {
            r.restore().await;
        }
        info!("audio streaming stopped");
    }
}
