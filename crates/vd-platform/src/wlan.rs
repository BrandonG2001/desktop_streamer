//! Wi-Fi latency tuning.  Port of `libVirtualDesktopNet.dll` (exports `OptimizeWLAN`,
//! `StartWLANOptimizerThread`, `StopWLANOptimizerThread`).
//!
//! Recovered behaviour (Ghidra decompilation of `OptimizeWLAN @ 0x180001270`):
//!
//! * open a WLAN handle (`WlanOpenHandle(2, …)`) once and keep it;
//! * for every interface in state `wlan_interface_state_connected`, set
//!   `wlan_intf_opcode_media_streaming_mode = enable` and
//!   `wlan_intf_opcode_background_scan_enabled = !enable`, reading each value back to verify;
//! * return 0 on success, 1 when no connected wireless interface exists, 2 when the handle
//!   cannot be opened, 3 when access is denied, 5 for other set errors, 6 when verification
//!   fails.
//!
//! The optimizer thread (`FUN_1800015c0`) calls `OptimizeWLAN(1)` every **11 seconds** until it is
//! stopped or a call returns a code greater than 1 (because Windows re-enables background scans
//! periodically).  On Linux the closest equivalent is disabling 802.11 power saving with `iw`,
//! which removes the periodic latency spikes caused by the radio dozing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::{debug, info};

/// Result codes of [`optimize_wlan`] (identical to the DLL's return values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum WlanStatus {
    /// Settings applied.
    Ok = 0,
    /// No connected wireless interface.
    NoWirelessInterface = 1,
    /// The WLAN service could not be opened.
    OpenFailed = 2,
    /// Access denied while changing a setting.
    AccessDenied = 3,
    /// Other error while changing a setting.
    SetFailed = 5,
    /// The value read back differs from the one written.
    VerifyFailed = 6,
}

/// Interval of the optimizer thread (`11000000000` ns in `FUN_1800015c0`).
pub const OPTIMIZER_INTERVAL: Duration = Duration::from_secs(11);

/// Applies (or reverts) the low-latency Wi-Fi settings once.
///
/// # Arguments
/// * `enable` - `true` to optimise for streaming, `false` to restore normal behaviour.
///
/// # Returns
/// The status code.
pub fn optimize_wlan(enable: bool) -> WlanStatus {
    imp::optimize(enable)
}

