//! `SendInput` backend (Windows).
//!
//! Mirrors what `VirtualDesktop.Server.exe` does with `SendInput`/`keybd_event`: keys are sent as
//! scan codes (`KEYEVENTF_SCANCODE`, plus `KEYEVENTF_EXTENDEDKEY` for `0xE0xx`), absolute pointer
//! positions are normalised to `0..65535` over the virtual desktop, and wheel deltas use
//! `WHEEL_DELTA` (120) units.  Touch contacts use `InjectTouchInput` with up to two contacts, in
//! the frame sequences of `VirtualDesktop.Server.exe` (`_0008._0002`).  Virtual gamepads require
//! the `vdvge.sys`/ViGEm driver of the original installation and are not provided by this port
//! on Windows.

use std::mem::size_of;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tracing::warn;
use windows_sys::Win32::Foundation::{POINT, RECT};
use windows_sys::Win32::UI::Input::Pointer::{
    InitializeTouchInjection, InjectTouchInput, POINTER_FLAG_DOWN, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE, POINTER_FLAG_UP, POINTER_FLAG_UPDATE, POINTER_FLAGS,
    POINTER_INFO, POINTER_TOUCH_INFO, TOUCH_FEEDBACK_DEFAULT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{PT_TOUCH, TOUCH_FLAG_NONE, TOUCH_MASK_CONTACTAREA};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput,
};

use super::{DesktopBounds, InputBackend, InputEvent, MouseButton};
use crate::error::{PlatformError, Result};
use crate::keymap::scan_to_win32;

/// Half the side of the contact rectangle of a touch contact (C#: an 8×8 `rcContact`).
const CONTACT_HALF: i32 = 4;

/// Longest gap between frames of a held touch contact; Windows cancels contacts that are not
/// updated (the original injects every millisecond).
const TOUCH_REFRESH: Duration = Duration::from_millis(50);

/// Whether touch injection is available (`InitializeTouchInjection` succeeds once per process).
///
/// # Returns
/// `true` if contacts can be injected.
fn touch_injection() -> bool {
    static READY: OnceLock<bool> = OnceLock::new();
    // SAFETY: plain call; two contacts with the default visual feedback, as the original.
    *READY.get_or_init(|| unsafe { InitializeTouchInjection(2, TOUCH_FEEDBACK_DEFAULT) } != 0)
}

/// The two touch contacts (C#: `POINTER_TOUCH_INFO[2]` of `_0008._0002`).
struct TouchContacts {
    /// Contact 0 and 1; a contact with no pointer flags is not injected.
    contacts: [POINTER_TOUCH_INFO; 2],
    /// Time of the last injected frame.
    last_frame: Instant,
}

// SAFETY: the only pointers inside are `sourceDevice` and `hwndTarget`, which stay null.
unsafe impl Send for TouchContacts {}

impl TouchContacts {
    /// Creates both contacts, idle.
    ///
    /// # Returns
    /// The contacts.
    fn new() -> Self {
        let contact = |id: u32| POINTER_TOUCH_INFO {
            // SAFETY: POINTER_INFO is plain data for which all-zero is valid.
            pointerInfo: POINTER_INFO { pointerType: PT_TOUCH, pointerId: id, ..unsafe { std::mem::zeroed() } },
            touchFlags: TOUCH_FLAG_NONE,
            touchMask: TOUCH_MASK_CONTACTAREA,
            rcContact: RECT { left: 0, top: 0, right: 0, bottom: 0 },
            rcContactRaw: RECT { left: 0, top: 0, right: 0, bottom: 0 },
            orientation: 0,
            pressure: 0,
        };
        Self { contacts: [contact(0), contact(1)], last_frame: Instant::now() }
    }

    /// Moves a contact.
    ///
    /// # Arguments
    /// * `id` - contact (0 or 1).
    /// * `x`, `y` - position in physical virtual-desktop pixels.
    fn place(&mut self, id: usize, x: i32, y: i32) {
        let c = &mut self.contacts[id];
        c.pointerInfo.ptPixelLocation = POINT { x, y };
        c.rcContact = RECT { left: x - CONTACT_HALF, top: y - CONTACT_HALF, right: x + CONTACT_HALF, bottom: y + CONTACT_HALF };
    }

    /// Injects one frame holding every contact with pointer flags.
    ///
    /// # Errors
    /// [`PlatformError::Win32`] if `InjectTouchInput` fails.
    fn frame(&mut self) -> Result<()> {
        let active: Vec<POINTER_TOUCH_INFO> = self.contacts.iter().filter(|c| c.pointerInfo.pointerFlags != 0).copied().collect();
        if active.is_empty() {
            return Ok(());
        }
        self.last_frame = Instant::now();
        // SAFETY: `active` is a valid array of `active.len()` initialised contacts.
        if unsafe { InjectTouchInput(active.len() as u32, active.as_ptr()) } == 0 {
            return Err(PlatformError::Win32 { api: "InjectTouchInput", code: std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32 });
        }
        Ok(())
    }

    /// Injects `flags` for contact `id` in successive frames.
    ///
    /// # Arguments
    /// * `id` - contact.
    /// * `sequence` - pointer flags of each frame.
    ///
    /// # Errors
    /// [`PlatformError::Win32`] if a frame is rejected; the contact is then reset.
    fn run(&mut self, id: usize, sequence: &[POINTER_FLAGS]) -> Result<()> {
        for flags in sequence {
            self.contacts[id].pointerInfo.pointerFlags = *flags;
            if let Err(e) = self.frame() {
                self.contacts[id].pointerInfo.pointerFlags = 0;
                return Err(e);
            }
        }
        Ok(())
    }

