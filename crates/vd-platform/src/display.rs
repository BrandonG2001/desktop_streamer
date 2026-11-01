//! Monitor enumeration.  C#: `IMonitorDuplication.GetMonitors()` → `VirtualDesktop.Interfaces.MonitorInfo[]`.

use crate::error::{PlatformError, Result};

/// One physical or virtual monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    /// Stable identifier (output name on Linux, device name on Windows).
    pub id: String,
    /// Index used by capture back-ends: on Windows the DXGI output of the first adapter
    /// (`ddagrab output_idx`, matched by device name), on Linux the X11 monitor order.
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
    /// A virtual monitor of the Parsec Virtual Display Driver ([`crate::virtual_display`]).
    pub is_virtual: bool,
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
            out.push(MonitorInfo { id, index, x, y, width, height, refresh_hz: 60, primary, is_virtual: false });
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

/// Display modes a monitor supports.
///
/// # Arguments
/// * `id` - [`MonitorInfo::id`] (`\\.\DISPLAYn` on Windows).
///
/// # Returns
/// `(width, height, refresh_hz)` per mode, deduplicated, in driver order; empty off Windows.
pub fn modes(id: &str) -> Vec<(u32, u32, u32)> {
    #[cfg(windows)]
    {
        return win::modes(id);
    }
    #[allow(unreachable_code)]
    {
        let _ = id;
        Vec::new()
    }
}

/// Changes a monitor's resolution and refresh rate, and stores it for the display.
///
/// # Arguments
/// * `id` - [`MonitorInfo::id`].
/// * `width`, `height` - resolution.
/// * `refresh_hz` - refresh rate.
///
/// # Errors
/// [`PlatformError::Win32`] with the `DISP_CHANGE_*` code when Windows rejects the mode;
/// [`PlatformError::Unsupported`] off Windows.
pub fn set_mode(id: &str, width: u32, height: u32, refresh_hz: u32) -> Result<()> {
    #[cfg(windows)]
    {
        return win::set_mode(id, width, height, refresh_hz);
    }
    #[allow(unreachable_code)]
    {
        let _ = (id, width, height, refresh_hz);
        Err(PlatformError::Unsupported("display modes"))
    }
}

/// Makes the process per-monitor DPI aware (Windows; a no-op elsewhere).
///
/// Without this, Windows virtualises monitor rectangles to 96 DPI when display scaling is above
/// 100 %, while the headset's laser-pointer coordinates and the captured frames are in physical
/// pixels — the cursor would land at the wrong place.  Call once at start-up, before any monitor
/// enumeration or window creation.
///
/// # Returns
/// `true` if awareness was set (or is not applicable), `false` if Windows refused (already set,
/// e.g. by a manifest).
pub fn enable_dpi_awareness() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
        // SAFETY: plain Win32 call with a predefined context handle.
        return unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) } != 0;
    }
    #[allow(unreachable_code)]
    true
}

#[cfg(windows)]
mod win {
    use super::MonitorInfo;
    use crate::error::{PlatformError, Result};
    use windows_sys::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
    use windows_sys::Win32::Graphics::Gdi::{
        CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DISPLAY_DEVICEW, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS, EnumDisplayDevicesW,
        EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    };

    /// Name of the Parsec adapter in `DISPLAY_DEVICEW.DeviceString`.
    const PARSEC_ADAPTER: &str = "Parsec Virtual Display Adapter";

