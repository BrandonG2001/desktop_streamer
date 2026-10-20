//! Process priority.  C#: `IServiceManager.UseRealtimePriority(processID, value)` /
//! `DisableProcessThrottling` and `IServer.UseRealtimePriority` (performed by the SYSTEM service
//! on Windows because the streamer runs unelevated).

use crate::error::{PlatformError, Result};

/// Raises the priority of the current process while streaming.
///
/// # Arguments
/// * `high` - `true` to raise, `false` to restore normal priority.
///
/// # Errors
/// [`PlatformError::Io`] / [`PlatformError::Win32`] if the OS refuses (on Linux raising the nice
/// value requires `CAP_SYS_NICE` or an `rtprio`/`nice` limit in `limits.conf`).
pub fn set_high_priority(high: bool) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let nice = if high { -10 } else { 0 };
        // SAFETY: plain syscall on the current process.
        let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice) };
        if rc != 0 {
            return Err(PlatformError::Io(std::io::Error::last_os_error()));
        }
        return Ok(());
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, HIGH_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, SetPriorityClass};
        let class = if high { HIGH_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS };
        // SAFETY: pseudo-handle of the current process.
        if unsafe { SetPriorityClass(GetCurrentProcess(), class) } == 0 {
            return Err(PlatformError::Win32 { api: "SetPriorityClass", code: std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32 });
        }
        return Ok(());
    }
    #[allow(unreachable_code)]
    {
        let _ = high;
        Err(PlatformError::Unsupported("process priority"))
    }
}

/// Stable machine identifier used as the computer id for registration.
///
/// C#: `_0091_0002._0002` reads `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProductId`
/// and falls back to a random GUID.  Linux uses `/etc/machine-id` (or the D-Bus copy).
///
/// # Returns
/// The id, or `None` if no source is available (callers then generate and persist a GUID).
pub fn machine_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        for p in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
            if let Ok(s) = std::fs::read_to_string(p) {
                let s = s.trim();
                if !s.is_empty() {
                    return Some(s.to_owned());
                }
            }
        }
        return None;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("reg").creation_flags(crate::system::CREATE_NO_WINDOW).args(["query", r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion", "/v", "ProductId"]).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        return text.lines().find(|l| l.contains("ProductId")).and_then(|l| l.split_whitespace().last()).map(str::to_owned);
    }
    #[allow(unreachable_code)]
    None
}

/// Human readable computer name (C#: `Environment.MachineName`).
///
/// # Returns
/// The host name, or `"Linux PC"` / `"PC"` when unavailable.
pub fn computer_name() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/etc/hostname") {
            let s = s.trim();
            if !s.is_empty() {
                return s.to_owned();
            }
        }
        return std::env::var("HOSTNAME").unwrap_or_else(|_| "Linux PC".into());
    }
    #[allow(unreachable_code)]
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into())
}
