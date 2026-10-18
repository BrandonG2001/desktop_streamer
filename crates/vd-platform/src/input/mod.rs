//! Input injection.  C#: `IInputInjection` (served by `VirtualDesktop.Server.exe`) fed through the
//! `VirtualDesktop.InputInjection` shared memory ([`vd_protocol::headset_state::InputInjectionShared`]).
//!
//! The streamer copies the [`InputState`] of every received headset packet into shared memory;
//! the service compares it with the previous state and injects keyboard, mouse, touch and
//! gamepad events.  This module performs the same diffing ([`InputTranslator`]) and hands the
//! resulting device-independent [`InputEvent`]s to a platform [`InputBackend`].

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
    const ORDER: [MouseButton; 5] = [MouseButton::Left, MouseButton::Right, MouseButton::Middle, MouseButton::Back, MouseButton::Forward];
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

/// Converts successive [`InputState`]s into events.
#[derive(Debug, Default)]
pub struct InputTranslator {
    prev: Option<InputState>,
    wheel_rest: (f32, f32),
    /// Offset added to pointer coordinates (origin of the streamed monitor).
    pub origin: (i32, i32),
}

impl InputTranslator {
    /// Creates a translator.
    ///
    /// # Arguments
    /// * `origin` - virtual-desktop position of the streamed monitor.
    ///
    /// # Returns
    /// The translator.
    pub fn new(origin: (i32, i32)) -> Self {
        Self { prev: None, wheel_rest: (0.0, 0.0), origin }
    }

    /// Diffs `state` against the previous state.
    ///
    /// # Arguments
    /// * `state` - newest input state.
    /// * `gamepad_enabled` - `SharedUserSettings.EmulateGamepad`.
    ///
    /// # Returns
    /// The events to inject, in order.
    pub fn translate(&mut self, state: &InputState, gamepad_enabled: bool) -> Vec<InputEvent> {
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

        // Laser pointers (absolute).
        for (cur, old) in state.pointers.iter().zip(prev.pointers.iter()) {
            let moved = (cur.x, cur.y) != (old.x, old.y);
            let active = cur.buttons.iter().any(|b| *b != 0) || old.buttons.iter().any(|b| *b != 0);
            if moved && (cur.x != 0 || cur.y != 0) {
                ev.push(InputEvent::PointerAbs { x: self.origin.0 + i32::from(cur.x), y: self.origin.1 + i32::from(cur.y) });
            }
            if active {
                for (i, button) in MouseButton::ORDER.iter().take(3).enumerate() {
                    let (c, o) = (cur.buttons[i] != 0, old.buttons[i] != 0);
                    if c != o {
                        ev.push(InputEvent::Button { button: *button, down: c });
                    }
                }
            }
        }

        // Relative mouse.
        let m = &state.mouse;
        if m.dx != 0 || m.dy != 0 {
            ev.push(InputEvent::MouseMove { dx: i32::from(m.dx), dy: i32::from(m.dy) });
        }
        for (i, button) in MouseButton::ORDER.iter().enumerate() {
            let (c, o) = (m.buttons[i] != 0, prev.mouse.buttons[i] != 0);
            if c != o {
                ev.push(InputEvent::Button { button: *button, down: c });
            }
        }
        // Wheel values are deltas consumed by every injection (the service zeroes them).
        let vy = self.wheel_rest.1 + m.wheel_y * 120.0;
        let vx = self.wheel_rest.0 + m.wheel_x * 120.0;
        let (iy, ix) = (vy.trunc() as i32, vx.trunc() as i32);
        self.wheel_rest = (vx - ix as f32, vy - iy as f32);
        if iy != 0 || ix != 0 {
            ev.push(InputEvent::Wheel { vertical: iy, horizontal: ix });
        }

        if gamepad_enabled && state.gamepad != prev.gamepad {
            ev.push(InputEvent::Gamepad(state.gamepad));
        }
        self.prev = Some(*state);
        ev
    }

    /// Events releasing everything currently held (C#: `IInputInjection.ReleaseInput`).
    ///
    /// # Returns
    /// Release events; the internal state is cleared.
    pub fn release_all(&mut self) -> Vec<InputEvent> {
        let Some(prev) = self.prev.take() else { return Vec::new() };
        let mut ev: Vec<InputEvent> = prev.keyboard.pressed().map(|s| InputEvent::Key { scan: s.scan_code, down: false }).collect();
        for (i, b) in MouseButton::ORDER.iter().enumerate() {
            if prev.mouse.buttons[i] != 0 || prev.pointers.iter().any(|p| i < 5 && p.buttons[i] != 0) {
                ev.push(InputEvent::Button { button: *b, down: false });
            }
        }
        ev.push(InputEvent::Gamepad(GamepadState::default()));
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
        let mut t = InputTranslator::new((0, 0));
        let mut s = InputState::default();
        s.keyboard.set_slot(0, KeySlot { scan_code: 0x1E, flags: 1 });
        assert_eq!(t.translate(&s, false), vec![InputEvent::Key { scan: 0x1E, down: true }]);
        assert!(t.translate(&s, false).is_empty());
        let s2 = InputState::default();
        assert_eq!(t.translate(&s2, false), vec![InputEvent::Key { scan: 0x1E, down: false }]);
    }

    #[test]
    fn wheel_accumulates_fractions() {
        let mut t = InputTranslator::new((0, 0));
        let mut s = InputState::default();
        s.mouse.wheel_y = 0.5;
        assert_eq!(t.translate(&s, false), vec![InputEvent::Wheel { vertical: 60, horizontal: 0 }]);
    }
}
