//! Windows audio back-end.
//!
//! * **PC → headset** — C#: `LoopbackCapture` (CSCore `WasapiLoopbackCapture`).  WASAPI loopback
//!   of the default render endpoint (`eRender`/`eConsole`) in the endpoint's mix format, converted
//!   to 48 kHz stereo `s16le` with gain 0.9 and cut into 1152-byte packets.  See
//!   [`RenderRoute`] for how "VR headset only" keeps the PC silent: normally the hidden
//!   **Speakers (Virtual Desktop Audio)** endpoint becomes the default device and is recorded
//!   ([`super::endpoint`]); without the driver the default endpoint is muted
//!   (`AudioEndpointVolume.IsMuted = true`) while it is recorded.  When recording the default
//!   device the stream is re-opened when it changes (`MMDeviceEnumerator.DefaultDeviceChanged`).
//! * **headset → PC microphone** — C#: `Microphone._0002()`.  The original enumerates
//!   `AM_KSCATEGORY_CAPTURE` DirectShow filters, binds the one named **"VDVAD Wave"** (the
//!   `vdvad.sys` virtual audio driver installed with Virtual Desktop) to `IKsControl`, and pushes
//!   48 kHz mono `s16le` PCM with `KsProperty(SET)` on property set
//!   `{15E0F7CE-5521-4214-A1D1-17DEDD428BE9}`, id 0.  The driver exposes it as the
//!   **Microphone (Virtual Desktop Audio)** capture endpoint.  When more than 7680 bytes are
//!   queued, older audio is dropped down to 1920 bytes to bound latency.  The driver only accepts
//!   samples while an application records from that endpoint (otherwise `KsProperty` returns
//!   `ERROR_INVALID_FUNCTION`, which is ignored — verified by recording the endpoint while
//!   pushing a 440 Hz tone).
//!
//! COM objects are apartment-bound, so each direction runs on its own thread initialised as
//! MTA; the async side talks to them through channels.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use vd_protocol::channels::audio::{SPEAKER_GAIN, SPEAKER_PACKET_BYTES};
use windows::Win32::Foundation::BOOL;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eCapture, eConsole, eRender,
};
use windows::Win32::Media::DirectShow::ICreateDevEnum;
use windows::Win32::Media::KernelStreaming::{IKsControl, KSIDENTIFIER};
use windows::Win32::System::Com::StructuredStorage::IPropertyBag;
use windows::Win32::System::Com::{CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IBindCtx, IEnumMoniker, IMoniker};
use windows::core::{BSTR, GUID, HSTRING, VARIANT, w};

use super::convert::{Packetizer, SampleFormat, StereoResampler, decode_to_f32};
use super::endpoint::{DefaultSwitch, find_virtual_endpoint};
use crate::error::{PlatformError, Result};

/// `CLSID_SystemDeviceEnum`.
const CLSID_SYSTEM_DEVICE_ENUM: GUID = GUID::from_u128(0x62be5d10_60eb_11d0_bd3b_00a0c911ce86);
/// `AM_KSCATEGORY_CAPTURE`.
const AM_KSCATEGORY_CAPTURE: GUID = GUID::from_u128(0x65e8773d_8f56_11d0_a3b9_00a0c9223196);
/// Property set of the VDVAD driver's "write microphone samples" property (`Microphone.m__0001`).
pub const VDVAD_MIC_PROPERTY_SET: GUID = GUID::from_u128(0x15e0f7ce_5521_4214_a1d1_17dedd428be9);
/// Friendly name of the driver's KS filter.
pub const VDVAD_FILTER_NAME: &str = "VDVAD Wave";
/// `KSPROPERTY_TYPE_SET`.
const KSPROPERTY_TYPE_SET: u32 = 2;
/// Microphone chunk size (20 ms of 48 kHz mono s16).
const MIC_CHUNK: usize = 1920;
/// Backlog above which old microphone audio is dropped (`> 7680` in the original).
const MIC_MAX_BACKLOG: usize = 7680;