    /// Handles a touch event (C#: the flag sequences of `_0008._0002._0001(in, in)`).
    ///
    /// # Arguments
    /// * `event` - [`InputEvent::TouchDown`], [`InputEvent::TouchMove`] or [`InputEvent::TouchUp`].
    ///
    /// # Errors
    /// [`PlatformError::Win32`] if a frame is rejected.
    fn apply(&mut self, event: &InputEvent) -> Result<()> {
        const HOVER: POINTER_FLAGS = POINTER_FLAG_INRANGE | POINTER_FLAG_UPDATE;
        const HELD: POINTER_FLAGS = POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_UPDATE;
        match *event {
            InputEvent::TouchDown { id, x, y } if id < 2 => {
                self.place(id as usize, x, y);
                self.run(id as usize, &[HOVER, POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_DOWN, HELD])
            }
            InputEvent::TouchMove { id, x, y } if id < 2 => {
                self.place(id as usize, x, y);
                self.run(id as usize, &[HELD])
            }
            InputEvent::TouchUp { id } if id < 2 && self.contacts[id as usize].pointerInfo.pointerFlags != 0 => {
                let r = self.run(id as usize, &[POINTER_FLAG_INRANGE | POINTER_FLAG_UP, HOVER, POINTER_FLAG_UPDATE]);
                self.contacts[id as usize].pointerInfo.pointerFlags = 0;
                r
            }
            _ => Ok(()),
        }
    }

    /// Re-injects held contacts that have not been updated for [`TOUCH_REFRESH`].
    ///
    /// # Errors
    /// [`PlatformError::Win32`] if the frame is rejected.
    fn refresh(&mut self) -> Result<()> {
        if self.last_frame.elapsed() < TOUCH_REFRESH || !self.contacts.iter().any(|c| c.pointerInfo.pointerFlags & POINTER_FLAG_INCONTACT != 0) {
            return Ok(());
        }
        self.frame()
    }
}

/// `SendInput` based injector.
pub struct SendInputBackend {
    bounds: DesktopBounds,
    warned_gamepad: bool,
    /// Touch contacts, when touch injection is available.
    touch: Option<TouchContacts>,
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
        Self { bounds, warned_gamepad: false, touch: touch_injection().then(TouchContacts::new) }
    }

    /// Sends and clears the pending `SendInput` events.
    ///
    /// # Arguments
    /// * `inputs` - pending events.
    ///
    /// # Errors
    /// [`PlatformError::Win32`] if `SendInput` inserts fewer events than requested (blocked by
    /// UIPI when a higher-integrity window has focus).
    fn send(inputs: &mut Vec<INPUT>) -> Result<()> {
        if inputs.is_empty() {
            return Ok(());
        }
        // SAFETY: `inputs` is a valid, initialised slice of INPUT structures.
        let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
        let all = sent as usize == inputs.len();
        inputs.clear();
        if !all {
            return Err(PlatformError::Win32 { api: "SendInput", code: std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32 });
        }
        Ok(())
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
    /// Injects events with `SendInput`, and touch contacts with `InjectTouchInput` in order.
    ///
    /// # Arguments
    /// * `events` - events.
    ///
    /// # Errors
    /// [`PlatformError::Win32`] if `SendInput` inserts fewer events than requested (blocked by
    /// UIPI when a higher-integrity window has focus), or a touch frame is rejected.
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
                InputEvent::TouchDown { .. } | InputEvent::TouchMove { .. } | InputEvent::TouchUp { .. } => {
                    Self::send(&mut inputs)?;
                    if let Some(t) = self.touch.as_mut() {
                        t.apply(e)?;
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
        Self::send(&mut inputs)
    }

    /// Reports touch support.
    ///
    /// # Returns
    /// `true` when `InitializeTouchInjection` succeeded.
    fn supports_touch(&self) -> bool {
        self.touch.is_some()
    }

    /// Keeps held touch contacts alive between headset packets.
    ///
    /// # Errors
    /// [`PlatformError::Win32`] if the refresh frame is rejected.
    fn tick(&mut self) -> Result<()> {
        self.touch.as_mut().map_or(Ok(()), TouchContacts::refresh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Taps the screen at `VD_TOUCH_TEST=x,y` (physical pixels) with a short drag, holding the
    /// contact long enough for a refresh frame.  Injects real input, so it is skipped by default.
    #[test]
    #[ignore]
    fn inject_touch_tap() {
        let at = std::env::var("VD_TOUCH_TEST").expect("VD_TOUCH_TEST=x,y");
        let (x, y) = at.split_once(',').map(|(a, b)| (a.trim().parse().unwrap(), b.trim().parse().unwrap())).unwrap();
        let mut b = SendInputBackend::new(DesktopBounds { x: 0, y: 0, width: 1, height: 1 }, false);
        assert!(b.supports_touch(), "InitializeTouchInjection failed");
        b.emit(&[InputEvent::TouchDown { id: 0, x, y }]).unwrap();
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(30));
            b.tick().unwrap();
        }
        b.emit(&[InputEvent::TouchMove { id: 0, x: x + 1, y }]).unwrap();
        b.emit(&[InputEvent::TouchUp { id: 0 }]).unwrap();
    }
}
