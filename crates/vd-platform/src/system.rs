//! Desktop-session helpers used by the settings window: log-in autostart
//! (`StreamerSettings.StartWithWindows`), workstation lock (`StreamerSettings.LockComputer`),
//! Secure Boot state (`DynamicSettings.SecureBoot`) and console handling for the GUI.

use crate::error::{PlatformError, Result};

/// Value name of the autostart entry (Windows `Run` key) / desktop-file name (Linux).
const AUTOSTART_NAME: &str = "VirtualDesktopStreamerRust";

/// `CREATE_NO_WINDOW` process-creation flag: console children of the GUI get no console window.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Registers or removes the streamer as a log-in item.
///
/// Windows writes `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`; Linux writes
/// `~/.config/autostart/VirtualDesktopStreamerRust.desktop` (XDG autostart).
///
/// # Arguments
/// * `enabled` - add (`true`) or remove (`false`) the entry.
/// * `args` - arguments appended to the current executable's path (e.g. `["--minimized"]`).
///
/// # Errors
/// [`PlatformError::Io`] if the executable path is unknown or the file cannot be written;
/// [`PlatformError::Command`] if `reg.exe` fails.
pub fn set_autostart(enabled: bool, args: &[&str]) -> Result<()> {
    let exe = std::env::current_exe()?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        let mut cmd = std::process::Command::new("reg");
        cmd.creation_flags(CREATE_NO_WINDOW);
        if enabled {
            let mut value = format!("\"{}\"", exe.display());
            for a in args {
                value.push(' ');
                value.push_str(a);
            }
            cmd.args(["add", key, "/v", AUTOSTART_NAME, "/t", "REG_SZ", "/d", &value, "/f"]);
        } else {
            cmd.args(["delete", key, "/v", AUTOSTART_NAME, "/f"]);
        }
        let out = cmd.output()?;
        // Deleting a value that does not exist is not an error for us.
        if !out.status.success() && enabled {
            return Err(PlatformError::Command(format!("reg add failed: {}", String::from_utf8_lossy(&out.stderr).trim())));
        }
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let dir = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
            .ok_or_else(|| PlatformError::Io(std::io::Error::other("no HOME")))?
            .join("autostart");
        let file = dir.join(format!("{AUTOSTART_NAME}.desktop"));
        if !enabled {
            return match std::fs::remove_file(&file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            };
        }
        let mut exec = format!("\"{}\"", exe.display());
        for a in args {
            exec.push(' ');
            exec.push_str(a);
        }
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&file, format!("[Desktop Entry]\nType=Application\nName=Virtual Desktop Streamer\nExec={exec}\nX-GNOME-Autostart-enabled=true\n"))?;
        return Ok(());
    }
    #[allow(unreachable_code)]
    {
        let _ = (enabled, args, exe);
        Err(PlatformError::Unsupported("autostart"))
    }
}

/// Locks the interactive session (C#: `User32.LockWorkStation()` on disconnect).
///
/// # Errors
/// [`PlatformError::Win32`] if `LockWorkStation` fails; [`PlatformError::Command`] if
/// `loginctl lock-session` fails (no logind session) on Linux.
pub fn lock_workstation() -> Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Shutdown::LockWorkStation;
        // SAFETY: no arguments; fails harmlessly outside an interactive desktop.
        if unsafe { LockWorkStation() } == 0 {
            return Err(PlatformError::Win32 { api: "LockWorkStation", code: std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32 });
        }
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let ok = std::process::Command::new("loginctl").arg("lock-session").status().map(|s| s.success()).unwrap_or(false);
        return if ok { Ok(()) } else { Err(PlatformError::Command("loginctl lock-session failed".into())) };
    }
    #[allow(unreachable_code)]
    Err(PlatformError::Unsupported("workstation lock"))
}

