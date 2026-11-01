//! `/dev/uinput` backend (Linux).
//!
//! Three virtual devices replace the Windows injection service and the `vdvge.sys` virtual
//! gamepad driver:
//!
//! * **Virtual Desktop Keyboard/Mouse** — every key code 1–248, five mouse buttons, relative X/Y,
//!   and (high-resolution) wheels;
//! * **Virtual Desktop Pointer** — absolute X/Y spanning the virtual desktop, used for the
//!   controller laser pointers;
//! * **Virtual Desktop Gamepad** — reports as an Xbox 360 controller (`045e:028e`) so games and
//!   SDL map it without configuration.
//!
//! The user needs write access to `/dev/uinput` (e.g. `KERNEL=="uinput", GROUP="input",
//! MODE="0660"` udev rule and membership of the `input` group).

use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
use evdev::{AbsInfo, AbsoluteAxisType, AttributeSet, BusType, EventType, InputEvent as EvEvent, InputId, Key, RelativeAxisType, UinputAbsSetup};
use tracing::{debug, warn};
use vd_protocol::headset_state::GamepadState;
use vd_protocol::types::gamepad_buttons as gb;

use super::{DesktopBounds, InputBackend, InputEvent, MouseButton};
use crate::error::{PlatformError, Result};
use crate::keymap::scan_to_linux;

fn map_err(e: std::io::Error) -> PlatformError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        PlatformError::PermissionDenied(format!("/dev/uinput: {e}"))
    } else {
        PlatformError::Io(e)
    }
}

fn button_key(b: MouseButton) -> Key {
    match b {
        MouseButton::Left => Key::BTN_LEFT,
        MouseButton::Right => Key::BTN_RIGHT,
        MouseButton::Middle => Key::BTN_MIDDLE,
        MouseButton::Back => Key::BTN_SIDE,
        MouseButton::Forward => Key::BTN_EXTRA,
    }
}

/// uinput devices.
pub struct UinputBackend {
    kbd_mouse: VirtualDevice,
    pointer: VirtualDevice,
    gamepad: Option<VirtualDevice>,
    want_gamepad: bool,
    bounds: DesktopBounds,
    /// High-resolution wheel units `(vertical, horizontal)` not yet reported as whole notches.
    wheel_rest: (i32, i32),
}

