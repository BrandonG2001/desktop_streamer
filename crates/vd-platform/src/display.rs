//! Monitor enumeration.  C#: `IMonitorDuplication.GetMonitors()` → `VirtualDesktop.Interfaces.MonitorInfo[]`.

use crate::error::{PlatformError, Result};

/// One physical or virtual monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    /// Stable identifier (output name on Linux, device name on Windows).
    pub id: String,
    /// Index used by capture back-ends (`ddagrab output_idx`, X11 screen order).
    pub index: u32,
    /// Left edge in virtual-desktop coordinates.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Refresh rate in Hz (`DisplayFrequency`).
    pub refresh_hz: u32,
    /// Primary monitor.
    pub primary: bool,
}

impl MonitorInfo {
    /// Whether the monitor is taller than wide (`MonitorInfo.IsPortrait`).
    ///
    /// # Returns
    /// `true` for portrait orientation.
    pub fn is_portrait(&self) -> bool {
        self.height > self.width
    }
}

/// Bounding box of all monitors.
///
/// # Arguments
/// * `monitors` - monitors.
///
/// # Returns
/// `(x, y, width, height)`; `(0, 0, 1920, 1080)` for an empty list.
pub fn virtual_desktop_bounds(monitors: &[MonitorInfo]) -> (i32, i32, i32, i32) {
    if monitors.is_empty() {
        return (0, 0, 1920, 1080);
    }
    let x0 = monitors.iter().map(|m| m.x).min().unwrap_or(0);
    let y0 = monitors.iter().map(|m| m.y).min().unwrap_or(0);
    let x1 = monitors.iter().map(|m| m.x + m.width as i32).max().unwrap_or(1920);
    let y1 = monitors.iter().map(|m| m.y + m.height as i32).max().unwrap_or(1080);
    (x0, y0, x1 - x0, y1 - y0)
}

/// Parses `xrandr --listmonitors` output.
///
/// # Arguments
/// * `text` - command output.
///
/// # Returns
/// Monitors in listed order.
pub fn parse_xrandr_listmonitors(text: &str) -> Vec<MonitorInfo> {
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        // " 0: +*DP-2 2560/597x1440/336+0+0  DP-2"
        let mut parts = line.split_whitespace();
        let (Some(idx), Some(flags_name), Some(geom)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let Ok(index) = idx.trim_end_matches(':').parse::<u32>() else { continue };
        let primary = flags_name.contains('*');
        let id = parts.next().unwrap_or(flags_name.trim_start_matches(['+', '*'])).to_owned();
        let parse = || -> Option<(u32, u32, i32, i32)> {
            let (wpart, rest) = geom.split_once('x')?;
            let w = wpart.split('/').next()?.parse().ok()?;
            let mut it = rest.split('+');
            let h = it.next()?.split('/').next()?.parse().ok()?;
            let x = it.next()?.parse().ok()?;
            let y = it.next()?.parse().ok()?;
            Some((w, h, x, y))
        };
        if let Some((width, height, x, y)) = parse() {
            out.push(MonitorInfo { id, index, x, y, width, height, refresh_hz: 60, primary });
        }
    }
    out
}

/// Lists monitors.
///
/// # Returns
/// The monitors (primary first on Windows; listed order on Linux).
///
/// # Errors
/// [`PlatformError::Command`] if `xrandr` is unavailable (pure Wayland sessions);
/// [`PlatformError::Unsupported`] on other platforms.
pub fn monitors() -> Result<Vec<MonitorInfo>> {
    #[cfg(target_os = "linux")]
    {
        let out = std::process::Command::new("xrandr").arg("--listmonitors").output().map_err(|e| PlatformError::Command(format!("xrandr: {e}")))?;
        if !out.status.success() {
            return Err(PlatformError::Command("xrandr --listmonitors failed (no X11/XWayland display?)".into()));
        }
        return Ok(parse_xrandr_listmonitors(&String::from_utf8_lossy(&out.stdout)));
    }
    #[cfg(windows)]
    {
        return Ok(win::enumerate());
    }
    #[allow(unreachable_code)]
    Err(PlatformError::Unsupported("monitor enumeration"))
}

#[cfg(windows)]
mod win {
    use super::MonitorInfo;
    use windows_sys::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
    use windows_sys::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};

    unsafe extern "system" fn callback(monitor: HMONITOR, _hdc: HDC, _rect: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `&mut Vec<MonitorInfo>` passed by `enumerate`.
        let list = unsafe { &mut *(data as *mut Vec<MonitorInfo>) };
        let mut info: MONITORINFOEXW = unsafe { std::mem::zeroed() };
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: `info` is a properly sized MONITORINFOEXW.
        if unsafe { GetMonitorInfoW(monitor, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) } == 0 {
            return TRUE;
        }
        let r = info.monitorInfo.rcMonitor;
        let name_len = info.szDevice.iter().position(|c| *c == 0).unwrap_or(info.szDevice.len());
        let id = String::from_utf16_lossy(&info.szDevice[..name_len]);
        let mut mode: DEVMODEW = unsafe { std::mem::zeroed() };
        mode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
        // SAFETY: `szDevice` is NUL-terminated and `mode` is initialised with its size.
        let refresh = if unsafe { EnumDisplaySettingsW(info.szDevice.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode) } != 0 { mode.dmDisplayFrequency } else { 60 };
        list.push(MonitorInfo {
            id,
            index: list.len() as u32,
            x: r.left,
            y: r.top,
            width: (r.right - r.left) as u32,
            height: (r.bottom - r.top) as u32,
            refresh_hz: refresh,
            primary: info.monitorInfo.dwFlags & 1 != 0,
        });
        TRUE
    }

    /// Enumerates monitors with `EnumDisplayMonitors`.
    ///
    /// # Returns
    /// The monitors in enumeration order.
    pub fn enumerate() -> Vec<MonitorInfo> {
        let mut list: Vec<MonitorInfo> = Vec::new();
        // SAFETY: the callback only touches `list`, which outlives the call.
        unsafe {
            EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(callback), &mut list as *mut _ as LPARAM);
        }
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xrandr() {
        let text = "Monitors: 2\n 0: +*DP-2 2560/597x1440/336+0+0  DP-2\n 1: +HDMI-1 1920/509x1080/286+2560+0  HDMI-1\n";
        let m = parse_xrandr_listmonitors(text);
        assert_eq!(m.len(), 2);
        assert!(m[0].primary);
        assert_eq!((m[1].x, m[1].width), (2560, 1920));
        assert_eq!(virtual_desktop_bounds(&m), (0, 0, 4480, 1440));
    }
}
