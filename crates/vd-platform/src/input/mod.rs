//! Input injection.  C#: `IInputInjection` (served by `VirtualDesktop.Server.exe`) fed through the
//! `VirtualDesktop.InputInjection` shared memory ([`vd_protocol::headset_state::InputInjectionShared`]).
//!
//! The streamer copies the [`InputState`] of every received headset packet into shared memory;
//! the service compares it with the previous state and injects keyboard, mouse, touch and
//! gamepad events.  This module performs the same diffing ([`InputTranslator`]) and hands the
//! resulting device-independent [`InputEvent`]s to a platform [`InputBackend`].

use std::time::Duration;

use vd_protocol::headset_state::{GamepadState, InputState};

use crate::error::Result;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

/// Mouse buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// Primary.
    Left,
    /// Secondary.
    Right,
    /// Middle / wheel.
    Middle,
    /// Back (X1).
    Back,
    /// Forward (X2).
    Forward,
}

impl MouseButton {
    /// Buttons of [`vd_protocol::headset_state::MouseState::buttons`]`[1..6]`, in order
    /// (C#: `LEFT`, `MIDDLE`, `RIGHT`, `XBUTTON1`, `XBUTTON2` in `_0008._0002._0001(in _0002)`).
    const MOUSE_ORDER: [MouseButton; 5] = [MouseButton::Left, MouseButton::Middle, MouseButton::Right, MouseButton::Back, MouseButton::Forward];
}

/// Device-independent input event.
#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    /// Key press/release by set-1 scan code.
    Key {
        /// Scan code (`0xE0xx` for extended keys).
        scan: u16,
        /// Pressed.
        down: bool,
    },
    /// Relative pointer motion.
    MouseMove {
        /// X delta.
        dx: i32,
        /// Y delta.
        dy: i32,
    },
    /// Absolute pointer position in virtual-desktop pixels.
    PointerAbs {
        /// X.
        x: i32,
        /// Y.
        y: i32,
    },
    /// Button press/release.
    Button {
        /// Button.
        button: MouseButton,
        /// Pressed.
        down: bool,
    },
    /// Scroll in high-resolution units (120 = one notch).
    Wheel {
        /// Vertical (positive = up / away from the user).
        vertical: i32,
        /// Horizontal (positive = right).
        horizontal: i32,
    },
    /// New gamepad state.
    Gamepad(GamepadState),
}

/// Longest gap, in milliseconds, that [`InputTranslator::tick`] turns into motion at once (avoids
/// a cursor jump after a stall).
const MAX_TICK_MS: f64 = 50.0;

/// Converts successive [`InputState`]s into events, reproducing the 1 ms injection loop of
/// `VirtualDesktop.Server.exe` (`_0008._0002._0001()`):
///
/// * **Keyboard** — every slot holding a scan code is a held key.
/// * **Laser pointers** — while a pointer is in range the cursor follows it (absolute); its
///   contact presses left, right (`buttons[3]`) or middle (`buttons[4]`).  The original injects
///   real touch contacts; this port emulates them with the mouse, so when both controllers point
///   at the screen the one in contact owns the cursor.
/// * **Mouse mode** — only while [`MouseState::active`](vd_protocol::headset_state::MouseState::active):
///   buttons left/middle/right/X1/X2, motion as a velocity applied by [`InputTranslator::tick`]
///   (or an absolute position when `controllers.absolute_mouse` is set), and the wheel.  While a
///   pointer is in range the mouse is ignored except for the wheel, which scrolls under it.
/// * **Gamepad** — forwarded as is when emulation is enabled.
#[derive(Debug, Default)]
pub struct InputTranslator {
    /// Last state passed to [`InputTranslator::translate`].
    prev: Option<InputState>,
    /// Mouse buttons held by mouse mode.
    mouse_held: Vec<MouseButton>,
    /// Button held by each pointer's contact.
    contact: [Option<MouseButton>; 2],
    /// Milliseconds of mouse-mode motion not yet emitted.
    pending_ms: f64,
}