fn win(e: windows::core::Error, api: &'static str) -> PlatformError {
    PlatformError::Win32 { api, code: e.code().0 as u32 }
}

/// RAII guard for `CoInitializeEx`/`CoUninitialize` on the current thread.
struct ComGuard(bool);

impl ComGuard {
    /// Initialises COM (MTA).
    ///
    /// # Returns
    /// The guard; `CoUninitialize` is only called if initialisation succeeded.
    fn new() -> Self {
        // SAFETY: plain COM initialisation for this thread.
        let ok = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        Self(ok)
    }
}

impl Drop for ComGuard {
    /// Balances a successful `CoInitializeEx`.
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balanced with the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }
}

/// Endpoint identifier string of a device.
///
/// # Arguments
/// * `device` - endpoint.
///
/// # Returns
/// The id (empty on failure).
pub(crate) fn device_id(device: &IMMDevice) -> String {
    // SAFETY: GetId returns a CoTaskMem string that we free after copying.
    unsafe {
        match device.GetId() {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const c_void));
                s
            }
            Err(_) => String::new(),
        }
    }
}

/// Decoded WASAPI mix format.
#[derive(Debug, Clone, Copy)]
struct MixFormat {
    rate: u32,
    channels: usize,
    block_align: usize,
    sample: SampleFormat,
}

/// Interprets a `WAVEFORMATEX` (possibly `WAVEFORMATEXTENSIBLE`).
///
/// # Arguments
/// * `p` - pointer returned by `GetMixFormat`.
///
/// # Returns
/// The decoded format.
///
/// # Errors
/// [`PlatformError::Unsupported`] for encodings other than PCM integer / IEEE float.
///
/// # Safety
/// `p` must point to a valid format block.
unsafe fn parse_format(p: *const WAVEFORMATEX) -> Result<MixFormat> {
    // SAFETY: caller guarantees validity; the struct is packed so read unaligned.
    let f: WAVEFORMATEX = unsafe { std::ptr::read_unaligned(p) };
    let tag = f.wFormatTag;
    let bits = f.wBitsPerSample;
    let is_float = match tag {
        3 => true,  // WAVE_FORMAT_IEEE_FLOAT
        1 => false, // WAVE_FORMAT_PCM
        0xFFFE => {
            // SAFETY: tag says the block is a WAVEFORMATEXTENSIBLE.
            let ext: WAVEFORMATEXTENSIBLE = unsafe { std::ptr::read_unaligned(p.cast()) };
            let sub = ext.SubFormat;
            match sub.data1 {
                3 => true,
                1 => false,
                _ => return Err(PlatformError::Unsupported("mix format sub-type")),
            }
        }
        _ => return Err(PlatformError::Unsupported("mix format tag")),
    };
    let sample = match (is_float, bits) {
        (true, 32) => SampleFormat::F32,
        (false, 16) => SampleFormat::I16,
        (false, 24) => SampleFormat::I24,
        (false, 32) => SampleFormat::I32,
        _ => return Err(PlatformError::Unsupported("mix format bit depth")),
    };
    Ok(MixFormat { rate: f.nSamplesPerSec, channels: usize::from(f.nChannels), block_align: usize::from(f.nBlockAlign), sample })
}

/// An open loopback stream on one endpoint.
struct Loopback {
    device_id: String,
    client: IAudioClient,
    capture: IAudioCaptureClient,
    format: MixFormat,
    /// Mute state to restore, when this stream muted the endpoint.
    restore_mute: Option<(IAudioEndpointVolume, bool)>,
}

