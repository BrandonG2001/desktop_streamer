//! Starts and stops the streamer's background tasks.  Shared by the settings window (GUI mode)
//! and the headless `run` command.  C#: `App.OnStartup` → `StreamerManager` / `ConnectionManager`
//! initialisation.

use std::sync::Arc;

use tracing::{info, warn};

use crate::app::{self, App};
use crate::settings::StreamerSettings;
use crate::{audio_task, connection, desktop, dispatch, input_task, videos};

/// Creates the application context and spawns every task on the current Tokio runtime.
///
/// Encoder detection runs in the background; the connection manager starts once it finishes so
/// a headset never connects to a streamer that does not know its encoder yet.
///
/// # Arguments
/// * `settings` - loaded settings.
/// * `direct` - direct (relay-less) mode.
///
/// # Returns
/// The shared context.
pub fn start(settings: StreamerSettings, direct: bool) -> Arc<App> {
    let (app, rx) = App::new(settings, direct);
    {
        let monitors = desktop::list_monitors((1920, 1080));
        info!(count = monitors.len(), "monitors detected");
        app.state.dynamic.send_modify(|d| d.monitors = monitors);
    }

    tokio::spawn(dispatch::run(Arc::clone(&app), rx.inbound.messages));
    tokio::spawn(dispatch::sync_streamer_settings(Arc::clone(&app)));
    tokio::spawn(desktop::run(Arc::clone(&app)));
    tokio::spawn(audio_task::run(Arc::clone(&app), rx.inbound.mic));
    tokio::spawn(input_task::run(Arc::clone(&app), rx.inbound.states));
    tokio::spawn(videos::run(Arc::clone(&app), rx.video));
    tokio::spawn(session_side_effects(Arc::clone(&app)));
    // Gaze samples drive the dynamic foveation map of the VR encoder, which is not ported; drain them.
    tokio::spawn(async move {
        let mut gaze = rx.inbound.gaze;
        while gaze.recv().await.is_some() {}
    });
    let peers = rx.peers;
    let conn_app = Arc::clone(&app);
    tokio::spawn(async move {
        let settings = conn_app.settings.lock().expect("poisoned").clone();
        let media = app::detect_media(&settings).await;
        conn_app.set_media(media);
        connection::run(conn_app, peers).await;
    });
    app
}

/// Stops the streamer: disconnects the headset and saves the settings.
///
/// # Arguments
/// * `app` - application context.
pub async fn stop(app: &App) {
    app.request_shutdown();
    app.channels.messaging.reset().await;
    app.save_settings();
    info!("streamer stopped");
}

/// Starts/stops session-scoped helpers: Wi-Fi optimisation (`StartWLANOptimizerThread`),
/// priority boost (`IServiceManager.UseRealtimePriority`) and the workstation lock on
/// disconnect (`StreamerSettings.LockComputer`).
///
/// # Arguments
/// * `app` - application context.
async fn session_side_effects(app: Arc<App>) {
    let mut rx = app.state.dynamic.subscribe();
    let mut optimizer: Option<vd_platform::wlan::WlanOptimizer> = None;
    loop {
        let connected = rx.borrow().is_connected;
        if connected && optimizer.is_none() {
            optimizer = Some(vd_platform::wlan::WlanOptimizer::start());
            if app.settings.lock().expect("poisoned").boost_priority {
                if let Err(e) = vd_platform::process::set_high_priority(true) {
                    warn!(error = %e, "cannot raise process priority");
                }
            }
        } else if !connected {
            if let Some(o) = optimizer.take() {
                tokio::task::spawn_blocking(move || o.stop());
                let _ = vd_platform::process::set_high_priority(false);
                if app.settings.lock().expect("poisoned").lock_computer && !app.is_shutting_down() {
                    if let Err(e) = vd_platform::system::lock_workstation() {
                        warn!(error = %e, "cannot lock the computer");
                    }
                }
            }
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}