impl InputTranslator {
    /// Creates a translator with nothing held.
    ///
    /// # Returns
    /// The translator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Diffs `state` against the previous state.
    ///
    /// # Arguments
    /// * `state` - newest input state.
    /// * `desktop` - the desktop is being streamed (`StreamingSource.Desktop`).  Otherwise the
    ///   streamer only forwards the gamepad, so keys, pointers and mouse buttons are released.
    /// * `gamepad_enabled` - `SharedUserSettings.EmulateGamepad`.
    ///
    /// # Returns
    /// The events to inject, in order.
    pub fn translate(&mut self, state: &InputState, desktop: bool, gamepad_enabled: bool) -> Vec<InputEvent> {
        let state = if desktop { *state } else { InputState { gamepad: state.gamepad, gamepad_model: state.gamepad_model, ..InputState::default() } };
        let prev = self.prev.unwrap_or_default();
        let mut ev = Vec::new();

        // Keyboard: a slot holding a scan code means "held".
        let now: Vec<u16> = state.keyboard.pressed().map(|s| s.scan_code).collect();
        let before: Vec<u16> = prev.keyboard.pressed().map(|s| s.scan_code).collect();
        for k in before.iter().filter(|k| !now.contains(k)) {
            ev.push(InputEvent::Key { scan: *k, down: false });
        }
        for k in now.iter().filter(|k| !before.contains(k)) {
            ev.push(InputEvent::Key { scan: *k, down: true });
        }

        let pointer_active = state.pointers.iter().any(|p| p.in_range());
        self.pointers(&state, &prev, &mut ev);

        // Wheel deltas are per packet (the server zeroes them after injecting `value * 8`).
        let m = &state.mouse;
        let has_wheel = m.wheel_x != 0 || m.wheel_y != 0;
        let wheel = InputEvent::Wheel { vertical: i32::from(m.wheel_y) * 8, horizontal: i32::from(m.wheel_x) * 8 };
        if pointer_active {
            // C#: InjectScrollInput(wheel_x, wheel_y) — touch scrolling under the laser.
            if has_wheel {
                ev.push(wheel);
            }
        } else {
            // Releases are delivered even when mouse mode has just been turned off.
            for (i, button) in MouseButton::MOUSE_ORDER.iter().enumerate() {
                let down = m.active() && m.buttons[i + 1] != 0;
                if !down && self.mouse_held.contains(button) {
                    self.mouse_held.retain(|b| b != button);
                    ev.push(InputEvent::Button { button: *button, down: false });
                }
            }
            if m.active() {
                if state.controllers.absolute_mouse != 0 && (m.x, m.y) != (prev.mouse.x, prev.mouse.y) {
                    ev.push(InputEvent::PointerAbs { x: m.x.round() as i32, y: m.y.round() as i32 });
                }
                for (i, button) in MouseButton::MOUSE_ORDER.iter().enumerate() {
                    if m.buttons[i + 1] != 0 && !self.mouse_held.contains(button) {
                        self.mouse_held.push(*button);
                        ev.push(InputEvent::Button { button: *button, down: true });
                    }
                }
                if has_wheel {
                    ev.push(wheel);
                }
            }
        }

        if gamepad_enabled && state.gamepad != prev.gamepad {
            ev.push(InputEvent::Gamepad(state.gamepad));
        }
        self.prev = Some(state);
        ev
    }

    /// Emits laser-pointer moves and contact presses/releases.
    ///
    /// # Arguments
    /// * `state` - newest state.
    /// * `prev` - previous state.
    /// * `ev` - output events (appended to).
    fn pointers(&mut self, state: &InputState, prev: &InputState, ev: &mut Vec<InputEvent>) {
        // Contacts that end are released first, so a press by the other pointer lands cleanly.
        for (i, cur) in state.pointers.iter().enumerate() {
            if !(cur.in_range() && cur.contact()) {
                if let Some(button) = self.contact[i].take() {
                    ev.push(InputEvent::Button { button, down: false });
                }
            }
        }
        // With a single cursor only one pointer can drive it: a pointer in contact wins.
        let owner = (0..2).find(|&i| self.contact[i].is_some()).or_else(|| (0..2).find(|&i| state.pointers[i].in_range() && state.pointers[i].contact()));
        for (i, (cur, old)) in state.pointers.iter().zip(prev.pointers.iter()).enumerate() {
            if !cur.in_range() || owner.is_some_and(|o| o != i) {
                continue;
            }
            if !old.in_range() || (cur.x, cur.y) != (old.x, old.y) {
                ev.push(InputEvent::PointerAbs { x: i32::from(cur.x), y: i32::from(cur.y) });
            }
            if cur.contact() && self.contact[i].is_none() {
                let button = if cur.buttons[3] != 0 {
                    MouseButton::Right
                } else if cur.buttons[4] != 0 {
                    MouseButton::Middle
                } else {
                    MouseButton::Left
                };
                self.contact[i] = Some(button);
                ev.push(InputEvent::Button { button, down: true });
            }
        }
    }