impl Loopback {
    /// Opens loopback capture on a render endpoint.
    ///
    /// # Arguments
    /// * `enumerator` - device enumerator.
    /// * `wanted` - endpoint id to record, or `None` for the default device.
    /// * `mute` - mute the endpoint while capturing (headset-only mode without the driver).
    ///
    /// # Returns
    /// The started stream.
    ///
    /// # Errors
    /// Win32 errors from WASAPI.
    fn open(enumerator: &IMMDeviceEnumerator, wanted: Option<&str>, mute: bool) -> Result<Self> {
        // SAFETY: standard WASAPI call sequence on an MTA thread; pointers come from the API.
        unsafe {
            let device = match wanted {
                Some(id) => enumerator.GetDevice(&HSTRING::from(id)).map_err(|e| win(e, "IMMDeviceEnumerator::GetDevice"))?,
                None => enumerator.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|e| win(e, "GetDefaultAudioEndpoint"))?,
            };
            let id = device_id(&device);
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| win(e, "IMMDevice::Activate(IAudioClient)"))?;
            let pfmt = client.GetMixFormat().map_err(|e| win(e, "GetMixFormat"))?;
            let parsed = parse_format(pfmt);
            // 100 ms shared-mode buffer, polled every few milliseconds.
            let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 1_000_000, 0, pfmt, None);
            CoTaskMemFree(Some(pfmt as *const c_void));
            let format = parsed?;
            init.map_err(|e| win(e, "IAudioClient::Initialize(loopback)"))?;
            let capture: IAudioCaptureClient = client.GetService().map_err(|e| win(e, "GetService(IAudioCaptureClient)"))?;
            let restore_mute = if mute {
                match device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) {
                    Ok(vol) => {
                        let was = vol.GetMute().map(|b| b.as_bool()).unwrap_or(false);
                        match vol.SetMute(BOOL::from(true), std::ptr::null()) {
                            Ok(()) => info!("PC speakers muted while streaming"),
                            Err(e) => warn!(error = %e, "cannot mute PC speakers"),
                        }
                        Some((vol, was))
                    }
                    Err(e) => {
                        warn!(error = %e, "cannot open endpoint volume");
                        None
                    }
                }
            } else {
                None
            };
            client.Start().map_err(|e| win(e, "IAudioClient::Start"))?;
            info!(device = %id, rate = format.rate, channels = format.channels, sample = ?format.sample, "WASAPI loopback capture started");
            Ok(Self { device_id: id, client, capture, format, restore_mute })
        }
    }

    /// Drains every available packet into `out` (raw bytes in the mix format).
    ///
    /// # Arguments
    /// * `out` - destination for raw interleaved samples.
    ///
    /// # Errors
    /// Win32 errors (e.g. `AUDCLNT_E_DEVICE_INVALIDATED` after unplugging).
    fn drain(&self, out: &mut Vec<u8>) -> Result<()> {
        // SAFETY: GetBuffer/ReleaseBuffer pairs on a started capture client.
        unsafe {
            loop {
                let next = self.capture.GetNextPacketSize().map_err(|e| win(e, "GetNextPacketSize"))?;
                if next == 0 {
                    return Ok(());
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None).map_err(|e| win(e, "GetBuffer"))?;
                let bytes = frames as usize * self.format.block_align;
                if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                    out.resize(out.len() + bytes, 0);
                } else {
                    out.extend_from_slice(std::slice::from_raw_parts(data, bytes));
                }
                self.capture.ReleaseBuffer(frames).map_err(|e| win(e, "ReleaseBuffer"))?;
            }
        }
    }
}

impl Drop for Loopback {
    /// Stops the stream and restores the endpoint mute state.
    fn drop(&mut self) {
        // SAFETY: valid COM objects owned by this struct.
        unsafe {
            let _ = self.client.Stop();
            if let Some((vol, was)) = self.restore_mute.take() {
                let _ = vol.SetMute(BOOL::from(was), std::ptr::null());
            }
        }
    }
}

/// How PC audio is routed while it is streamed (C#: `LoopbackCapture._0003()` and
/// `IAudioService.SetVirtualAudioDeviceRender`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderRoute {
    /// Record the default device; the PC keeps playing ("VR headset & computer").
    Shared,
    /// Mute the default device while recording it ("VR headset only" without the virtual driver).
    MuteDefault,
    /// Make **Speakers (Virtual Desktop Audio)** the default device and record it ("VR headset
    /// only" with "Use virtual audio driver"); falls back to [`RenderRoute::MuteDefault`].
    VirtualDefault,
    /// Record **Speakers (Virtual Desktop Audio)** without changing the default ("VoiceMeeter
    /// mode": VoiceMeeter routes audio into that device itself).
    VirtualOnly,
}