impl UinputBackend {
    /// Creates the virtual devices.
    ///
    /// # Arguments
    /// * `bounds` - virtual desktop bounds (range of the absolute pointer).
    /// * `gamepad` - create the gamepad immediately (it is otherwise created on first use).
    ///
    /// # Returns
    /// The backend.
    ///
    /// # Errors
    /// [`PlatformError::PermissionDenied`] when `/dev/uinput` is not writable.
    pub fn new(bounds: DesktopBounds, gamepad: bool) -> Result<Self> {
        let mut keys = AttributeSet::<Key>::new();
        for code in 1..=248u16 {
            keys.insert(Key::new(code));
        }
        for b in [MouseButton::Left, MouseButton::Right, MouseButton::Middle, MouseButton::Back, MouseButton::Forward] {
            keys.insert(button_key(b));
        }
        let mut rel = AttributeSet::<RelativeAxisType>::new();
        for a in [RelativeAxisType::REL_X, RelativeAxisType::REL_Y, RelativeAxisType::REL_WHEEL, RelativeAxisType::REL_HWHEEL, RelativeAxisType::REL_WHEEL_HI_RES, RelativeAxisType::REL_HWHEEL_HI_RES] {
            rel.insert(a);
        }
        let kbd_mouse = VirtualDeviceBuilder::new()
            .map_err(map_err)?
            .name("Virtual Desktop Keyboard/Mouse")
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0x5644, 0x0001, 1))
            .with_keys(&keys)
            .map_err(map_err)?
            .with_relative_axes(&rel)
            .map_err(map_err)?
            .build()
            .map_err(map_err)?;

        let mut pkeys = AttributeSet::<Key>::new();
        for k in [Key::BTN_LEFT, Key::BTN_RIGHT, Key::BTN_MIDDLE] {
            pkeys.insert(k);
        }
        let ax = UinputAbsSetup::new(AbsoluteAxisType::ABS_X, AbsInfo::new(0, 0, bounds.width.max(1) - 1, 0, 0, 1));
        let ay = UinputAbsSetup::new(AbsoluteAxisType::ABS_Y, AbsInfo::new(0, 0, bounds.height.max(1) - 1, 0, 0, 1));
        let pointer = VirtualDeviceBuilder::new()
            .map_err(map_err)?
            .name("Virtual Desktop Pointer")
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0x5644, 0x0002, 1))
            .with_keys(&pkeys)
            .map_err(map_err)?
            .with_absolute_axis(&ax)
            .map_err(map_err)?
            .with_absolute_axis(&ay)
            .map_err(map_err)?
            .build()
            .map_err(map_err)?;

        let mut me = Self { kbd_mouse, pointer, gamepad: None, want_gamepad: gamepad, bounds, wheel_rest: (0, 0) };
        if gamepad {
            me.ensure_gamepad()?;
        }
        Ok(me)
    }

    fn ensure_gamepad(&mut self) -> Result<&mut VirtualDevice> {
        if self.gamepad.is_none() {
            let mut keys = AttributeSet::<Key>::new();
            for k in [Key::BTN_SOUTH, Key::BTN_EAST, Key::BTN_NORTH, Key::BTN_WEST, Key::BTN_TL, Key::BTN_TR, Key::BTN_SELECT, Key::BTN_START, Key::BTN_MODE, Key::BTN_THUMBL, Key::BTN_THUMBR] {
                keys.insert(k);
            }
            let stick = |a| UinputAbsSetup::new(a, AbsInfo::new(0, -32768, 32767, 16, 128, 0));
            let trig = |a| UinputAbsSetup::new(a, AbsInfo::new(0, 0, 255, 0, 0, 0));
            let hat = |a| UinputAbsSetup::new(a, AbsInfo::new(0, -1, 1, 0, 0, 0));
            let dev = VirtualDeviceBuilder::new()
                .map_err(map_err)?
                .name("Virtual Desktop Gamepad")
                .input_id(InputId::new(BusType::BUS_USB, 0x045e, 0x028e, 0x0110))
                .with_keys(&keys)
                .map_err(map_err)?
                .with_absolute_axis(&stick(AbsoluteAxisType::ABS_X))
                .map_err(map_err)?
                .with_absolute_axis(&stick(AbsoluteAxisType::ABS_Y))
                .map_err(map_err)?
                .with_absolute_axis(&stick(AbsoluteAxisType::ABS_RX))
                .map_err(map_err)?
                .with_absolute_axis(&stick(AbsoluteAxisType::ABS_RY))
                .map_err(map_err)?
                .with_absolute_axis(&trig(AbsoluteAxisType::ABS_Z))
                .map_err(map_err)?
                .with_absolute_axis(&trig(AbsoluteAxisType::ABS_RZ))
                .map_err(map_err)?
                .with_absolute_axis(&hat(AbsoluteAxisType::ABS_HAT0X))
                .map_err(map_err)?
                .with_absolute_axis(&hat(AbsoluteAxisType::ABS_HAT0Y))
                .map_err(map_err)?
                .build()
                .map_err(map_err)?;
            debug!("virtual gamepad created");
            self.gamepad = Some(dev);
        }
        Ok(self.gamepad.as_mut().expect("just created"))
    }

    fn gamepad_events(s: &GamepadState) -> Vec<EvEvent> {
        let k = |key: Key, mask: u16| EvEvent::new(EventType::KEY, key.code(), i32::from(s.buttons & mask != 0));
        let a = |axis: AbsoluteAxisType, v: i32| EvEvent::new(EventType::ABSOLUTE, axis.0, v);
        let hat_x = i32::from(s.buttons & gb::DPAD_RIGHT != 0) - i32::from(s.buttons & gb::DPAD_LEFT != 0);
        let hat_y = i32::from(s.buttons & gb::DPAD_DOWN != 0) - i32::from(s.buttons & gb::DPAD_UP != 0);
        vec![
            k(Key::BTN_SOUTH, gb::A),
            k(Key::BTN_EAST, gb::B),
            k(Key::BTN_NORTH, gb::X),
            k(Key::BTN_WEST, gb::Y),
            k(Key::BTN_TL, gb::LEFT_SHOULDER),
            k(Key::BTN_TR, gb::RIGHT_SHOULDER),
            k(Key::BTN_SELECT, gb::BACK),
            k(Key::BTN_START, gb::START),
            k(Key::BTN_MODE, gb::GUIDE),
            k(Key::BTN_THUMBL, gb::LEFT_THUMB),
            k(Key::BTN_THUMBR, gb::RIGHT_THUMB),
            a(AbsoluteAxisType::ABS_X, i32::from(s.thumb_lx)),
            // XInput Y is "up positive", evdev Y is "down positive".
            a(AbsoluteAxisType::ABS_Y, -i32::from(s.thumb_ly).clamp(-32767, 32767)),
            a(AbsoluteAxisType::ABS_RX, i32::from(s.thumb_rx)),
            a(AbsoluteAxisType::ABS_RY, -i32::from(s.thumb_ry).clamp(-32767, 32767)),
            a(AbsoluteAxisType::ABS_Z, i32::from(s.left_trigger)),
            a(AbsoluteAxisType::ABS_RZ, i32::from(s.right_trigger)),
            a(AbsoluteAxisType::ABS_HAT0X, hat_x),
            a(AbsoluteAxisType::ABS_HAT0Y, hat_y),
        ]
    }
}