/// Background thread re-applying [`optimize_wlan`] every [`OPTIMIZER_INTERVAL`].
pub struct WlanOptimizer {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WlanOptimizer {
    /// Starts the thread.  C#: `StartWLANOptimizerThread()` (called when streaming starts).
    ///
    /// # Returns
    /// The running optimizer.
    pub fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("WLAN Optimizer".into())
            .spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    let status = optimize_wlan(true);
                    debug!(?status, "WLAN optimisation pass");
                    if status as u32 > 1 {
                        info!(?status, "WLAN optimizer stopped");
                        return;
                    }
                    let mut waited = Duration::ZERO;
                    while waited < OPTIMIZER_INTERVAL && !flag.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(250));
                        waited += Duration::from_millis(250);
                    }
                }
            })
            .ok();
        Self { stop, thread }
    }

    /// Stops the thread and restores the default settings.  C#: `StopWLANOptimizerThread()`.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = optimize_wlan(false);
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::sync::Mutex;

    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, HANDLE};
    use windows_sys::Win32::NetworkManagement::WiFi::{
        WLAN_INTERFACE_INFO_LIST, WLAN_OPCODE_VALUE_TYPE, WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle, WlanQueryInterface, WlanSetInterface,
        wlan_interface_state_connected, wlan_intf_opcode_background_scan_enabled, wlan_intf_opcode_media_streaming_mode,
    };
    use windows_sys::core::GUID;

    use super::WlanStatus;

    struct Handle(HANDLE);
    // SAFETY: WLAN client handles may be used from any thread.
    unsafe impl Send for Handle {}

    static CLIENT: Mutex<Option<Handle>> = Mutex::new(None);

    /// `FUN_180001100`: sets a boolean interface opcode if it differs, then verifies it.
    unsafe fn set_bool(h: HANDLE, guid: &GUID, opcode: i32, value: bool) -> Result<(), WlanStatus> {
        let query = |out: &mut u32| -> bool {
            let mut size = 0u32;
            let mut data: *mut c_void = std::ptr::null_mut();
            let mut ty: WLAN_OPCODE_VALUE_TYPE = 0;
            // SAFETY: valid handle/guid and out-pointers.
            let rc = unsafe { WlanQueryInterface(h, guid, opcode, std::ptr::null(), &mut size, &mut data, &mut ty) };
            if rc != 0 || size == 0 || data.is_null() {
                return false;
            }
            // SAFETY: the API returned at least 4 bytes holding a BOOL.
            *out = unsafe { *(data as *const u32) };
            unsafe { WlanFreeMemory(data) };
            true
        };
        let mut cur = 0u32;
        if !query(&mut cur) {
            return Err(WlanStatus::VerifyFailed);
        }
        if (cur != 0) == value {
            return Ok(());
        }
        let v: u32 = u32::from(value);
        // SAFETY: `v` lives for the call; size is 4.
        let rc = unsafe { WlanSetInterface(h, guid, opcode, 4, &v as *const u32 as *const c_void, std::ptr::null()) };
        if rc != 0 {
            return Err(if rc == ERROR_ACCESS_DENIED { WlanStatus::AccessDenied } else { WlanStatus::SetFailed });
        }
        let mut after = 0u32;
        if !query(&mut after) || after != v {
            return Err(WlanStatus::VerifyFailed);
        }
        Ok(())
    }

    /// Windows implementation of [`super::optimize_wlan`].
    pub fn optimize(enable: bool) -> WlanStatus {
        let mut guard = CLIENT.lock().expect("poisoned");
        if guard.is_none() {
            let mut negotiated = 0u32;
            let mut h: HANDLE = std::ptr::null_mut();
            // SAFETY: out-pointers are valid.
            let rc = unsafe { WlanOpenHandle(2, std::ptr::null(), &mut negotiated, &mut h) };
            if rc != 0 || h.is_null() {
                return WlanStatus::OpenFailed;
            }
            *guard = Some(Handle(h));
        }
        let h = guard.as_ref().expect("set above").0;
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        // SAFETY: valid handle and out-pointer.
        if unsafe { WlanEnumInterfaces(h, std::ptr::null(), &mut list) } != 0 || list.is_null() {
            return WlanStatus::NoWirelessInterface;
        }
        let mut status = WlanStatus::NoWirelessInterface;
        let mut found = false;
        // SAFETY: `list` points to a WLAN_INTERFACE_INFO_LIST with `dwNumberOfItems` entries.
        unsafe {
            let n = (*list).dwNumberOfItems as usize;
            let first = (*list).InterfaceInfo.as_ptr();
            for i in 0..n {
                let info = &*first.add(i);
                if info.isState != wlan_interface_state_connected {
                    continue;
                }
                found = true;
                status = WlanStatus::Ok;
                if let Err(e) = set_bool(h, &info.InterfaceGuid, wlan_intf_opcode_media_streaming_mode, enable) {
                    status = e;
                }
                if let Err(e) = set_bool(h, &info.InterfaceGuid, wlan_intf_opcode_background_scan_enabled, !enable) {
                    status = e;
                }
            }
            WlanFreeMemory(list as *const c_void);
        }
        if found { status } else { WlanStatus::NoWirelessInterface }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::WlanStatus;

    fn wireless_interfaces() -> Vec<String> {
        let Ok(dir) = std::fs::read_dir("/sys/class/net") else { return Vec::new() };
        dir.filter_map(|e| e.ok())
            .filter(|e| e.path().join("wireless").exists() || e.path().join("phy80211").exists())
            .filter(|e| std::fs::read_to_string(e.path().join("operstate")).is_ok_and(|s| s.trim() == "up"))
            .filter_map(|e| e.file_name().into_string().ok())
            .collect()
    }

    /// Linux implementation of [`super::optimize_wlan`]: toggles 802.11 power saving with `iw`.
    pub fn optimize(enable: bool) -> WlanStatus {
        let ifaces = wireless_interfaces();
        if ifaces.is_empty() {
            return WlanStatus::NoWirelessInterface;
        }
        let mut status = WlanStatus::Ok;
        for iface in ifaces {
            let ps = if enable { "off" } else { "on" };
            match std::process::Command::new("iw").args(["dev", &iface, "set", "power_save", ps]).output() {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr);
                    status = if err.contains("Operation not permitted") || err.contains("-1") { WlanStatus::AccessDenied } else { WlanStatus::SetFailed };
                }
                Err(_) => status = WlanStatus::OpenFailed,
            }
        }
        status
    }
}