/// Chooses the endpoint to record for a route, switching the default device if required.
///
/// # Arguments
/// * `enumerator` - device enumerator.
/// * `route` - requested route.
///
/// # Returns
/// `(endpoint id or None for the default device, default-device switch to keep alive, mute)`.
fn resolve_route(enumerator: &IMMDeviceEnumerator, route: RenderRoute) -> (Option<String>, Option<DefaultSwitch>, bool) {
    match route {
        RenderRoute::Shared => (None, None, false),
        RenderRoute::MuteDefault => (None, None, true),
        RenderRoute::VirtualDefault | RenderRoute::VirtualOnly => {
            let Some((id, hidden)) = find_virtual_endpoint(eRender) else {
                warn!("Speakers (Virtual Desktop Audio) not found; is the Virtual Desktop audio driver installed?");
                return (None, None, route == RenderRoute::VirtualDefault);
            };
            if route == RenderRoute::VirtualOnly {
                return (Some(id), None, false);
            }
            match DefaultSwitch::activate(enumerator, eRender, &id, hidden) {
                Some(switch) => (Some(id), Some(switch), false),
                None => {
                    warn!("cannot make Virtual Desktop Audio the default playback device; muting the PC speakers instead");
                    (None, None, true)
                }
            }
        }
    }
}

