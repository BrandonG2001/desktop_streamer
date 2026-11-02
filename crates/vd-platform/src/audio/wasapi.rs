//! Windows audio back-end.
//!
//! * **PC → headset** — C#: `LoopbackCapture` (CSCore `WasapiLoopbackCapture`).  WASAPI loopback
//!   of the default render endpoint (`eRender`/`eConsole`) in the endpoint's mix format, converted
//!   to 48 kHz stereo `s16le` with gain 0.9 and cut into 1152-byte packets.  See
//!   [`RenderRoute`] for how "VR headset only" keeps the PC silent: with the Virtual Desktop
//!   driver installed the hidden **Speakers (Virtual Desktop Audio)** endpoint becomes the default
//!   device and is recorded ([`super::endpoint`]); without it (or with "Use virtual audio driver"
//!   off) every application is recorded with *process loopback* and all playback devices are
//!   silenced ([`super::silence`]).  When recording the default device the stream is re-opened
//!   when it changes (`MMDeviceEnumerator.DefaultDeviceChanged`).
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
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eCapture, eConsole, eRender,
};
use windows::Win32::Media::DirectShow::ICreateDevEnum;
use windows::Win32::Media::KernelStreaming::{IKsControl, KSIDENTIFIER};
use windows::Win32::System::Com::StructuredStorage::IPropertyBag;
use windows::Win32::System::Com::{CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IBindCtx, IEnumMoniker, IMoniker};
use windows::core::{BSTR, GUID, HSTRING, Interface, VARIANT, implement, w};

use super::convert::{Packetizer, SampleFormat, StereoResampler, decode_to_f32};
use super::endpoint::{DefaultSwitch, find_virtual_endpoint};
use super::silence::SpeakerSilencer;
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

/// What a loopback stream records.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// The default render endpoint (followed when it changes).
    DefaultDevice,
    /// A specific render endpoint.
    Device(String),
    /// Every process except this one, whatever device they play on (Windows 10 build 20348+).
    AllProcesses,
}

/// Fixed format requested from process loopback, which has no mix format of its own.
const PROCESS_LOOPBACK_FORMAT: MixFormat = MixFormat { rate: 48_000, channels: 2, block_align: 4, sample: SampleFormat::I16 };

/// Completion callback of `ActivateAudioInterfaceAsync`.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationDone(std_mpsc::Sender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationDone_Impl {
    /// Signals the waiting thread.
    ///
    /// # Arguments
    /// * `_op` - the finished operation (read by the waiter).
    ///
    /// # Errors
    /// Never.
    fn ActivateCompleted(&self, _op: Option<&IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        let _ = self.0.send(());
        Ok(())
    }
}

/// `PROPVARIANT` holding a `VT_BLOB` (the layout `ActivateAudioInterfaceAsync` reads).
#[repr(C)]
struct BlobVariant {
    vt: u16,
    reserved: [u16; 3],
    size: u32,
    data: *const c_void,
}

/// `VT_BLOB`.
const VT_BLOB: u16 = 65;

/// Activates an audio client that records every process except this one.
/// C++ equivalent: Microsoft's *ApplicationLoopback* sample with
/// `PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE`.
///
/// # Returns
/// The (uninitialised) audio client.
///
/// # Errors
/// [`PlatformError::Win32`] when process loopback is unsupported (Windows older than build
/// 20348) or activation fails.
fn activate_process_loopback() -> Result<IAudioClient> {
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: std::process::id(),
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    let blob = BlobVariant { vt: VT_BLOB, reserved: [0; 3], size: std::mem::size_of_val(&params) as u32, data: (&params as *const AUDIOCLIENT_ACTIVATION_PARAMS).cast() };
    let (tx, rx) = std_mpsc::channel();
    let handler: IActivateAudioInterfaceCompletionHandler = ActivationDone(tx).into();
    // SAFETY: `blob` points at `params`, both outlive the wait below; the handler is a COM object.
    unsafe {
        let op = ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some((&blob as *const BlobVariant).cast()), &handler)
            .map_err(|e| win(e, "ActivateAudioInterfaceAsync"))?;
        rx.recv_timeout(Duration::from_secs(5)).map_err(|_| PlatformError::Unsupported("process loopback activation timed out"))?;
        let mut hr = windows::core::HRESULT(0);
        let mut unknown: Option<windows::core::IUnknown> = None;
        op.GetActivateResult(&mut hr, &mut unknown).map_err(|e| win(e, "GetActivateResult"))?;
        hr.ok().map_err(|e| win(e, "process loopback activation"))?;
        unknown.ok_or(PlatformError::Unsupported("process loopback"))?.cast::<IAudioClient>().map_err(|e| win(e, "IAudioClient"))
    }
}