    /// Device names (`\\.\DISPLAYn`) driven by the Parsec Virtual Display Adapter.
    ///
    /// # Returns
    /// The names.
    fn parsec_displays() -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0;
        loop {
            // SAFETY: zeroed DISPLAY_DEVICEW with its size set, as the API requires.
            let mut dev: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
            dev.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
            // SAFETY: enumerates display adapters (no device name), writing into `dev`.
            if unsafe { EnumDisplayDevicesW(std::ptr::null(), i, &mut dev, 0) } == 0 {
                break;
            }
            i += 1;
            let text = |s: &[u16]| String::from_utf16_lossy(&s[..s.iter().position(|c| *c == 0).unwrap_or(s.len())]);
            if text(&dev.DeviceString) == PARSEC_ADAPTER {
                out.push(text(&dev.DeviceName));
            }
        }
        out
    }

    /// NUL-terminated UTF-16 copy of a device name.
    ///
    /// # Arguments
    /// * `id` - device name.
    ///
    /// # Returns
    /// The wide string.
    fn wide(id: &str) -> Vec<u16> {
        id.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Device names of the outputs of the first DXGI adapter, in `EnumOutputs` order.  FFmpeg's
    /// ddagrab captures `output_idx` of this adapter (its D3D11 device uses the default one).
    /// Indirect displays such as virtual monitors are listed under the GPU that renders them.
    ///
    /// # Returns
    /// The names, or an empty list when DXGI is unavailable.
    fn dxgi_outputs() -> Vec<String> {
        use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
        let mut names = Vec::new();
        // SAFETY: COM calls on interfaces owned by this function; names are NUL-terminated.
        unsafe {
            let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return names };
            let Ok(adapter) = factory.EnumAdapters1(0) else { return names };
            let mut i = 0;
            while let Ok(output) = adapter.EnumOutputs(i) {
                i += 1;
                if let Ok(desc) = output.GetDesc() {
                    let len = desc.DeviceName.iter().position(|c| *c == 0).unwrap_or(desc.DeviceName.len());
                    names.push(String::from_utf16_lossy(&desc.DeviceName[..len]));
                }
            }
        }
        names
    }

    /// Lists the modes of a display with `EnumDisplaySettingsW`.
    ///
    /// # Arguments
    /// * `id` - device name.
    ///
    /// # Returns
    /// The distinct `(width, height, refresh)` modes.
    pub fn modes(id: &str) -> Vec<(u32, u32, u32)> {
        let name = wide(id);
        let mut out = Vec::new();
        let mut i = 0;
        loop {
            // SAFETY: zeroed DEVMODEW with its size set, as the API requires.
            let mut mode: DEVMODEW = unsafe { std::mem::zeroed() };
            mode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
            // SAFETY: `name` is NUL-terminated and `mode` is initialised.
            if unsafe { EnumDisplaySettingsW(name.as_ptr(), i, &mut mode) } == 0 {
                break;
            }
            i += 1;
            let m = (mode.dmPelsWidth, mode.dmPelsHeight, mode.dmDisplayFrequency);
            if !out.contains(&m) {
                out.push(m);
            }
        }
        out
    }

    /// Applies a mode with `ChangeDisplaySettingsExW(CDS_UPDATEREGISTRY)`.
    ///
    /// # Arguments
    /// * `id` - device name.
    /// * `width`, `height`, `refresh_hz` - the mode.
    ///
    /// # Errors
    /// [`PlatformError::Win32`] with the `DISP_CHANGE_*` result.
    pub fn set_mode(id: &str, width: u32, height: u32, refresh_hz: u32) -> Result<()> {
        let name = wide(id);
        // SAFETY: zeroed DEVMODEW with its size and the fields being changed.
        let mut mode: DEVMODEW = unsafe { std::mem::zeroed() };
        mode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
        mode.dmPelsWidth = width;
        mode.dmPelsHeight = height;
        mode.dmDisplayFrequency = refresh_hz;
        mode.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
        // SAFETY: `name` is NUL-terminated and `mode` is initialised; no window or extra data.
        let r = unsafe { ChangeDisplaySettingsExW(name.as_ptr(), &mode, std::ptr::null_mut(), CDS_UPDATEREGISTRY, std::ptr::null()) };
        if r == DISP_CHANGE_SUCCESSFUL { Ok(()) } else { Err(PlatformError::Win32 { api: "ChangeDisplaySettingsExW", code: r as u32 }) }
    }

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
            is_virtual: false,
        });
        TRUE
    }

    /// Enumerates monitors with `EnumDisplayMonitors`, numbering them by their DXGI output
    /// (enumeration order when DXGI does not list a monitor).
    ///
    /// # Returns
    /// The monitors in enumeration order.
    pub fn enumerate() -> Vec<MonitorInfo> {
        let mut list: Vec<MonitorInfo> = Vec::new();
        // SAFETY: the callback only touches `list`, which outlives the call.
        unsafe {
            EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(callback), &mut list as *mut _ as LPARAM);
        }
        let outputs = dxgi_outputs();
        let virtual_displays = parsec_displays();
        for m in &mut list {
            if let Some(i) = outputs.iter().position(|o| o.eq_ignore_ascii_case(&m.id)) {
                m.index = i as u32;
            }
            m.is_virtual = virtual_displays.iter().any(|v| v.eq_ignore_ascii_case(&m.id));
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
