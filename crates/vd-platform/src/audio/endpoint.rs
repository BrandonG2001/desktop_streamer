//! Default audio device switching (Windows).  C#: `IAudioService.SetVirtualAudioDeviceRender` /
//! `SetVirtualAudioDeviceCapture` / `RestoreDefaultAudioDeviceRender` /
//! `RestoreDefaultAudioDeviceCapture`, implemented by `VirtualDesktop.Service.exe`.
//!
//! The `vdvad.sys` driver exposes **Speakers (Virtual Desktop Audio)** and **Microphone (Virtual
//! Desktop Audio)**.  The speaker endpoint is kept *hidden* (`DeviceState` has the
//! `0x10000000` flag) while no session runs.  In "VR headset only" mode the service makes it
//! visible and the default playback device for every role, so applications play into a device
//! with no physical output; `LoopbackCapture` then records that device.  When the session ends
//! the previous defaults are restored and the endpoint is hidden again.  "Auto-select microphone"
//! does the same for the capture side.
//!
//! Windows has no documented API to change the default device; like the Sound control panel
//! (and the original service) this uses the `IPolicyConfig` COM interface of
//! `CLSID_PolicyConfigClient`.  The previous configuration is also written to a file in the temp
//! directory so that [`recover`] can undo the switch after a crash.

use std::ffi::c_void;
use std::path::PathBuf;

use tracing::{debug, info, warn};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Media::Audio::{EDataFlow, ERole, IMMDeviceEnumerator, eCapture, eCommunications, eConsole, eMultimedia, eRender};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW};
use windows::core::{GUID, HRESULT, HSTRING, IUnknown, Interface, PCWSTR, PWSTR};

/// Interface name shared by both endpoints of the driver.
pub const VDVAD_INTERFACE_NAME: &str = "Virtual Desktop Audio";
/// `CLSID_PolicyConfigClient`.
const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
/// `IID_IPolicyConfig` (Windows 7 and later).
const IID_POLICY_CONFIG: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);
/// `DeviceState` flag of endpoints hidden with `SetEndpointVisibility(false)`.
const DEVICE_STATE_HIDDEN: u32 = 0x1000_0000;
/// `PKEY_DeviceInterface_FriendlyName` as stored under `Properties`.
const PKEY_INTERFACE_NAME: &str = "{b3f8fa53-0004-438e-9003-51a46e139bfc},6";
/// All roles a default device is set for.
const ROLES: [ERole; 3] = [eConsole, eMultimedia, eCommunications];

/// Registry key and endpoint-id prefix of a data flow.
///
/// # Arguments
/// * `flow` - render or capture.
///
/// # Returns
/// `(registry sub-key, id prefix)`.
fn flow_info(flow: EDataFlow) -> (&'static str, &'static str) {
    if flow == eCapture {
        (r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture", "{0.0.1.00000000}.")
    } else {
        (r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render", "{0.0.0.00000000}.")
    }
}

/// Reads a `REG_SZ` value.
///
/// # Arguments
/// * `key` - open key.
/// * `sub` - sub-key path.
/// * `name` - value name.
///
/// # Returns
/// The string, or `None`.
fn reg_string(key: HKEY, sub: &str, name: &str) -> Option<String> {
    let (sub, name) = (HSTRING::from(sub), HSTRING::from(name));
    let mut buf = [0u16; 512];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: the buffer and its byte length are valid for the call.
    let rc = unsafe { RegGetValueW(key, &sub, &name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut len)) };
    if rc != ERROR_SUCCESS {
        return None;
    }
    let n = (len as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..n.min(buf.len())]))
}

/// Reads a `REG_DWORD` value.
///
/// # Arguments
/// * `key` - open key.
/// * `sub` - sub-key path.
/// * `name` - value name.
///
/// # Returns
/// The value, or `None`.
fn reg_dword(key: HKEY, sub: &str, name: &str) -> Option<u32> {
    let (sub, name) = (HSTRING::from(sub), HSTRING::from(name));
    let mut v = 0u32;
    let mut len = 4u32;
    // SAFETY: `v` is a 4-byte buffer.
    let rc = unsafe { RegGetValueW(key, &sub, &name, RRF_RT_REG_DWORD, None, Some((&mut v as *mut u32).cast()), Some(&mut len)) };
    (rc == ERROR_SUCCESS).then_some(v)
}

