//! `SendInput` backend (Windows).
//!
//! Mirrors what `VirtualDesktop.Server.exe` does with `SendInput`/`keybd_event`: keys are sent as
//! scan codes (`KEYEVENTF_SCANCODE`, plus `KEYEVENTF_EXTENDEDKEY` for `0xE0xx`), absolute pointer
//! positions are normalised to `0..65535` over the virtual desktop, and wheel deltas use
//! `WHEEL_DELTA` (120) units.  Virtual gamepads require the `vdvge.sys`/ViGEm driver of the
//! original installation and are not provided by this port on Windows.

use std::mem::size_of;

use tracing::warn;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput,
};

use super::{DesktopBounds, InputBackend, InputEvent, MouseButton};
use crate::error::{PlatformError, Result};
use crate::keymap::scan_to_win32;

/// `SendInput` based injector.
pub struct SendInputBackend {
    bounds: DesktopBounds,
    warned_gamepad: bool,
}

impl SendInputBackend {
    /// Creates the backend.
    ///
    /// # Arguments
    /// * `bounds` - virtual desktop bounds (for absolute normalisation).
    /// * `_gamepad` - ignored (no virtual gamepad on Windows in this port).
    ///
    /// # Returns
    /// The backend.
    pub fn new(bounds: DesktopBounds, _gamepad: bool) -> Self {
        Self { bounds, warned_gamepad: false }
    }

    fn mouse(dx: i32, dy: i32, data: i32, flags: u32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data as _, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        }
    }

    fn key(scan: u16, down: bool) -> INPUT {
        let (sc, ext) = scan_to_win32(scan);
        let mut flags = KEYEVENTF_SCANCODE;
        if ext {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if !down {
            flags |= KEYEVENTF_KEYUP;
        }
        INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: 0, wScan: sc, dwFlags: flags, time: 0, dwExtraInfo: 0 } } }
    }
}

impl InputBackend for SendInputBackend {
    /// Injects events with one `SendInput` call.
    ///
    /// # Arguments
    /// * `events` - events.
    ///
    /// # Errors
    /// [`PlatformError::Win32`] if `SendInput` inserts fewer events than requested (blocked by
    /// UIPI when a higher-integrity window has focus).
    fn emit(&mut self, events: &[InputEvent]) -> Result<()> {
        let mut inputs = Vec::with_capacity(events.len());
        for e in events {
            match e {
                InputEvent::Key { scan, down } => inputs.push(Self::key(*scan, *down)),
                InputEvent::MouseMove { dx, dy } => inputs.push(Self::mouse(*dx, *dy, 0, MOUSEEVENTF_MOVE)),
                InputEvent::PointerAbs { x, y } => {
                    let nx = ((x - self.bounds.x) as i64 * 65535 / i64::from(self.bounds.width.max(2) - 1)) as i32;
                    let ny = ((y - self.bounds.y) as i64 * 65535 / i64::from(self.bounds.height.max(2) - 1)) as i32;
                    inputs.push(Self::mouse(nx, ny, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK));
                }
                InputEvent::Button { button, down } => {
                    let (flags, data) = match (button, down) {
                        (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                        (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
                        (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                        (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
                        (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                        (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                        (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, 1),
                        (MouseButton::Back, false) => (MOUSEEVENTF_XUP, 1),
                        (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, 2),
                        (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, 2),
                    };
                    inputs.push(Self::mouse(0, 0, data, flags));
                }
                InputEvent::Wheel { vertical, horizontal } => {
                    if *vertical != 0 {
                        inputs.push(Self::mouse(0, 0, *vertical, MOUSEEVENTF_WHEEL));
                    }
                    if *horizontal != 0 {
                        inputs.push(Self::mouse(0, 0, *horizontal, MOUSEEVENTF_HWHEEL));
                    }
                }
                InputEvent::Gamepad(state) => {
                    // Neutral states (sent on release) carry no input worth warning about.
                    if !self.warned_gamepad && *state != vd_protocol::headset_state::GamepadState::default() {
                        self.warned_gamepad = true;
                        warn!("gamepad emulation on Windows requires the original vdvge/ViGEm driver; ignoring gamepad input");
                    }
                }
            }
        }
        if inputs.is_empty() {
            return Ok(());
        }
        // SAFETY: `inputs` is a valid, initialised slice of INPUT structures.
        let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
        if sent as usize != inputs.len() {
            return Err(PlatformError::Win32 { api: "SendInput", code: std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32 });
        }
        Ok(())
    }
}