    /// Applies mouse-mode motion for the time elapsed since the previous tick.
    ///
    /// The server moves the cursor by `round(x), round(y)` pixels every millisecond for as long
    /// as the last packet's values sit in shared memory, so this should be called every few
    /// milliseconds between packets.
    ///
    /// # Arguments
    /// * `elapsed` - time since the previous call.
    ///
    /// # Returns
    /// An [`InputEvent::MouseMove`] when the cursor should move, else `None`.
    pub fn tick(&mut self, elapsed: Duration) -> Option<InputEvent> {
        let s = self.prev.as_ref()?;
        let m = &s.mouse;
        let moving = m.active() && s.controllers.absolute_mouse == 0 && !s.pointers.iter().any(|p| p.in_range());
        let (vx, vy) = (m.x.round() as i32, m.y.round() as i32);
        if !moving || (vx == 0 && vy == 0) {
            self.pending_ms = 0.0;
            return None;
        }
        self.pending_ms = (self.pending_ms + elapsed.as_secs_f64() * 1000.0).min(MAX_TICK_MS);
        let ms = self.pending_ms.floor();
        self.pending_ms -= ms;
        let ms = ms as i32;
        (ms > 0).then_some(InputEvent::MouseMove { dx: vx * ms, dy: vy * ms })
    }

    /// Events releasing everything currently held (C#: `IInputInjection.ReleaseInput`).
    ///
    /// # Returns
    /// Release events; the internal state is cleared.
    pub fn release_all(&mut self) -> Vec<InputEvent> {
        let Some(prev) = self.prev.take() else { return Vec::new() };
        let mut ev: Vec<InputEvent> = prev.keyboard.pressed().map(|s| InputEvent::Key { scan: s.scan_code, down: false }).collect();
        for button in self.mouse_held.drain(..).chain(self.contact.iter_mut().filter_map(Option::take)) {
            ev.push(InputEvent::Button { button, down: false });
        }
        ev.push(InputEvent::Gamepad(GamepadState::default()));
        self.pending_ms = 0.0;
        ev
    }
}

/// A platform input sink.
pub trait InputBackend: Send {
    /// Injects events.
    ///
    /// # Arguments
    /// * `events` - events in order.
    ///
    /// # Errors
    /// OS errors from the injection API.
    fn emit(&mut self, events: &[InputEvent]) -> Result<()>;
}

/// Backend that discards events (used when injection is unavailable).
#[derive(Debug, Default)]
pub struct NullBackend;

impl InputBackend for NullBackend {
    /// Discards `events`.
    ///
    /// # Arguments
    /// * `_events` - ignored.
    ///
    /// # Errors
    /// Never.
    fn emit(&mut self, _events: &[InputEvent]) -> Result<()> {
        Ok(())
    }
}

/// Size of the virtual desktop used to scale absolute pointer events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopBounds {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub width: i32,
    /// Height.
    pub height: i32,
}