/// An open loopback stream.
struct Loopback {
    /// Endpoint id (empty for process loopback).
    device_id: String,
    client: IAudioClient,
    capture: IAudioCaptureClient,
    format: MixFormat,
}

impl Loopback {
    /// Opens loopback capture.
    ///
    /// # Arguments
    /// * `enumerator` - device enumerator.
    /// * `source` - what to record.
    ///
    /// # Returns
    /// The started stream.
    ///
    /// # Errors
    /// Win32 errors from WASAPI.
    fn open(enumerator: &IMMDeviceEnumerator, source: &Source) -> Result<Self> {
        // SAFETY: standard WASAPI call sequence on an MTA thread; pointers come from the API.
        unsafe {
            if *source == Source::AllProcesses {
                let client = activate_process_loopback()?;
                let f = PROCESS_LOOPBACK_FORMAT;
                let wfx = WAVEFORMATEX {
                    wFormatTag: 1, // WAVE_FORMAT_PCM
                    nChannels: f.channels as u16,
                    nSamplesPerSec: f.rate,
                    nAvgBytesPerSec: f.rate * f.block_align as u32,
                    nBlockAlign: f.block_align as u16,
                    wBitsPerSample: 16,
                    cbSize: 0,
                };
                // Process loopback is event driven; the event is signalled but the thread polls.
                let event = windows_sys::Win32::System::Threading::CreateEventW(std::ptr::null(), 0, 0, std::ptr::null());
                client
                    .Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, 1_000_000, 0, &wfx, None)
                    .map_err(|e| win(e, "IAudioClient::Initialize(process loopback)"))?;
                client.SetEventHandle(HANDLE(event as _)).map_err(|e| win(e, "SetEventHandle"))?;
                let capture: IAudioCaptureClient = client.GetService().map_err(|e| win(e, "GetService(IAudioCaptureClient)"))?;
                client.Start().map_err(|e| win(e, "IAudioClient::Start"))?;
                info!("process loopback capture started (all applications except the streamer)");
                return Ok(Self { device_id: String::new(), client, capture, format: f });
            }
            let device = match source {
                Source::Device(id) => enumerator.GetDevice(&HSTRING::from(id.as_str())).map_err(|e| win(e, "IMMDeviceEnumerator::GetDevice"))?,
                _ => enumerator.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|e| win(e, "GetDefaultAudioEndpoint"))?,
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
            client.Start().map_err(|e| win(e, "IAudioClient::Start"))?;
            info!(device = %id, rate = format.rate, channels = format.channels, sample = ?format.sample, "WASAPI loopback capture started");
            Ok(Self { device_id: id, client, capture, format })
        }
    }

    /// Drains every available packet into `out` (raw bytes in the stream format).
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
    /// Stops the stream.
    fn drop(&mut self) {
        // SAFETY: valid COM object owned by this struct.
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

/// How PC audio is routed while it is streamed (C#: `LoopbackCapture._0003()` and
/// `IAudioService.SetVirtualAudioDeviceRender`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderRoute {
    /// Record the default device; the PC keeps playing ("VR headset & computer").
    Shared,
    /// Driver-free "VR headset only": record every application with process loopback and
    /// silence all playback devices ([`SpeakerSilencer`]).  Falls back to recording the default
    /// device on Windows versions without process loopback.
    Silenced,
    /// Make **Speakers (Virtual Desktop Audio)** the default device and record it ("VR headset
    /// only" with "Use virtual audio driver"); falls back to [`RenderRoute::Silenced`] when the
    /// driver is not installed.
    VirtualDefault,
    /// Record **Speakers (Virtual Desktop Audio)** without changing the default ("VoiceMeeter
    /// mode": VoiceMeeter routes audio into that device itself).
    VirtualOnly,
}

/// Device changes made for a route; restored when dropped.
#[derive(Default)]
struct RouteGuards {
    /// Virtual speakers made the default device.
    _switch: Option<DefaultSwitch>,
    /// All playback devices silenced.
    silencer: Option<SpeakerSilencer>,
}