/// Finds the driver's endpoint of a flow, including hidden ones (which
/// `IMMDeviceEnumerator::EnumAudioEndpoints` does not list).
///
/// # Arguments
/// * `flow` - render or capture.
///
/// # Returns
/// `(endpoint id, hidden)`, or `None` when the driver is not installed.
pub fn find_virtual_endpoint(flow: EDataFlow) -> Option<(String, bool)> {
    let (path, prefix) = flow_info(flow);
    let mut key = HKEY::default();
    // SAFETY: opening a read-only key; closed below.
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, &HSTRING::from(path), 0, KEY_READ, &mut key) } != ERROR_SUCCESS {
        return None;
    }
    let mut found = None;
    for i in 0.. {
        let mut name = [0u16; 128];
        let mut len = name.len() as u32;
        // SAFETY: `name` holds `len` UTF-16 units.
        if unsafe { RegEnumKeyExW(key, i, PWSTR(name.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None) } != ERROR_SUCCESS {
            break;
        }
        let guid = String::from_utf16_lossy(&name[..len as usize]);
        let iface = reg_string(key, &format!(r"{guid}\Properties"), PKEY_INTERFACE_NAME);
        let state = reg_dword(key, &guid, "DeviceState").unwrap_or(0);
        // Only present devices (DEVICE_STATE_ACTIVE, possibly hidden).
        if iface.as_deref() == Some(VDVAD_INTERFACE_NAME) && state & 0xF == 1 {
            found = Some((format!("{prefix}{guid}"), state & DEVICE_STATE_HIDDEN != 0));
            break;
        }
    }
    // SAFETY: closing the key opened above.
    unsafe {
        let _ = RegCloseKey(key);
    }
    found
}

/// Raw vtable of `IPolicyConfig` (only the slots used here are typed).
#[repr(C)]
struct PolicyConfigVtbl {
    unknown: [usize; 3],
    /// `GetMixFormat` … `SetPropertyValue` (10 methods).
    _unused: [usize; 10],
    set_default_endpoint: unsafe extern "system" fn(this: *mut c_void, id: PCWSTR, role: ERole) -> HRESULT,
    set_endpoint_visibility: unsafe extern "system" fn(this: *mut c_void, id: PCWSTR, visible: i32) -> HRESULT,
}

/// `IPolicyConfig` instance.
struct PolicyConfig {
    /// Keeps the object alive.
    _unknown: IUnknown,
    /// `IPolicyConfig` pointer (one reference owned by this struct).
    raw: *mut c_void,
}

impl PolicyConfig {
    /// Creates the policy object (COM must be initialised on this thread).
    ///
    /// # Returns
    /// The object, or `None` when it is unavailable.
    fn new() -> Option<Self> {
        // SAFETY: creating and querying a COM object on an initialised thread.
        unsafe {
            let unknown: IUnknown = CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL).ok()?;
            let mut raw = std::ptr::null_mut();
            unknown.query(&IID_POLICY_CONFIG, &mut raw).ok().ok()?;
            Some(Self { _unknown: unknown, raw })
        }
    }

    /// The vtable.
    ///
    /// # Returns
    /// A reference to the object's vtable.
    fn vtbl(&self) -> &PolicyConfigVtbl {
        // SAFETY: `raw` is a live IPolicyConfig whose first field is its vtable pointer.
        unsafe { &**(self.raw as *const *const PolicyConfigVtbl) }
    }

    /// Makes `id` the default endpoint for `role`.
    ///
    /// # Arguments
    /// * `id` - endpoint id.
    /// * `role` - device role.
    ///
    /// # Returns
    /// `true` on success.
    fn set_default(&self, id: &str, role: ERole) -> bool {
        let id = HSTRING::from(id);
        // SAFETY: valid object and NUL-terminated id.
        unsafe { (self.vtbl().set_default_endpoint)(self.raw, PCWSTR(id.as_ptr()), role) }.is_ok()
    }

    /// Shows or hides an endpoint.
    ///
    /// # Arguments
    /// * `id` - endpoint id.
    /// * `visible` - new visibility.
    ///
    /// # Returns
    /// `true` on success.
    fn set_visible(&self, id: &str, visible: bool) -> bool {
        let id = HSTRING::from(id);
        // SAFETY: valid object and NUL-terminated id.
        unsafe { (self.vtbl().set_endpoint_visibility)(self.raw, PCWSTR(id.as_ptr()), i32::from(visible)) }.is_ok()
    }
}

impl Drop for PolicyConfig {
    /// Releases the `IPolicyConfig` reference.
    fn drop(&mut self) {
        // SAFETY: balances the reference returned by QueryInterface.
        unsafe {
            let release: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(self.vtbl().unknown[2]);
            release(self.raw);
        }
    }
}

/// File recording a switch in progress, used by [`recover`].
///
/// # Arguments
/// * `flow` - render or capture.
///
/// # Returns
/// The path in the temp directory.
fn restore_file(flow: EDataFlow) -> PathBuf {
    std::env::temp_dir().join(if flow == eCapture { "vd-rust-default-capture.txt" } else { "vd-rust-default-render.txt" })
}

