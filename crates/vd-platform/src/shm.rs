//! Named shared memory and events — the IPC contract between the streamer and the VR drivers
//! (`OpenVRDriver/driver_VirtualDesktop.dll`, `OpenXR/virtualdesktop-openxr.dll`,
//! `VirtualDesktop.LibOVRRT64_1.dll`) and the input-injection service.
//!
//! C#: `OculusStreamer` creates these with `MemoryMappedFile.CreateOrOpen` and
//! `EventWaitHandle` (names below).  A Linux port of the drivers would open the same names with
//! POSIX shared memory (`/dev/shm/<name>`) and named semaphores, which is what [`SharedMemory`]
//! and [`NamedEvent`] implement on Unix.  POSIX semaphores behave like **auto-reset** events;
//! manual-reset semantics are approximated by posting once per waiter.

use crate::error::{PlatformError, Result};

/// Object names used by the original (session namespace on Windows).
pub mod names {
    /// Driver ↔ streamer frame/pose exchange (`_001E._0002`, 464 bytes).
    pub const DRIVER_DATA: &str = "VirtualDesktop.Oculus.DriverData";
    /// Guardian boundary geometry.
    pub const BOUNDARY_DATA: &str = "VirtualDesktop.Oculus.BoundaryData";
    /// Body/hand/face tracking forwarded to the drivers (`_008A._0002`, 9792 bytes).
    pub const BODY_STATE: &str = "VirtualDesktop.BodyState";
    /// Signalled when `BodyState` is updated.
    pub const BODY_STATE_EVENT: &str = "VirtualDesktop.BodyStateEvent";
    /// Second `BodyState` event (other consumer).
    pub const BODY_STATE_EVENT2: &str = "VirtualDesktop.BodyStateEvent2";
    /// A driver operation starts.
    pub const OPERATION_START: &str = "VirtualDesktop.Oculus.OperationStartEvent";
    /// A driver operation ended.
    pub const OPERATION_END: &str = "VirtualDesktop.Oculus.OperationEndEvent";
    /// A new head pose is available.
    pub const POSE_UPDATED: &str = "VirtualDesktop.Oculus.PoseUpdatedEvent";
    /// The VR app presented a frame.
    pub const PRESENT: &str = "VirtualDesktop.Oculus.PresentEvent";
    /// The streamer finished encoding the frame (manual reset).
    pub const RENDERED: &str = "VirtualDesktop.Oculus.RenderedEvent";
    /// Input injection state ([`vd_protocol::headset_state::InputInjectionShared`], 108 bytes).
    pub const INPUT_INJECTION: &str = "VirtualDesktop.InputInjection";
    /// Global mutex/event names of the single-instance logic.
    pub const SHOW_WINDOW: &str = "Global\\VirtualDesktop.ShowWindow";
    /// Ask the running instance to exit.
    pub const EXIT: &str = "Global\\VirtualDesktop.Exit";
    /// Set while protected (DRM) content is on screen.
    pub const PROTECTED_CONTENT: &str = "Global\\VirtualDesktop.ProtectedContent";
}

/// A named, read-write shared memory region.
pub struct SharedMemory {
    ptr: *mut u8,
    len: usize,
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(unix)]
    name: std::ffi::CString,
    #[cfg(unix)]
    owner: bool,
}

// SAFETY: the mapping is process-wide memory; synchronisation is the caller's responsibility
// (exactly as with the original's MemoryMappedViewAccessor).
unsafe impl Send for SharedMemory {}

impl SharedMemory {
    /// Creates (or opens) a region of `len` bytes.  C#: `MemoryMappedFile.CreateOrOpen`.
    ///
    /// # Arguments
    /// * `name` - object name (see [`names`]).
    /// * `len` - size in bytes.
    ///
    /// # Returns
    /// The mapped region (zero-filled when newly created).
    ///
    /// # Errors
    /// OS errors from mapping.
    pub fn create_or_open(name: &str, len: usize) -> Result<Self> {
        imp::create_or_open(name, len)
    }

