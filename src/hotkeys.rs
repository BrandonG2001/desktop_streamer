//! System-wide hotkeys for the [`crate::bindings`] actions.  C#: `BindingSettings.HotKeyNativeWindow`
//! (`RegisterHotKey` on a hidden window).
//!
//! `global-hotkey` registers the keys on the thread that runs the window event loop (Windows
//! `RegisterHotKey`, X11 `XGrabKey`), so [`Hotkeys`] is created by the settings window.  Presses
//! are forwarded to [`run`] on the async runtime, which performs the action.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::app::App;
use crate::bindings::{BindingAction, BindingSettings, BindingShortcut, modifiers};

/// Converts a virtual-key code to a `global-hotkey` key.
///
/// # Arguments
/// * `vk` - virtual-key code from [`crate::bindings::KEYS`].
///
/// # Returns
/// The key, or `None` for unsupported codes.
fn code(vk: u32) -> Option<Code> {
    const LETTERS: [Code; 26] = [
        Code::KeyA, Code::KeyB, Code::KeyC, Code::KeyD, Code::KeyE, Code::KeyF, Code::KeyG, Code::KeyH, Code::KeyI, Code::KeyJ, Code::KeyK,
        Code::KeyL, Code::KeyM, Code::KeyN, Code::KeyO, Code::KeyP, Code::KeyQ, Code::KeyR, Code::KeyS, Code::KeyT, Code::KeyU, Code::KeyV,
        Code::KeyW, Code::KeyX, Code::KeyY, Code::KeyZ,
    ];
    const DIGITS: [Code; 10] =
        [Code::Digit0, Code::Digit1, Code::Digit2, Code::Digit3, Code::Digit4, Code::Digit5, Code::Digit6, Code::Digit7, Code::Digit8, Code::Digit9];
    const FKEYS: [Code; 12] = [Code::F1, Code::F2, Code::F3, Code::F4, Code::F5, Code::F6, Code::F7, Code::F8, Code::F9, Code::F10, Code::F11, Code::F12];
    match vk {
        0x41..=0x5A => Some(LETTERS[(vk - 0x41) as usize]),
        0x30..=0x39 => Some(DIGITS[(vk - 0x30) as usize]),
        0x70..=0x7B => Some(FKEYS[(vk - 0x70) as usize]),
        _ => None,
    }
}

/// Converts a shortcut to a `global-hotkey` hotkey.
///
/// # Arguments
/// * `s` - the shortcut.
///
/// # Returns
/// The hotkey, or `None` if the key is unsupported.
fn hotkey(s: &BindingShortcut) -> Option<HotKey> {
    let mut m = Modifiers::empty();
    for (bit, flag) in [(modifiers::ALT, Modifiers::ALT), (modifiers::CONTROL, Modifiers::CONTROL), (modifiers::SHIFT, Modifiers::SHIFT), (modifiers::WINDOWS, Modifiers::META)] {
        if s.modifiers & bit != 0 {
            m |= flag;
        }
    }
    Some(HotKey::new(Some(m), code(s.key_code)?))
}

/// Registered hotkeys.
pub struct Hotkeys {
    manager: GlobalHotKeyManager,
    /// Hotkey id → action, shared with the event handler.
    actions: Arc<Mutex<HashMap<u32, BindingAction>>>,
    registered: Vec<HotKey>,
}

impl Hotkeys {
    /// Creates the hotkey manager and forwards presses to `tx`.  Must be called on the thread
    /// that runs the window event loop.
    ///
    /// # Arguments
    /// * `tx` - receives triggered actions.
    ///
    /// # Returns
    /// The manager, or `None` if the platform has no global hotkeys (e.g. Wayland without
    /// XWayland), which is logged.
    pub fn new(tx: mpsc::UnboundedSender<BindingAction>) -> Option<Self> {
        let manager = match GlobalHotKeyManager::new() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "global hotkeys unavailable");
                return None;
            }
        };
        let actions: Arc<Mutex<HashMap<u32, BindingAction>>> = Arc::default();
        let map = Arc::clone(&actions);
        GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
            if e.state == HotKeyState::Pressed
                && let Some(a) = map.lock().expect("poisoned").get(&e.id) {
                    let _ = tx.send(*a);
                }
        }));
        Some(Self { manager, actions, registered: Vec::new() })
    }

    /// Replaces the registered hotkeys with `settings`.
    ///
    /// # Arguments
    /// * `settings` - shortcuts to register.
    ///
    /// # Returns
    /// One message per shortcut that could not be registered (usually taken by another app).
    pub fn apply(&mut self, settings: &BindingSettings) -> Vec<String> {
        let _ = self.manager.unregister_all(&self.registered);
        self.registered.clear();
        let mut actions = self.actions.lock().expect("poisoned");
        actions.clear();
        let mut errors = Vec::new();
        if !settings.hotkeys_enabled {
            return errors;
        }
        for action in BindingAction::ALL {
            let shortcut = settings.get(action);
            let Some(hk) = hotkey(&shortcut) else { continue };
            match self.manager.register(hk) {
                Ok(()) => {
                    actions.insert(hk.id(), action);
                    self.registered.push(hk);
                }
                Err(e) => {
                    debug!(error = %e, ?action, "hotkey registration failed");
                    errors.push(format!("{} ({}) is in use by another application", action.label(), shortcut.display()));
                }
            }
        }
        info!(count = self.registered.len(), "hotkeys registered");
        errors
    }
}

/// Performs hotkey actions as they arrive.
///
/// # Arguments
/// * `app` - application context.
/// * `rx` - actions from [`Hotkeys`].
pub async fn run(app: Arc<App>, mut rx: mpsc::UnboundedReceiver<BindingAction>) {
    let mut last_vr_toggle: Option<Instant> = None;
    while let Some(action) = rx.recv().await {
        crate::bindings::perform(&app, action, &mut last_vr_toggle).await;
    }
}