/// Chooses what to record for a route and makes the device changes it needs.
///
/// # Arguments
/// * `enumerator` - device enumerator.
/// * `route` - requested route.
/// * `keep` - playback endpoints that must not be silenced (microphone cables).
///
/// # Returns
/// The source to record and the guards to keep alive while recording.
fn resolve_route(enumerator: &IMMDeviceEnumerator, route: RenderRoute, keep: Vec<String>) -> (Source, RouteGuards) {
    let silenced = || (Source::AllProcesses, RouteGuards { _switch: None, silencer: Some(SpeakerSilencer::engage(enumerator, keep.clone())) });
    match route {
        RenderRoute::Shared => (Source::DefaultDevice, RouteGuards::default()),
        RenderRoute::Silenced => silenced(),
        RenderRoute::VirtualDefault | RenderRoute::VirtualOnly => {
            let Some((id, hidden)) = find_virtual_endpoint(eRender) else {
                info!("Virtual Desktop audio driver not installed; using driver-free routing");
                return if route == RenderRoute::VirtualDefault { silenced() } else { (Source::DefaultDevice, RouteGuards::default()) };
            };
            if route == RenderRoute::VirtualOnly {
                return (Source::Device(id), RouteGuards::default());
            }
            match DefaultSwitch::activate(enumerator, eRender, &id, hidden) {
                Some(switch) => (Source::Device(id), RouteGuards { _switch: Some(switch), silencer: None }),
                None => {
                    warn!("cannot make Virtual Desktop Audio the default playback device; using driver-free routing");
                    silenced()
                }
            }
        }
    }
}

/// Body of the "Audio Loopback Thread".
///
/// # Arguments
/// * `route` - audio routing for this session.
/// * `keep` - playback endpoints that must not be silenced (microphone cables).
/// * `tx` - packet sink.
/// * `stop` - stop flag.
fn loopback_thread(route: RenderRoute, keep: Vec<String>, tx: mpsc::Sender<Vec<u8>>, stop: Arc<AtomicBool>) {
    let _com = ComGuard::new();
    // SAFETY: creating the MMDevice enumerator on an initialised COM thread.
    let enumerator: IMMDeviceEnumerator = match unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) } {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "cannot create MMDeviceEnumerator");
            return;
        }
    };
    // Dropped (restoring devices) when the thread ends, before COM is released.
    let (mut source, mut guards) = resolve_route(&enumerator, route, keep);
    let mut warned = false;
    while !stop.load(Ordering::Relaxed) {
        let stream = match Loopback::open(&enumerator, &source) {
            Ok(s) => s,
            Err(e) if source == Source::AllProcesses => {
                // Windows 10 before build 20348: record the default device instead.
                warn!(error = %e, "process loopback unavailable; recording the default playback device");
                source = Source::DefaultDevice;
                continue;
            }
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
            if last_check.elapsed() > Duration::from_secs(2) {
                last_check = Instant::now();
                // Silence playback devices connected during the session.
                if let Some(s) = guards.silencer.as_mut() {
                    s.refresh();
                }
                // Follow default-device changes (C#: DefaultDeviceChanged → re-initialise).
                if source == Source::DefaultDevice {
                    // SAFETY: enumerator is valid on this thread.
                    let current = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }.map(|d| device_id(&d)).unwrap_or_default();
                    if !current.is_empty() && current != stream.device_id {
                        info!("default audio device changed; reopening loopback capture");
                        break;
                    }
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
    /// * `keep` - playback endpoints that must not be silenced (microphone cables).
    ///
    /// # Returns
    /// The running capture.
    ///
    /// # Errors
    /// [`PlatformError::Io`] if the thread cannot be spawned.
    pub fn start(route: RenderRoute, keep: Vec<String>) -> Result<Self> {
        let (tx, rx) = mpsc::channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new().name("Audio Loopback Thread".into()).spawn(move || loopback_thread(route, keep, tx, flag))?;
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
            let moniker = monikers[0].take()?;
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
            // SAFETY: creating the MMDevice enumerator on an initialised COM thread.
            if let Ok(enumerator) = unsafe { CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) } {
                super::silence::recover(&enumerator);
            }
        })
        .map(|t| t.join());
}