/// Body of the "Audio Loopback Thread".
///
/// # Arguments
/// * `route` - audio routing for this session.
/// * `tx` - packet sink.
/// * `stop` - stop flag.
fn loopback_thread(route: RenderRoute, tx: mpsc::Sender<Vec<u8>>, stop: Arc<AtomicBool>) {
    let _com = ComGuard::new();
    // SAFETY: creating the MMDevice enumerator on an initialised COM thread.
    let enumerator: IMMDeviceEnumerator = match unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) } {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "cannot create MMDeviceEnumerator");
            return;
        }
    };
    // Dropped (restoring the previous default device) when the thread ends, before COM is released.
    let (target, _switch, mute) = resolve_route(&enumerator, route);
    let mut warned = false;
    while !stop.load(Ordering::Relaxed) {
        let stream = match Loopback::open(&enumerator, target.as_deref(), mute) {
            Ok(s) => s,
            Err(e) => {
                if !warned {
                    warn!(error = %e, "cannot open loopback capture; retrying");
                    warned = true;
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        warned = false;
        let mut resampler = StereoResampler::new(stream.format.rate, stream.format.channels, SPEAKER_GAIN);
        let mut packets = Packetizer::new(SPEAKER_PACKET_BYTES);
        let mut raw = Vec::new();
        let mut samples = Vec::new();
        let mut pcm = Vec::new();
        let mut last_check = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            raw.clear();
            if let Err(e) = stream.drain(&mut raw) {
                debug!(error = %e, "loopback stream invalidated; reopening");
                break;
            }
            if !raw.is_empty() {
                samples.clear();
                decode_to_f32(&raw, stream.format.sample, &mut samples);
                pcm.clear();
                resampler.process(&samples, &mut pcm);
                for p in packets.push(&pcm) {
                    if tx.blocking_send(p).is_err() {
                        return;
                    }
                }
            }
            // Follow default-device changes (C#: DefaultDeviceChanged → re-initialise).
            if target.is_none() && last_check.elapsed() > Duration::from_secs(2) {
                last_check = Instant::now();
                // SAFETY: enumerator is valid on this thread.
                let current = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }.map(|d| device_id(&d)).unwrap_or_default();
                if !current.is_empty() && current != stream.device_id {
                    info!("default audio device changed; reopening loopback capture");
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(3));
        }
    }
}

/// PC audio capture handle.
pub struct WasapiCapture {
    rx: mpsc::Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WasapiCapture {
    /// Starts the loopback thread.
    ///
    /// # Arguments
    /// * `route` - how PC audio is routed while streaming.
    ///
    /// # Returns
    /// The running capture.
    ///
    /// # Errors
    /// [`PlatformError::Io`] if the thread cannot be spawned.
    pub fn start(route: RenderRoute) -> Result<Self> {
        let (tx, rx) = mpsc::channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new().name("Audio Loopback Thread".into()).spawn(move || loopback_thread(route, tx, flag))?;
        Ok(Self { rx, stop, thread: Some(thread) })
    }

    /// Waits for the next 1152-byte packet.
    ///
    /// # Returns
    /// The packet, or `None` when the thread ended.
    pub async fn next(&mut self) -> Option<Vec<u8>> {
        self.rx.recv().await
    }

    /// Stops capturing and restores the speaker mute state / default device.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.rx.close();
        while self.rx.try_recv().is_ok() {}
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for WasapiCapture {
    /// See [`WasapiCapture::stop`].
    fn drop(&mut self) {
        self.stop();
    }
}

/// `KSPROPERTY` (`KSIDENTIFIER`) as laid out in memory.
#[repr(C)]
struct KsProperty {
    set: GUID,
    id: u32,
    flags: u32,
}

/// Finds the VDVAD KS filter and binds `IKsControl`.
///
/// # Returns
/// The control interface, or `None` if the driver is not installed.
fn find_vdvad() -> Option<IKsControl> {
    // SAFETY: DirectShow device enumeration on an initialised COM thread.
    unsafe {
        let devenum: ICreateDevEnum = CoCreateInstance(&CLSID_SYSTEM_DEVICE_ENUM, None, CLSCTX_INPROC_SERVER).ok()?;
        let mut em: Option<IEnumMoniker> = None;
        devenum.CreateClassEnumerator(&AM_KSCATEGORY_CAPTURE, &mut em, 0).ok()?;
        let em = em?;
        loop {
            let mut monikers: [Option<IMoniker>; 1] = [None];
            let mut fetched = 0u32;
            if em.Next(&mut monikers, Some(&mut fetched)).is_err() || fetched == 0 {
                return None;
            }
            let Some(moniker) = monikers[0].take() else { return None };
            let name = moniker
                .BindToStorage::<_, _, IPropertyBag>(None::<&IBindCtx>, None::<&IMoniker>)
                .ok()
                .and_then(|bag| {
                    let mut var = VARIANT::default();
                    bag.Read(w!("FriendlyName"), &mut var, None).ok()?;
                    BSTR::try_from(&var).ok().map(|b| b.to_string())
                })
                .unwrap_or_default();
            debug!(name, "KS capture filter");
            if name == VDVAD_FILTER_NAME {
                return moniker.BindToObject::<_, _, IKsControl>(None::<&IBindCtx>, None::<&IMoniker>).ok();
            }
        }
    }
}

/// Makes **Microphone (Virtual Desktop Audio)** the default recording device.
/// C#: `IAudioService.SetVirtualAudioDeviceCapture` ("Auto-select microphone").
///
/// # Returns
/// The switch (restored on drop), or `None` when unavailable (logged).
fn select_virtual_microphone() -> Option<DefaultSwitch> {
    // SAFETY: creating the MMDevice enumerator on an initialised COM thread.
    let enumerator: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.ok()?;
    let Some((id, hidden)) = find_virtual_endpoint(eCapture) else {
        warn!("Microphone (Virtual Desktop Audio) not found; not changing the default microphone");
        return None;
    };
    let switch = DefaultSwitch::activate(&enumerator, eCapture, &id, hidden);
    if switch.is_none() {
        warn!("cannot make Virtual Desktop Audio the default microphone");
    }
    switch
}

/// Body of the microphone thread.
///
/// # Arguments
/// * `rx` - PCM chunks (gain already applied).
/// * `stop` - stop flag.
/// * `ready` - set once the driver was found.
/// * `select_default` - make the virtual microphone the default recording device.
fn mic_thread(rx: std_mpsc::Receiver<Vec<u8>>, stop: Arc<AtomicBool>, ready: Arc<AtomicBool>, select_default: bool) {
    let _com = ComGuard::new();
    // Restored when the thread ends, before COM is released.
    let _switch = if select_default { select_virtual_microphone() } else { None };
    // The original retries 10 × 500 ms while the driver initialises.
    let mut ks = None;
    for _ in 0..10 {
        ks = find_vdvad();
        if ks.is_some() || stop.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let Some(ks) = ks else {
        warn!("\"{VDVAD_FILTER_NAME}\" not found — is the Virtual Desktop audio driver installed? Microphone passthrough disabled");
        return;
    };
    ready.store(true, Ordering::Relaxed);
    info!("virtual microphone (VDVAD) ready");
    let prop = KsProperty { set: VDVAD_MIC_PROPERTY_SET, id: 0, flags: KSPROPERTY_TYPE_SET };
    let mut backlog: Vec<u8> = Vec::with_capacity(MIC_MAX_BACKLOG * 2);
    let mut errors = 0u32;
    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(pcm) => backlog.extend_from_slice(&pcm),
            Err(std_mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
        }
        while let Ok(more) = rx.try_recv() {
            backlog.extend_from_slice(&more);
        }
        if backlog.len() > MIC_MAX_BACKLOG {
            let keep = MIC_CHUNK;
            backlog.drain(..backlog.len() - keep);
        }
        while backlog.len() >= MIC_CHUNK {
            let mut returned = 0u32;
            // SAFETY: `prop` is a valid KSPROPERTY and the data pointer covers MIC_CHUNK bytes.
            let res = unsafe {
                ks.KsProperty(
                    &prop as *const KsProperty as *const KSIDENTIFIER,
                    std::mem::size_of::<KsProperty>() as u32,
                    backlog.as_mut_ptr() as *mut c_void,
                    MIC_CHUNK as u32,
                    &mut returned,
                )
            };
            // The driver rejects samples (ERROR_INVALID_FUNCTION) while no application is
            // recording from "Microphone (Virtual Desktop Audio)"; the original ignores the
            // result as well, so this is only logged once at debug level.
            if let Err(e) = res {
                errors += 1;
                if errors == 1 {
                    debug!(error = %e, "VDVAD rejected microphone samples (no active recorder?)");
                }
            }
            backlog.drain(..MIC_CHUNK);
        }
    }
}

/// Virtual microphone handle.
pub struct KsMicrophone {
    tx: Option<std_mpsc::Sender<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl KsMicrophone {
    /// Starts the microphone thread (driver discovery happens on that thread).
    ///
    /// # Arguments
    /// * `select_default` - make the virtual microphone the default recording device while
    ///   running ("Auto-select microphone").
    ///
    /// # Returns
    /// The handle.
    ///
    /// # Errors
    /// [`PlatformError::Io`] if the thread cannot be spawned.
    pub fn start(select_default: bool) -> Result<Self> {
        let (tx, rx) = std_mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(AtomicBool::new(false));
        let (s, r) = (Arc::clone(&stop), Arc::clone(&ready));
        let thread = std::thread::Builder::new().name("Microphone Thread".into()).spawn(move || mic_thread(rx, s, r, select_default))?;
        Ok(Self { tx: Some(tx), stop, ready, thread: Some(thread) })
    }

    /// Whether the driver was found.
    ///
    /// # Returns
    /// `true` once samples are being delivered to the driver.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    /// Queues PCM for the virtual microphone.
    ///
    /// # Arguments
    /// * `pcm` - 48 kHz mono `s16le` with gain applied.
    pub fn write(&self, pcm: Vec<u8>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(pcm);
        }
    }

    /// Stops the thread.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for KsMicrophone {
    /// See [`KsMicrophone::stop`].
    fn drop(&mut self) {
        self.stop();
    }
}

/// Restores default devices left switched by a crashed session (see [`super::endpoint::recover`]).
/// Runs on a temporary COM thread; call before starting new audio threads.
pub fn recover_defaults() {
    let _ = std::thread::Builder::new()
        .name("Audio Recovery".into())
        .spawn(|| {
            let _com = ComGuard::new();
            super::endpoint::recover();
        })
        .map(|t| t.join());
}