    /// Borrows the region.
    ///
    /// # Returns
    /// The bytes.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` maps `len` bytes for the lifetime of `self`.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// Mutably borrows the region.
    ///
    /// # Returns
    /// The bytes.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above; `&mut self` guarantees no aliasing within this process.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    /// Copies a `Pod` value into the start of the region.
    ///
    /// # Arguments
    /// * `value` - value to write.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] if the region is smaller than the value.
    pub fn write_pod<T: bytemuck_like::Pod>(&mut self, value: &T) -> Result<()> {
        let bytes = value.as_bytes();
        if bytes.len() > self.len {
            return Err(PlatformError::Unsupported("shared memory region too small"));
        }
        self.as_mut_slice()[..bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
}

/// Minimal "plain old data" abstraction so this crate does not depend on `bytemuck` directly.
pub mod bytemuck_like {
    /// Types that can be viewed as raw bytes.
    pub trait Pod {
        /// Returns the value's bytes.
        ///
        /// # Returns
        /// The byte view.
        fn as_bytes(&self) -> &[u8];
    }

    impl Pod for vd_protocol::headset_state::InputInjectionShared {
        /// Returns the struct bytes.
        ///
        /// # Returns
        /// 108 bytes.
        fn as_bytes(&self) -> &[u8] {
            // SAFETY: `InputInjectionShared` is `#[repr(C)]` + `Pod` with explicit padding.
            unsafe { std::slice::from_raw_parts((self as *const Self).cast::<u8>(), std::mem::size_of::<Self>()) }
        }
    }
}

impl Drop for SharedMemory {
    /// Unmaps the region (and unlinks it on Unix if this process created it).
    fn drop(&mut self) {
        imp::close(self);
    }
}

/// A named event.
pub struct NamedEvent {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(unix)]
    sem: *mut libc::sem_t,
}

// SAFETY: event handles / semaphores are usable from any thread.
unsafe impl Send for NamedEvent {}
// SAFETY: as above.
unsafe impl Sync for NamedEvent {}

impl NamedEvent {
    /// Creates or opens an event.  C#: `new EventWaitHandle(false, mode, name, …)`.
    ///
    /// # Arguments
    /// * `name` - event name.
    /// * `manual_reset` - manual-reset event (Windows only; Unix events are auto-reset).
    ///
    /// # Returns
    /// The event.
    ///
    /// # Errors
    /// OS errors.
    pub fn create_or_open(name: &str, manual_reset: bool) -> Result<Self> {
        imp::event_create(name, manual_reset)
    }

    /// Signals the event.
    ///
    /// # Errors
    /// OS errors.
    pub fn set(&self) -> Result<()> {
        imp::event_set(self)
    }

    /// Waits for the event.
    ///
    /// # Arguments
    /// * `timeout` - maximum wait.
    ///
    /// # Returns
    /// `true` if signalled, `false` on timeout.
    ///
    /// # Errors
    /// OS errors.
    pub fn wait(&self, timeout: std::time::Duration) -> Result<bool> {
        imp::event_wait(self, timeout)
    }
}

impl Drop for NamedEvent {
    /// Closes the handle.
    fn drop(&mut self) {
        imp::event_close(self);
    }
}

#[cfg(windows)]
mod imp {
    use super::{NamedEvent, SharedMemory};
    use crate::error::{PlatformError, Result};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READWRITE, UnmapViewOfFile};
    use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last(api: &'static str) -> PlatformError {
        PlatformError::Win32 { api, code: std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32 }
    }

    pub fn create_or_open(name: &str, len: usize) -> Result<SharedMemory> {
        let w = wide(name);
        // SAFETY: valid NUL-terminated name; INVALID_HANDLE_VALUE requests pagefile backing.
        let h = unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE, (len as u64 >> 32) as u32, len as u32, w.as_ptr()) };
        if h.is_null() {
            return Err(last("CreateFileMappingW"));
        }
        // SAFETY: `h` is a valid mapping handle of at least `len` bytes.
        let view = unsafe { MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, len) };
        if view.Value.is_null() {
            unsafe { CloseHandle(h) };
            return Err(last("MapViewOfFile"));
        }
        Ok(SharedMemory { ptr: view.Value.cast(), len, handle: h })
    }

    pub fn close(m: &mut SharedMemory) {
        // SAFETY: pointer/handle came from MapViewOfFile/CreateFileMappingW.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: m.ptr.cast() });
            CloseHandle(m.handle);
        }
    }

    pub fn event_create(name: &str, manual_reset: bool) -> Result<NamedEvent> {
        let w = wide(name);
        // SAFETY: valid NUL-terminated name.
        let h = unsafe { CreateEventW(std::ptr::null(), i32::from(manual_reset), 0, w.as_ptr()) };
        if h.is_null() {
            return Err(last("CreateEventW"));
        }
        Ok(NamedEvent { handle: h })
    }

    pub fn event_set(e: &NamedEvent) -> Result<()> {
        // SAFETY: valid event handle.
        if unsafe { SetEvent(e.handle) } == 0 { Err(last("SetEvent")) } else { Ok(()) }
    }

    pub fn event_wait(e: &NamedEvent, timeout: std::time::Duration) -> Result<bool> {
        // SAFETY: valid event handle.
        match unsafe { WaitForSingleObject(e.handle, timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(last("WaitForSingleObject")),
        }
    }

    pub fn event_close(e: &mut NamedEvent) {
        // SAFETY: valid handle owned by `e`.
        unsafe { CloseHandle(e.handle) };
    }
}

