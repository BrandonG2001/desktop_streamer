//! Audio streaming.  C#: `VirtualDesktop.Streamer.LoopbackCapture` (PC → headset) and
//! `VirtualDesktop.Streamer.Microphone` (headset → PC).
//!
//! Audio runs while a session is connected and `StreamerSettings.AudioStreaming` is not
//! `Disabled`.  PC audio is captured only while the PC is the source (desktop or VR — video
//! files carry their own audio); the microphone is forwarded whenever
//! `SharedUserSettings.MicPassthrough` is on.  A change of any of these restarts the session,
//! as the original re-initialises its capture on `AudioStreamingChanged`, `StreamingSourceChanged`
//! and `MicPassthroughChanged`.

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use vd_platform::audio::{AudioOptions, AudioSession};
use vd_protocol::channels::audio::mic_gain;
use vd_protocol::types::{AudioStreaming, StreamingSource};

use crate::app::App;

/// Computes the audio components wanted right now.
///
/// # Arguments
/// * `app` - application context.
///
/// # Returns
/// The options, or `None` when no audio should run.
fn wanted(app: &App) -> Option<AudioOptions> {
    if !app.state.dynamic.borrow().is_connected {
        return None;
    }
    let (mode, virtual_speaker, voicemeeter, select_microphone, manage, microphone_device) = {
        let s = app.settings.lock().expect("poisoned");
        (s.audio_streaming, s.use_virtual_audio_driver, s.voicemeeter_mode, s.auto_select_microphone, s.port.manage_audio_devices, s.port.microphone_device.clone())
    };
    let capture = mode != AudioStreaming::Disabled && matches!(app.state.streaming_source(), StreamingSource::Desktop | StreamingSource::VR);
    let microphone = app.state.user.borrow().mic_passthrough;
    if !capture && !microphone {
        return None;
    }
    Some(AudioOptions {
        capture,
        headset_only: mode == AudioStreaming::HeadsetOnly,
        virtual_speaker,
        voicemeeter: virtual_speaker && voicemeeter,
        microphone,
        select_microphone,
        microphone_device,
        manage_devices: manage,
    })
}

/// Audio task.
///
/// # Arguments
/// * `app` - application context.
/// * `mic_rx` - microphone PCM from the audio channel.
pub async fn run(app: Arc<App>, mut mic_rx: mpsc::Receiver<Vec<u8>>) {
    let mut dyn_rx = app.state.dynamic.subscribe();
    let mut str_rx = app.state.streamer.subscribe();
    let mut user_rx = app.state.user.subscribe();
    let mut last_failure: Option<AudioOptions> = None;
    while !app.is_shutting_down() {
        let Some(opts) = wanted(&app) else {
            last_failure = None;
            tokio::select! {
                _ = dyn_rx.changed() => {}
                _ = str_rx.changed() => {}
                _ = user_rx.changed() => {}
                _ = mic_rx.recv() => {} // drop microphone audio outside a session
            }
            continue;
        };
        if last_failure.as_ref() == Some(&opts) {
            // Nothing could be started with these options; wait for a change instead of retrying.
            tokio::select! {
                _ = dyn_rx.changed() => {}
                _ = str_rx.changed() => {}
                _ = user_rx.changed() => {}
                _ = mic_rx.recv() => {}
            }
            continue;
        }
        let mut session = match AudioSession::start(opts.clone()).await {
            Ok(s) => s,
            Err(e) => {
                // Each component already logged why it could not start.
                debug!(error = %e, "audio streaming not started");
                if opts.microphone {
                    app.state.dynamic.send_modify(|d| d.microphone = Some(String::new()));
                }
                last_failure = Some(opts);
                continue;
            }
        };
        let mic = opts.microphone.then(|| session.mic_name().unwrap_or_default().to_owned());
        app.state.dynamic.send_modify(|d| d.microphone = mic);
        info!(capture = session.has_capture(), microphone = session.has_mic(), headset_only = opts.headset_only, "audio streaming started");
        let mut sent: u64 = 0;
        loop {
            tokio::select! {
                pkt = session.next_capture() => match pkt {
                    Some(p) => {
                        if app.channels.audio.send_pcm(&p).await.is_ok() {
                            sent += 1;
                            if sent == 1 {
                                debug!("first audio packet sent");
                            }
                        }
                    }
                    None => break,
                },
                m = mic_rx.recv() => {
                    let Some(pcm) = m else { break };
                    let gain = mic_gain(app.state.user.borrow().mic_volume);
                    if let Err(e) = session.write_mic(&pcm, gain).await {
                        warn!(error = %e, "microphone playback failed");
                    }
                }
                _ = dyn_rx.changed() => if wanted(&app).as_ref() != Some(&opts) { break },
                _ = str_rx.changed() => if wanted(&app).as_ref() != Some(&opts) { break },
                _ = user_rx.changed() => if wanted(&app).as_ref() != Some(&opts) { break },
            }
        }
        session.stop().await;
        app.state.dynamic.send_modify(|d| d.microphone = None);
        info!(packets = sent, "audio streaming stopped");
    }
}