/// Reports whether UEFI Secure Boot is enabled.
///
/// Windows reads `HKLM\SYSTEM\CurrentControlSet\Control\SecureBoot\State\UEFISecureBootEnabled`;
/// Linux reads the `SecureBoot-8be4df61-…` EFI variable.
///
/// # Returns
/// `Some(enabled)`, or `None` on legacy-BIOS machines or when the state cannot be read.
pub fn secure_boot() -> Option<bool> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("reg")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["query", r"HKLM\SYSTEM\CurrentControlSet\Control\SecureBoot\State", "/v", "UEFISecureBootEnabled"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let value = text.lines().find(|l| l.contains("UEFISecureBootEnabled"))?.split_whitespace().last()?.to_owned();
        return Some(value != "0x0");
    }
    #[cfg(target_os = "linux")]
    {
        let dir = std::fs::read_dir("/sys/firmware/efi/efivars").ok()?;
        for e in dir.flatten() {
            if e.file_name().to_string_lossy().starts_with("SecureBoot-") {
                // 4 attribute bytes followed by the 1-byte value.
                let data = std::fs::read(e.path()).ok()?;
                return data.get(4).map(|v| *v == 1);
            }
        }
        return None;
    }
    #[allow(unreachable_code)]
    None
}

/// Name of the GPU that drives the desktop (C#: `DynamicSettings.AdapterName`, from the DXGI
/// adapter description).
///
/// Windows picks the first hardware DXGI adapter with the most dedicated video memory; Linux
/// reads the NVIDIA driver's model file or the first VGA/3D controller listed by `lspci`.
///
/// # Returns
/// The adapter description, or `None` when it cannot be determined.
pub fn gpu_name() -> Option<String> {
    #[cfg(windows)]
    {
        use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1};
        // SAFETY: COM calls on interfaces owned by this function; descriptions are NUL-terminated UTF-16.
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
            let mut best: Option<(usize, String)> = None;
            let mut i = 0;
            while let Ok(adapter) = factory.EnumAdapters1(i) {
                i += 1;
                let Ok(desc) = adapter.GetDesc1() else { continue };
                if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                    continue;
                }
                let len = desc.Description.iter().position(|c| *c == 0).unwrap_or(desc.Description.len());
                let name = String::from_utf16_lossy(&desc.Description[..len]).trim().to_owned();
                if best.as_ref().is_none_or(|(mem, _)| desc.DedicatedVideoMemory > *mem) {
                    best = Some((desc.DedicatedVideoMemory, name));
                }
            }
            return best.map(|(_, n)| n);
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(dir) = std::fs::read_dir("/proc/driver/nvidia/gpus") {
            for e in dir.flatten() {
                if let Ok(text) = std::fs::read_to_string(e.path().join("information"))
                    && let Some(model) = text.lines().find_map(|l| l.strip_prefix("Model:"))
                {
                    return Some(model.trim().to_owned());
                }
            }
        }
        let out = std::process::Command::new("lspci").output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        return text
            .lines()
            .find(|l| l.contains("VGA compatible controller") || l.contains("3D controller") || l.contains("Display controller"))
            .and_then(|l| l.splitn(3, ':').nth(2))
            .map(|s| s.trim().to_owned());
    }
    #[allow(unreachable_code)]
    None
}

/// Detaches from the console when this process owns it (launched from Explorer / a shortcut), so
/// the settings window is not accompanied by an empty console window.  When started from a
/// terminal the console is shared with the shell and kept, so log output stays visible.
///
/// The standard handles are cleared as well: `FreeConsole` closes them but leaves their values
/// in place, where they go stale or are reused for sockets and files.  Console logging would then
/// write into those, and processes started with inherited standard handles fail
/// (`CreateProcess`: os error 6 or 50).  With null handles, output is discarded and children
/// start without them.
///
/// # Returns
/// `true` if the console was released.
pub fn detach_own_console() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
        let mut pids = [0u32; 2];
        // SAFETY: the buffer holds 2 entries; the call reports how many processes share the console.
        let n = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) };
        if n != 1 {
            return false;
        }
        // SAFETY: plain Win32 calls; a null standard handle is valid and means "none".
        unsafe {
            if FreeConsole() == 0 {
                return false;
            }
            for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                SetStdHandle(id, std::ptr::null_mut());
            }
        }
        return true;
    }
    #[allow(unreachable_code)]
    false
}
