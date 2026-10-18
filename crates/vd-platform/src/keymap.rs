//! PC/AT scan code (set 1) → Linux `KEY_*` code translation.
//!
//! The headset sends keys as set-1 scan codes with `0xE0` in the high byte for extended keys
//! (C#: `MapVirtualKey(code, MAPVK_VSC_TO_VK_EX)` and the `(code >> 8) == 0xE0` test in the
//! injection service).  For the base set the Linux input codes are numerically identical to
//! the scan codes (`KEY_ESC = 1` … `KEY_F12 = 88`); extended and non-US keys need a table.

/// Translates a set-1 scan code to a Linux input event code.
///
/// # Arguments
/// * `scan` - scan code; `0xE0xx` for extended keys.
///
/// # Returns
/// The `KEY_*` code, or `None` for unknown codes.
pub fn scan_to_linux(scan: u16) -> Option<u16> {
    let (ext, code) = ((scan >> 8) as u8, (scan & 0xFF) as u8);
    if ext == 0 {
        return match code {
            0x01..=0x58 => Some(u16::from(code)),
            0x64..=0x6E => Some(183 + u16::from(code - 0x64)), // F13–F23
            0x76 => Some(194),                                  // F24
            0x70 => Some(93),                                   // KEY_KATAKANAHIRAGANA
            0x73 => Some(89),                                   // KEY_RO
            0x79 => Some(92),                                   // KEY_HENKAN
            0x7B => Some(94),                                   // KEY_MUHENKAN
            0x7D => Some(124),                                  // KEY_YEN
            _ => None,
        };
    }
    if ext != 0xE0 {
        return if scan == 0xE11D { Some(119) } else { None }; // Pause (E1 1D 45)
    }
    Some(match code {
        0x1C => 96,  // KEY_KPENTER
        0x1D => 97,  // KEY_RIGHTCTRL
        0x35 => 98,  // KEY_KPSLASH
        0x37 => 99,  // KEY_SYSRQ (Print Screen)
        0x38 => 100, // KEY_RIGHTALT
        0x46 => 119, // KEY_PAUSE (Ctrl+Break)
        0x47 => 102, // KEY_HOME
        0x48 => 103, // KEY_UP
        0x49 => 104, // KEY_PAGEUP
        0x4B => 105, // KEY_LEFT
        0x4D => 106, // KEY_RIGHT
        0x4F => 107, // KEY_END
        0x50 => 108, // KEY_DOWN
        0x51 => 109, // KEY_PAGEDOWN
        0x52 => 110, // KEY_INSERT
        0x53 => 111, // KEY_DELETE
        0x5B => 125, // KEY_LEFTMETA
        0x5C => 126, // KEY_RIGHTMETA
        0x5D => 127, // KEY_COMPOSE (Menu)
        0x5E => 116, // KEY_POWER
        0x5F => 142, // KEY_SLEEP
        0x63 => 143, // KEY_WAKEUP
        0x10 => 165, // KEY_PREVIOUSSONG
        0x19 => 163, // KEY_NEXTSONG
        0x20 => 113, // KEY_MUTE
        0x21 => 140, // KEY_CALC
        0x22 => 164, // KEY_PLAYPAUSE
        0x24 => 166, // KEY_STOPCD
        0x2E => 114, // KEY_VOLUMEDOWN
        0x30 => 115, // KEY_VOLUMEUP
        0x32 => 172, // KEY_HOMEPAGE
        0x65 => 217, // KEY_SEARCH
        0x66 => 156, // KEY_BOOKMARKS
        0x67 => 173, // KEY_REFRESH
        0x68 => 128, // KEY_STOP
        0x69 => 159, // KEY_FORWARD
        0x6A => 158, // KEY_BACK
        0x6B => 157, // KEY_COMPUTER
        0x6C => 155, // KEY_MAIL
        0x6D => 226, // KEY_MEDIA
        _ => return None,
    })
}

/// Splits a scan code into the `(scan, extended)` pair used by Win32 `SendInput`
/// (`KEYEVENTF_SCANCODE` + `KEYEVENTF_EXTENDEDKEY`).
///
/// # Arguments
/// * `scan` - set-1 scan code.
///
/// # Returns
/// `(low byte, is extended)`.
pub fn scan_to_win32(scan: u16) -> (u16, bool) {
    (scan & 0xFF, (scan >> 8) == 0xE0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_and_extended() {
        assert_eq!(scan_to_linux(0x1E), Some(30)); // A
        assert_eq!(scan_to_linux(0xE048), Some(103)); // Up
        assert_eq!(scan_to_linux(0xE05B), Some(125)); // Left Win
        assert_eq!(scan_to_linux(0xE0FF), None);
        assert_eq!(scan_to_win32(0xE01D), (0x1D, true));
    }
}