#[cfg(unix)]
mod imp {
    use std::ffi::CString;

    use super::{NamedEvent, SharedMemory};
    use crate::error::{PlatformError, Result};

    fn posix_name(name: &str) -> CString {
        // POSIX names must start with '/' and contain no other '/'; backslashes ("Global\") too.
        let clean: String = name.chars().map(|c| if c == '/' || c == '\\' { '_' } else { c }).collect();
        CString::new(format!("/{clean}")).expect("no interior NUL")
    }

    pub fn create_or_open(name: &str, len: usize) -> Result<SharedMemory> {
        let cname = posix_name(name);
        // SAFETY: valid C string; standard flags.
        let mut owner = true;
        let mut fd = unsafe { libc::shm_open(cname.as_ptr(), libc::O_CREAT | libc::O_EXCL | libc::O_RDWR, 0o600) };
        if fd < 0 {
            owner = false;
            fd = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDWR, 0o600) };
        }
        if fd < 0 {
            return Err(PlatformError::Io(std::io::Error::last_os_error()));
        }
        // SAFETY: `fd` is a valid shared-memory descriptor.
        if owner && unsafe { libc::ftruncate(fd, len as libc::off_t) } != 0 {
            let e = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(PlatformError::Io(e));
        }
        // SAFETY: mapping `len` bytes of a descriptor sized to at least `len`.
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0) };
        unsafe { libc::close(fd) };
        if ptr == libc::MAP_FAILED {
            return Err(PlatformError::Io(std::io::Error::last_os_error()));
        }
        Ok(SharedMemory { ptr: ptr.cast(), len, name: cname, owner })
    }

    pub fn close(m: &mut SharedMemory) {
        // SAFETY: `ptr`/`len` came from mmap.
        unsafe {
            libc::munmap(m.ptr.cast(), m.len);
            if m.owner {
                libc::shm_unlink(m.name.as_ptr());
            }
        }
    }

    pub fn event_create(name: &str, _manual_reset: bool) -> Result<NamedEvent> {
        let cname = posix_name(name);
        // SAFETY: variadic sem_open with mode and initial value, as documented.
        let sem = unsafe { libc::sem_open(cname.as_ptr(), libc::O_CREAT, 0o600 as libc::c_uint, 0 as libc::c_uint) };
        if sem == libc::SEM_FAILED {
            return Err(PlatformError::Io(std::io::Error::last_os_error()));
        }
        Ok(NamedEvent { sem })
    }

    pub fn event_set(e: &NamedEvent) -> Result<()> {
        // SAFETY: valid semaphore.
        if unsafe { libc::sem_post(e.sem) } != 0 { Err(PlatformError::Io(std::io::Error::last_os_error())) } else { Ok(()) }
    }

    pub fn event_wait(e: &NamedEvent, timeout: std::time::Duration) -> Result<bool> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            // SAFETY: valid semaphore.
            if unsafe { libc::sem_trywait(e.sem) } == 0 {
                return Ok(true);
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EAGAIN) && err.raw_os_error() != Some(libc::EINTR) {
                return Err(PlatformError::Io(err));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(std::time::Duration::from_micros(250));
        }
    }

    pub fn event_close(e: &mut NamedEvent) {
        // SAFETY: valid semaphore opened by sem_open.
        unsafe { libc::sem_close(e.sem) };
    }
}