/// Saved state: the virtual endpoint, whether it was hidden, and the previous default per role.
#[derive(Debug, Clone)]
struct Saved {
    virtual_id: String,
    was_hidden: bool,
    previous: Vec<String>,
}

impl Saved {
    /// Serialises as lines: virtual id, hidden flag, one previous id per role.
    ///
    /// # Returns
    /// The text.
    fn to_text(&self) -> String {
        let mut lines = vec![self.virtual_id.clone(), u8::from(self.was_hidden).to_string()];
        lines.extend(self.previous.iter().cloned());
        lines.join("\n")
    }

    /// Parses [`Saved::to_text`] output.
    ///
    /// # Arguments
    /// * `text` - file contents.
    ///
    /// # Returns
    /// The state, or `None` if malformed.
    fn from_text(text: &str) -> Option<Self> {
        let mut it = text.lines();
        let virtual_id = it.next()?.to_owned();
        let was_hidden = it.next()? == "1";
        Some(Self { virtual_id, was_hidden, previous: it.map(str::to_owned).collect() })
    }

    /// Puts the previous defaults back and re-hides the virtual endpoint.
    ///
    /// # Arguments
    /// * `policy` - policy object.
    fn restore(&self, policy: &PolicyConfig) {
        for (role, prev) in ROLES.iter().zip(&self.previous) {
            if !prev.is_empty() && *prev != self.virtual_id && !policy.set_default(prev, *role) {
                warn!(device = %prev, "cannot restore the default audio device");
            }
        }
        if self.was_hidden {
            policy.set_visible(&self.virtual_id, false);
        }
    }
}

/// While alive, the Virtual Desktop endpoint of one flow is the system default.
pub struct DefaultSwitch {
    flow: EDataFlow,
    saved: Saved,
}

impl DefaultSwitch {
    /// Makes the virtual endpoint visible and the default for every role.
    ///
    /// # Arguments
    /// * `enumerator` - device enumerator (to read the current defaults).
    /// * `flow` - render or capture.
    /// * `id` - virtual endpoint id from [`find_virtual_endpoint`].
    /// * `hidden` - the endpoint is currently hidden.
    ///
    /// # Returns
    /// The switch (restored on drop), or `None` if `IPolicyConfig` is unavailable or refused.
    pub fn activate(enumerator: &IMMDeviceEnumerator, flow: EDataFlow, id: &str, hidden: bool) -> Option<Self> {
        let policy = PolicyConfig::new()?;
        let previous: Vec<String> = ROLES
            .iter()
            // SAFETY: enumerator is valid on this COM thread.
            .map(|r| unsafe { enumerator.GetDefaultAudioEndpoint(flow, *r) }.map(|d| super::wasapi::device_id(&d)).unwrap_or_default())
            .collect();
        let saved = Saved { virtual_id: id.to_owned(), was_hidden: hidden, previous };
        // Record before switching so a crash can be undone at the next start.
        if let Err(e) = std::fs::write(restore_file(flow), saved.to_text()) {
            debug!(error = %e, "cannot write the audio restore file");
        }
        if hidden && !policy.set_visible(id, true) {
            warn!("cannot show the Virtual Desktop Audio endpoint");
        }
        let ok = ROLES.iter().all(|r| policy.set_default(id, *r));
        if !ok {
            saved.restore(&policy);
            let _ = std::fs::remove_file(restore_file(flow));
            return None;
        }
        info!(flow = if flow == eRender { "playback" } else { "recording" }, "Virtual Desktop Audio is now the default device");
        Some(Self { flow, saved })
    }
}

impl Drop for DefaultSwitch {
    /// Restores the previous default devices (COM must be initialised on this thread).
    fn drop(&mut self) {
        if let Some(policy) = PolicyConfig::new() {
            self.saved.restore(&policy);
            info!(flow = if self.flow == eRender { "playback" } else { "recording" }, "previous default audio device restored");
        }
        let _ = std::fs::remove_file(restore_file(self.flow));
    }
}

/// Undoes a switch left behind by a crash (C#: the service restores the defaults when the
/// streamer process it was called for exits).  COM must be initialised on this thread.
pub fn recover() {
    for flow in [eRender, eCapture] {
        let path = restore_file(flow);
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        if let (Some(saved), Some(policy)) = (Saved::from_text(&text), PolicyConfig::new()) {
            info!("restoring the default audio device after an unclean exit");
            saved.restore(&policy);
        }
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_state_round_trips() {
        let s = Saved { virtual_id: "{0.0.0.00000000}.{a}".into(), was_hidden: true, previous: vec!["x".into(), "y".into(), "z".into()] };
        let back = Saved::from_text(&s.to_text()).unwrap();
        assert_eq!(back.virtual_id, s.virtual_id);
        assert!(back.was_hidden);
        assert_eq!(back.previous, ["x", "y", "z"]);
    }
}
