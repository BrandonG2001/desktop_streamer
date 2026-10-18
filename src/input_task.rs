//! Headset state consumer.  C#: the "ReceiveFrameData Thread" of `StreamerManager`, which copies
//! the input part of every headset state into the `VirtualDesktop.InputInjection` shared memory
//! for the injection service, and records the frame index used in frame headers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;
use tracing::warn;
use vd_platform::input::{DesktopBounds, InputBackend, InputEvent, InputTranslator, NullBackend, open_backend};
use vd_protocol::headset_state::{GamepadState, HeadsetState};
use vd_protocol::types::StreamingSource;

use crate::app::App;

static RELEASE_GAMEPAD: AtomicBool = AtomicBool::new(false);

/// Requests that the virtual gamepad be reset (`DisposeGamepadEmulation`).
pub fn release_gamepad() {
    RELEASE_GAMEPAD.store(true, Ordering::Relaxed);
}

/// Opens the input backend for the current monitor layout.
///
/// # Arguments
/// * `app` - application context.
///
/// # Returns
/// The backend, or a [`NullBackend`] when injection is unavailable (logged).
fn open(app: &App) -> Box<dyn InputBackend> {
    let monitors = app.state.dynamic.borrow().monitors.clone();
    let (x, y, width, height) = vd_platform::display::virtual_desktop_bounds(&monitors);
    let gamepad = app.state.user.borrow().emulate_gamepad;
    match open_backend(DesktopBounds { x, y, width, height }, gamepad) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "input injection unavailable");
            Box::new(NullBackend)
        }
    }
}

/// Input task.
///
/// # Arguments
/// * `app` - application context.
/// * `states` - decoded headset states from the data channel.
pub async fn run(app: Arc<App>, mut states: mpsc::Receiver<HeadsetState>) {
    let mut translator = InputTranslator::new((0, 0));
    let mut backend: Option<Box<dyn InputBackend>> = None;
    let mut dyn_rx = app.state.dynamic.subscribe();
    loop {
        tokio::select! {
            s = states.recv() => {
                let Some(state) = s else { return };
                app.headset_frame.store(state.frame_index, Ordering::Relaxed);
                if app.state.streaming_source() != StreamingSource::Desktop {
                    continue;
                }
                let b = backend.get_or_insert_with(|| open(&app));
                {
                    let d = app.state.dynamic.borrow();
                    if let Some(m) = d.monitors.get(d.monitor_start_index) {
                        translator.origin = (m.x, m.y);
                    }
                }
                let mut events = translator.translate(&state.input, app.state.user.borrow().emulate_gamepad);
                if RELEASE_GAMEPAD.swap(false, Ordering::Relaxed) {
                    events.push(InputEvent::Gamepad(GamepadState::default()));
                }
                if !events.is_empty() {
                    if let Err(e) = b.emit(&events) {
                        warn!(error = %e, "input injection failed");
                    }
                }
            }
            r = dyn_rx.changed() => {
                if r.is_err() { return }
                if !dyn_rx.borrow().is_connected {
                    // C#: IInputInjection.ReleaseInput() when the session ends.
                    if let Some(b) = backend.as_mut() {
                        let _ = b.emit(&translator.release_all());
                    }
                    backend = None;
                }
            }
        }
    }
}