/// Opens the native input backend.
///
/// # Arguments
/// * `bounds` - virtual desktop bounds (absolute pointer range).
/// * `gamepad` - create a virtual gamepad as well.
///
/// # Returns
/// The backend.
///
/// # Errors
/// [`crate::PlatformError::PermissionDenied`] if `/dev/uinput` cannot be opened (Linux), or
/// [`crate::PlatformError::Unsupported`] on other platforms.
pub fn open_backend(bounds: DesktopBounds, gamepad: bool) -> Result<Box<dyn InputBackend>> {
    #[cfg(target_os = "linux")]
    {
        return Ok(Box::new(linux::UinputBackend::new(bounds, gamepad)?));
    }
    #[cfg(windows)]
    {
        return Ok(Box::new(windows::SendInputBackend::new(bounds, gamepad)));
    }
    #[allow(unreachable_code)]
    {
        let _ = (bounds, gamepad);
        Err(crate::PlatformError::Unsupported("input injection"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vd_protocol::headset_state::KeySlot;

    #[test]
    fn key_press_and_release() {
        let mut t = InputTranslator::new();
        let mut s = InputState::default();
        s.keyboard.set_slot(0, KeySlot { scan_code: 0x1E, flags: 1 });
        assert_eq!(t.translate(&s, true, false), vec![InputEvent::Key { scan: 0x1E, down: true }]);
        assert!(t.translate(&s, true, false).is_empty());
        let s2 = InputState::default();
        assert_eq!(t.translate(&s2, true, false), vec![InputEvent::Key { scan: 0x1E, down: false }]);
    }

    #[test]
    fn mouse_buttons_follow_server_order_and_need_active() {
        let mut t = InputTranslator::new();
        let mut s = InputState::default();
        s.mouse.buttons = [0, 1, 0, 0, 0, 0];
        assert!(t.translate(&s, true, false).is_empty(), "inactive mouse mode ignores presses");
        s.mouse.buttons = [1, 0, 1, 1, 0, 0];
        assert_eq!(
            t.translate(&s, true, false),
            vec![InputEvent::Button { button: MouseButton::Middle, down: true }, InputEvent::Button { button: MouseButton::Right, down: true }]
        );
        // Turning mouse mode off releases what it held.
        s.mouse.buttons = [0, 0, 1, 1, 0, 0];
        assert_eq!(
            t.translate(&s, true, false),
            vec![InputEvent::Button { button: MouseButton::Middle, down: false }, InputEvent::Button { button: MouseButton::Right, down: false }]
        );
    }

    #[test]
    fn mouse_motion_is_a_velocity() {
        let mut t = InputTranslator::new();
        let mut s = InputState::default();
        s.mouse.buttons[0] = 1;
        s.mouse.x = 2.4;
        s.mouse.y = -1.0;
        assert!(t.translate(&s, true, false).is_empty());
        assert_eq!(t.tick(Duration::from_millis(5)), Some(InputEvent::MouseMove { dx: 10, dy: -5 }));
        s.controllers.absolute_mouse = 1;
        s.mouse.x = 100.0;
        assert_eq!(t.translate(&s, true, false), vec![InputEvent::PointerAbs { x: 100, y: -1 }]);
        assert_eq!(t.tick(Duration::from_millis(5)), None);
    }

    #[test]
    fn wheel_is_scaled_by_eight() {
        let mut t = InputTranslator::new();
        let mut s = InputState::default();
        s.mouse.buttons[0] = 1;
        s.mouse.wheel_y = 15;
        s.mouse.wheel_x = -1;
        assert_eq!(t.translate(&s, true, false), vec![InputEvent::Wheel { vertical: 120, horizontal: -8 }]);
    }

    #[test]
    fn pointer_hover_and_contact() {
        let mut t = InputTranslator::new();
        let mut s = InputState::default();
        s.pointers[1].buttons = [1, 0, 0, 0, 0];
        s.pointers[1].x = 1920;
        s.pointers[1].y = 40;
        assert_eq!(t.translate(&s, true, false), vec![InputEvent::PointerAbs { x: 1920, y: 40 }]);
        s.pointers[1].buttons = [1, 0, 1, 1, 0];
        assert_eq!(t.translate(&s, true, false), vec![InputEvent::Button { button: MouseButton::Right, down: true }]);
        s.pointers[1].buttons = [1, 0, 0, 0, 0];
        assert_eq!(t.translate(&s, true, false), vec![InputEvent::Button { button: MouseButton::Right, down: false }]);
    }

    #[test]
    fn contact_owns_the_cursor() {
        let mut t = InputTranslator::new();
        let mut s = InputState::default();
        s.pointers[0].buttons = [1, 0, 1, 0, 0];
        s.pointers[0].x = 10;
        s.pointers[1].buttons = [1, 0, 0, 0, 0];
        s.pointers[1].x = 500;
        assert_eq!(
            t.translate(&s, true, false),
            vec![InputEvent::PointerAbs { x: 10, y: 0 }, InputEvent::Button { button: MouseButton::Left, down: true }]
        );
    }

    #[test]
    fn non_desktop_only_forwards_gamepad() {
        let mut t = InputTranslator::new();
        let mut s = InputState::default();
        s.pointers[0].buttons = [1, 0, 1, 0, 0];
        s.gamepad.buttons = 0x1000;
        assert_eq!(t.translate(&s, false, true), vec![InputEvent::Gamepad(s.gamepad)]);
    }
}
