//! Virtual monitors.  C#: `IVirtualMonitor` (`Add`, `Remove`, `AddIfNecessary`) of the
//! Virtual Desktop service, backed by its IddCx driver `vdvmd.dll`.
//!
//! The original driver is not redistributable, so the port drives the free Parsec Virtual
//! Display Driver instead (`winget install Parsec.ParsecVDD`).  Its control interface, as
//! documented by github.com/nomi-san/parsec-vdd (`core/parsec-vdd.h`):
//!
//! * the adapter exposes the device interface `{00b41627-04c4-429e-a26e-0265cf50c8fa}`, opened
//!   for overlapped I/O;
//! * `DeviceIoControl` with a 32-byte input buffer and a 4-byte output:
//!   * `0x22E004` adds a display and returns its index;
//!   * `0x22A008` removes the display whose index is the first two input bytes, big-endian;
//!   * `0x22A00C` is the keep-alive.  Displays disappear when it is not sent for about a second,
//!     so they also go away when the streamer crashes;
//!   * `0x22E010` returns the driver version (major in the high 16 bits).
//!
//! Linux has no equivalent here; [`VirtualDisplays::open`] reports
//! [`PlatformError::Unsupported`].

use crate::error::{PlatformError, Result};

/// Most displays the port adds (the driver allows 16 per adapter; Parsec's own tools use 8).
pub const MAX_DISPLAYS: usize = 8;

/// Displays added through the Parsec Virtual Display Driver.  Dropping it removes them.
pub struct VirtualDisplays {
    #[cfg(windows)]
    inner: win::Controller,
}

/// Whether the Parsec Virtual Display Driver is installed and running.
///
/// # Returns
/// `true` if its adapter can be opened (always `false` off Windows).
pub fn is_available() -> bool {
    #[cfg(windows)]
    {
        return win::is_available();
    }
    #[allow(unreachable_code)]
    false
}

impl VirtualDisplays {
    /// Opens the driver and starts the keep-alive.
    ///
    /// # Returns
    /// The controller, with no displays added yet.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when the driver is not installed or not running, or off
    /// Windows; [`PlatformError::Win32`] when the device cannot be opened.
    pub fn open() -> Result<Self> {
        #[cfg(windows)]
        {
            return Ok(Self { inner: win::Controller::open()? });
        }
        #[allow(unreachable_code)]
        Err(PlatformError::Unsupported("virtual monitors"))
    }

    /// Driver version.
    ///
    /// # Returns
    /// `major.minor`, e.g. `0.45`.
    pub fn version(&self) -> String {
        #[cfg(windows)]
        {
            return self.inner.version();
        }
        #[allow(unreachable_code)]
        String::new()
    }

    /// Number of displays added by this controller.
    ///
    /// # Returns
    /// The count.
    pub fn count(&self) -> usize {
        #[cfg(windows)]
        {
            return self.inner.count();
        }
        #[allow(unreachable_code)]
        0
    }

    /// Adds a display (C#: `IVirtualMonitor.Add`).  Windows needs a moment to attach it; the
    /// caller enumerates the monitors again afterwards.
    ///
    /// # Returns
    /// The driver's index of the new display.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when [`MAX_DISPLAYS`] are already added;
    /// [`PlatformError::Win32`] when the driver rejects the request.
    pub fn add(&mut self) -> Result<u32> {
        #[cfg(windows)]
        {
            return self.inner.add();
        }
        #[allow(unreachable_code)]
        Err(PlatformError::Unsupported("virtual monitors"))
    }

    /// Removes the most recently added display (C#: `IVirtualMonitor.Remove`).
    ///
    /// # Returns
    /// `true` if a display was removed, `false` if none was added.
    ///
    /// # Errors
    /// [`PlatformError::Win32`] when the driver rejects the request.
    pub fn remove(&mut self) -> Result<bool> {
        #[cfg(windows)]
        {
            return self.inner.remove();
        }
        #[allow(unreachable_code)]
        Ok(false)
    }
}