impl InputBackend for UinputBackend {
    /// Injects events through the virtual devices (each device's batch ends with `SYN_REPORT`).
    ///
    /// # Arguments
    /// * `events` - events to inject.
    ///
    /// # Errors
    /// [`PlatformError::Io`] if writing to uinput fails.
    fn emit(&mut self, events: &[InputEvent]) -> Result<()> {
        let mut km = Vec::new();
        let mut ptr = Vec::new();
        for e in events {
            match e {
                InputEvent::Key { scan, down } => match scan_to_linux(*scan) {
                    Some(code) => km.push(EvEvent::new(EventType::KEY, code, i32::from(*down))),
                    None => debug!(scan = format!("{scan:#06x}"), "unmapped scan code"),
                },
                InputEvent::MouseMove { dx, dy } => {
                    km.push(EvEvent::new(EventType::RELATIVE, RelativeAxisType::REL_X.0, *dx));
                    km.push(EvEvent::new(EventType::RELATIVE, RelativeAxisType::REL_Y.0, *dy));
                }
                InputEvent::PointerAbs { x, y } => {
                    ptr.push(EvEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_X.0, (x - self.bounds.x).clamp(0, self.bounds.width - 1)));
                    ptr.push(EvEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_Y.0, (y - self.bounds.y).clamp(0, self.bounds.height - 1)));
                }
                InputEvent::Button { button, down } => km.push(EvEvent::new(EventType::KEY, button_key(*button).code(), i32::from(*down))),
                InputEvent::Wheel { vertical, horizontal } => {
                    // Legacy notch axes get whole notches only; the remainder carries over.
                    if *vertical != 0 {
                        km.push(EvEvent::new(EventType::RELATIVE, RelativeAxisType::REL_WHEEL_HI_RES.0, *vertical));
                        self.wheel_rest.0 += vertical;
                        if self.wheel_rest.0 / 120 != 0 {
                            km.push(EvEvent::new(EventType::RELATIVE, RelativeAxisType::REL_WHEEL.0, self.wheel_rest.0 / 120));
                            self.wheel_rest.0 %= 120;
                        }
                    }
                    if *horizontal != 0 {
                        km.push(EvEvent::new(EventType::RELATIVE, RelativeAxisType::REL_HWHEEL_HI_RES.0, *horizontal));
                        self.wheel_rest.1 += horizontal;
                        if self.wheel_rest.1 / 120 != 0 {
                            km.push(EvEvent::new(EventType::RELATIVE, RelativeAxisType::REL_HWHEEL.0, self.wheel_rest.1 / 120));
                            self.wheel_rest.1 %= 120;
                        }
                    }
                }
                // Never produced: this backend does not report touch support.
                InputEvent::TouchDown { .. } | InputEvent::TouchMove { .. } | InputEvent::TouchUp { .. } => {}
                InputEvent::Gamepad(state) => {
                    if self.want_gamepad || self.gamepad.is_some() || *state != GamepadState::default() {
                        match self.ensure_gamepad() {
                            Ok(dev) => dev.emit(&Self::gamepad_events(state))?,
                            Err(e) => warn!(error = %e, "cannot create virtual gamepad"),
                        }
                    }
                }
            }
        }
        if !ptr.is_empty() {
            self.pointer.emit(&ptr)?;
        }
        if !km.is_empty() {
            self.kbd_mouse.emit(&km)?;
        }
        Ok(())
    }
}