#[cfg(windows)]
mod win {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use tracing::{debug, warn};
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
        SetupDiGetDeviceInterfaceDetailW,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_NO_BUFFERING, FILE_FLAG_OVERLAPPED, FILE_FLAG_WRITE_THROUGH, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, DeviceIoControl, GetOverlappedResultEx, OVERLAPPED};
    use windows_sys::Win32::System::Threading::CreateEventW;
    use windows_sys::core::GUID;

    use super::MAX_DISPLAYS;
    use crate::error::{PlatformError, Result};

    /// Device interface of the Parsec Virtual Display Adapter.
    const ADAPTER_GUID: GUID = GUID::from_u128(0x00b41627_04c4_429e_a26e_0265cf50c8fa);
    /// Adds a display; returns its index.
    const IOCTL_ADD: u32 = 0x0022_E004;
    /// Removes the display given by a big-endian 16-bit index.
    const IOCTL_REMOVE: u32 = 0x0022_A008;
    /// Keep-alive.
    const IOCTL_UPDATE: u32 = 0x0022_A00C;
    /// Returns the driver version.
    const IOCTL_VERSION: u32 = 0x0022_E010;
    /// Keep-alive period (the reference controller pings every 100 ms).
    const PING_PERIOD: Duration = Duration::from_millis(100);
    /// How long removing a display may take.  The driver completes the request once Windows has
    /// finished reconfiguring the desktop: about 3 s right after a mode change.
    const REMOVE_TIMEOUT_MS: u32 = 5000;
    /// How long a timed-out request is given to complete its cancellation.
    const CANCEL_WAIT_MS: u32 = 1000;

    /// Buffers of one request; they must stay valid until the driver completes it.
    struct Request {
        /// Input data.
        input: [u8; 32],
        /// 32-bit result.
        output: u32,
        /// Completion state and event.
        overlapped: OVERLAPPED,
    }

    /// Open handle of the adapter.
    struct Device(HANDLE);

    // SAFETY: a file handle may be used from any thread; every request has its own OVERLAPPED.
    unsafe impl Send for Device {}
    // SAFETY: as above, concurrent DeviceIoControl calls on one handle are allowed.
    unsafe impl Sync for Device {}

    impl Drop for Device {
        /// Closes the handle.
        fn drop(&mut self) {
            // SAFETY: the handle was opened by `open_device` and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    impl Device {
        /// Sends one control request and waits for it (C#: `Core.IoControl`).
        ///
        /// # Arguments
        /// * `code` - control code.
        /// * `input` - request data (at most 32 bytes are sent).
        /// * `timeout_ms` - how long to wait for the driver.
        ///
        /// # Returns
        /// The 32-bit result.
        ///
        /// # Errors
        /// [`PlatformError::Win32`] when the request fails or times out.
        fn control(&self, code: u32, input: &[u8], timeout_ms: u32) -> Result<u32> {
            // SAFETY: an all-zero Request (buffers and OVERLAPPED) is a valid initial state.
            let mut req: Box<Request> = Box::new(unsafe { std::mem::zeroed() });
            let n = input.len().min(req.input.len());
            req.input[..n].copy_from_slice(&input[..n]);
            // SAFETY: plain event creation (manual reset, initially clear, unnamed).
            req.overlapped.hEvent = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            if req.overlapped.hEvent.is_null() {
                // SAFETY: reads the calling thread's last error.
                return Err(PlatformError::Win32 { api: "CreateEventW", code: unsafe { GetLastError() } });
            }
            let mut transferred = 0u32;
            let r = &mut *req;
            // SAFETY: the request's buffers live in `req`, which is freed only once the request
            // has completed; a request the driver never completes leaks `req` instead.
            unsafe {
                // The reference code ignores the return value and always polls the OVERLAPPED.
                DeviceIoControl(self.0, code, r.input.as_ptr().cast(), r.input.len() as u32, (&raw mut r.output).cast(), 4, std::ptr::null_mut(), &mut r.overlapped);
                if GetOverlappedResultEx(self.0, &r.overlapped, &mut transferred, timeout_ms, 0) != 0 {
                    let output = r.output;
                    CloseHandle(r.overlapped.hEvent);
                    return Ok(output);
                }
                let error = GetLastError();
                CancelIoEx(self.0, &r.overlapped);
                if GetOverlappedResultEx(self.0, &r.overlapped, &mut transferred, CANCEL_WAIT_MS, 0) != 0 || GetLastError() != WAIT_TIMEOUT {
                    CloseHandle(r.overlapped.hEvent);
                } else {
                    // Still pending: the driver may write into the buffers later.
                    warn!(code, "the virtual display driver did not complete a request; leaking its buffers");
                    std::mem::forget(req);
                }
                Err(PlatformError::Win32 { api: "DeviceIoControl", code: error })
            }
        }
    }

    /// Opens the first present adapter interface (C#: `Device.OpenHandle`).
    ///
    /// # Returns
    /// The device.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when the driver is not installed or not started.
    fn open_device() -> Result<Device> {
        // SAFETY: SetupAPI enumeration of present interfaces of one class; the list is destroyed
        // before returning, and the detail buffer is sized and aligned for the returned path.
        unsafe {
            let list = SetupDiGetClassDevsW(&ADAPTER_GUID, std::ptr::null(), std::ptr::null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
            if list == INVALID_HANDLE_VALUE as isize {
                return Err(PlatformError::Win32 { api: "SetupDiGetClassDevsW", code: GetLastError() });
            }
            let mut found = None;
            let mut iface: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            iface.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            let mut i = 0;
            while found.is_none() && SetupDiEnumDeviceInterfaces(list, std::ptr::null(), &ADAPTER_GUID, i, &mut iface) != 0 {
                i += 1;
                let mut size = 0u32;
                SetupDiGetDeviceInterfaceDetailW(list, &iface, std::ptr::null_mut(), 0, &mut size, std::ptr::null_mut());
                if size == 0 {
                    continue;
                }
                let mut storage = vec![0u64; (size as usize).div_ceil(8)];
                let detail = storage.as_mut_ptr().cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
                (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
                if SetupDiGetDeviceInterfaceDetailW(list, &iface, detail, size, &mut size, std::ptr::null_mut()) == 0 {
                    continue;
                }
                let handle = CreateFileW(
                    (&raw const (*detail).DevicePath).cast(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_NO_BUFFERING | FILE_FLAG_OVERLAPPED | FILE_FLAG_WRITE_THROUGH,
                    std::ptr::null_mut(),
                );
                if handle != INVALID_HANDLE_VALUE && !handle.is_null() {
                    found = Some(Device(handle));
                }
            }
            SetupDiDestroyDeviceInfoList(list);
            found.ok_or(PlatformError::Unsupported("virtual monitors: the Parsec Virtual Display Driver is not installed or not running"))
        }
    }

    /// Whether the adapter can be opened.
    ///
    /// # Returns
    /// `true` when the driver is installed and started.
    pub(super) fn is_available() -> bool {
        open_device().is_ok()
    }

    /// Driver connection, keep-alive thread and the indices of the added displays.
    pub(super) struct Controller {
        /// Adapter handle, shared with the keep-alive thread.
        device: Arc<Device>,
        /// Driver indices of the added displays, in order of addition.
        added: Vec<u32>,
        /// Tells the keep-alive thread to stop.
        stop: Arc<AtomicBool>,
        /// Keep-alive thread.
        pinger: Option<JoinHandle<()>>,
    }

    impl Controller {
        /// Opens the driver and starts the keep-alive thread.
        ///
        /// # Returns
        /// The controller.
        ///
        /// # Errors
        /// See [`super::VirtualDisplays::open`].
        pub(super) fn open() -> Result<Self> {
            let device = Arc::new(open_device()?);
            let stop = Arc::new(AtomicBool::new(false));
            let pinger = {
                let (device, stop) = (Arc::clone(&device), Arc::clone(&stop));
                std::thread::Builder::new()
                    .name("virtual display keep-alive".into())
                    .spawn(move || {
                        while !stop.load(Ordering::Relaxed) {
                            if let Err(e) = device.control(IOCTL_UPDATE, &[], 1000) {
                                debug!(error = %e, "virtual display keep-alive failed");
                            }
                            std::thread::sleep(PING_PERIOD);
                        }
                    })
                    .map_err(PlatformError::Io)?
            };
            Ok(Self { device, added: Vec::new(), stop, pinger: Some(pinger) })
        }

        /// Driver version.
        ///
        /// # Returns
        /// `major.minor`, or an empty string when the driver does not answer.
        pub(super) fn version(&self) -> String {
            self.device.control(IOCTL_VERSION, &[], 100).map(|v| format!("{}.{}", v >> 16, v & 0xFFFF)).unwrap_or_default()
        }

        /// Number of added displays.
        ///
        /// # Returns
        /// The count.
        pub(super) fn count(&self) -> usize {
            self.added.len()
        }

        /// Adds a display.
        ///
        /// # Returns
        /// Its driver index.
        ///
        /// # Errors
        /// See [`super::VirtualDisplays::add`].
        pub(super) fn add(&mut self) -> Result<u32> {
            if self.added.len() >= MAX_DISPLAYS {
                return Err(PlatformError::Unsupported("more virtual monitors"));
            }
            let index = self.device.control(IOCTL_ADD, &[], 5000)?;
            let _ = self.device.control(IOCTL_UPDATE, &[], 1000);
            self.added.push(index);
            Ok(index)
        }

        /// Removes the last added display.
        ///
        /// # Returns
        /// Whether one was removed.
        ///
        /// # Errors
        /// See [`super::VirtualDisplays::remove`].
        pub(super) fn remove(&mut self) -> Result<bool> {
            let Some(&index) = self.added.last() else { return Ok(false) };
            let be = (index as u16).to_be_bytes();
            let started = std::time::Instant::now();
            self.device.control(IOCTL_REMOVE, &be, REMOVE_TIMEOUT_MS)?;
            debug!(index, elapsed_ms = started.elapsed().as_millis() as u64, "virtual display removed");
            let _ = self.device.control(IOCTL_UPDATE, &[], 1000);
            self.added.pop();
            Ok(true)
        }
    }

    impl Drop for Controller {
        /// Removes the added displays (newest first, as Parsec's tools do), then stops the
        /// keep-alive; the handle closes with the last reference.
        fn drop(&mut self) {
            while !self.added.is_empty() {
                if let Err(e) = self.remove() {
                    // The driver drops it anyway once the keep-alive stops.
                    warn!(error = %e, "cannot remove a virtual monitor");
                    self.added.pop();
                }
            }
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.pinger.take() {
                let _ = t.join();
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Adds one display, lists the monitors Windows reports, then removes it.  `VD_VDD_HOLD=<s>`
    /// keeps the display for that many seconds (to try capturing it meanwhile).
    #[test]
    #[ignore = "needs the Parsec Virtual Display Driver; adds a display for a few seconds"]
    fn adds_and_removes_a_display() {
        // Physical pixels, as in the streamer (otherwise scaled monitors report smaller sizes).
        crate::display::enable_dpi_awareness();
        let before = crate::display::monitors().unwrap();
        let mut vd = VirtualDisplays::open().unwrap();
        println!("driver {}", vd.version());
        println!("added index {}", vd.add().unwrap());
        if std::env::var_os("VD_VDD_TWO").is_some() {
            println!("added index {}", vd.add().unwrap());
            std::thread::sleep(std::time::Duration::from_secs(3));
            let t = std::time::Instant::now();
            let r = vd.remove();
            println!("remove of the second: {r:?} after {:?}", t.elapsed());
        }
        let hold = std::env::var("VD_VDD_HOLD").ok().and_then(|s| s.parse().ok()).unwrap_or(3);
        std::thread::sleep(std::time::Duration::from_secs(hold));
        let during = crate::display::monitors().unwrap();
        for m in &during {
            println!("{m:?}");
        }
        assert_eq!(during.len(), before.len() + 1);
        let added = during.iter().find(|m| !before.iter().any(|b| b.id == m.id)).unwrap();
        assert!(added.is_virtual);
        println!("modes: {:?}", crate::display::modes(&added.id));
        crate::display::set_mode(&added.id, 2560, 1440, 60).unwrap();
        let changed = crate::display::monitors().unwrap().into_iter().find(|m| m.id == added.id).unwrap();
        println!("after set_mode: {changed:?}");
        assert_eq!((changed.width, changed.height), (2560, 1440));
        assert!(vd.remove().unwrap());
        drop(vd);
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert_eq!(crate::display::monitors().unwrap().len(), before.len());
    }
}
