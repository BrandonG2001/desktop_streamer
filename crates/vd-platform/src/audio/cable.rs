//! Driver-free microphone passthrough (Windows): play the headset microphone into a virtual
//! audio cable.
//!
//! Windows has no user-mode API to create a recording device, so without the Virtual Desktop
//! driver the port relies on a *virtual audio cable* the user installs (VB-Audio Virtual Cable,
//! Hi-Fi Cable, VoiceMeeter …).  Such a driver pairs a playback endpoint ("CABLE Input") with a
//! recording endpoint ("CABLE Output"): whatever is played into the first can be recorded from
//! the second.  [`RenderMicrophone`] plays the 48 kHz mono microphone stream into the playback
//! side with ordinary WASAPI; applications select the recording side as their microphone, and
//! "Auto-select microphone" makes it the default recording device.
//!
//! [`resolve`] picks the target from the `Port.MicrophoneDevice` setting: `auto` prefers the
//! Virtual Desktop driver, then the first known cable; `vdvad` forces the driver; any other value
//! is the endpoint id of a playback device chosen in the settings window.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use tracing::{debug, info, warn};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, WAVEFORMATEX, eCapture, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::core::HSTRING;

use super::endpoint::{DefaultSwitch, EndpointInfo, VDVAD_INTERFACE_NAME, list_endpoints};
use super::{MIC_AUTO, MIC_VDVAD};
use crate::error::Result;

/// Playback/recording device names of known virtual cables, in order of preference.
pub const KNOWN_CABLES: &[(&str, &str)] = &[
    ("CABLE Input", "CABLE Output"),                         // VB-Audio Virtual Cable
    ("Hi-Fi Cable Input", "Hi-Fi Cable Output"),             // VB-Audio Hi-Fi Cable
    ("CABLE-A Input", "CABLE-A Output"),                     // VB-Audio Cable A+B
    ("CABLE-B Input", "CABLE-B Output"),
    ("VoiceMeeter Aux Input", "VoiceMeeter Aux Output"),     // VoiceMeeter Banana/Potato
    ("VoiceMeeter VAIO3 Input", "VoiceMeeter VAIO3 Output"), // VoiceMeeter Potato
    ("VoiceMeeter Input", "VoiceMeeter Output"),             // VoiceMeeter
];

/// Microphone chunk size (20 ms of 48 kHz mono s16), as for the Virtual Desktop driver.
const MIC_CHUNK: usize = 1920;
/// Backlog above which old microphone audio is dropped (80 ms), as for the driver.
const MIC_MAX_BACKLOG: usize = 7680;

/// Where microphone audio goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicTarget {
    /// The Virtual Desktop driver (`IKsControl` property, see `wasapi`).
    Vdvad,
    /// A playback endpoint whose audio reappears on a recording endpoint.
    Render {
        /// Playback endpoint id.
        id: String,
        /// Name shown to the user.
        name: String,
        /// Paired recording endpoint id, if known (made default by "Auto-select microphone").
        pair: Option<String>,
    },
}

/// The recording endpoint paired with a cable's playback endpoint.
///
/// # Arguments
/// * `input` - the playback endpoint.
/// * `captures` - recording endpoints.
///
/// # Returns
/// The pair's endpoint id, when `input` is a known cable.
fn paired_capture(input: &EndpointInfo, captures: &[EndpointInfo]) -> Option<String> {
    let (_, output) = KNOWN_CABLES.iter().find(|(i, _)| input.name.eq_ignore_ascii_case(i))?;
    captures.iter().find(|c| c.name.eq_ignore_ascii_case(output) && c.interface == input.interface).map(|c| c.id.clone())
}

/// Builds a [`MicTarget::Render`] for an endpoint.
///
/// # Arguments
/// * `e` - playback endpoint.
/// * `captures` - recording endpoints (for the pair).
///
/// # Returns
/// The target.
fn render_target(e: &EndpointInfo, captures: &[EndpointInfo]) -> MicTarget {
    MicTarget::Render { id: e.id.clone(), name: e.friendly_name(), pair: paired_capture(e, captures) }
}

/// Chooses the microphone target for a `Port.MicrophoneDevice` setting.
///
/// # Arguments
/// * `pref` - [`MIC_AUTO`], [`MIC_VDVAD`] or a playback endpoint id.
///
/// # Returns
/// The target, or `None` when nothing suitable is installed.
pub fn resolve(pref: &str) -> Option<MicTarget> {
    let renders = list_endpoints(eRender);
    let captures = list_endpoints(eCapture);
    let vdvad = captures.iter().any(|c| c.interface == VDVAD_INTERFACE_NAME);
    match pref {
        MIC_VDVAD => vdvad.then_some(MicTarget::Vdvad),
        MIC_AUTO | "" => {
            if vdvad {
                return Some(MicTarget::Vdvad);
            }
            KNOWN_CABLES.iter().find_map(|(input, _)| renders.iter().find(|e| e.name.eq_ignore_ascii_case(input))).map(|e| render_target(e, &captures))
        }
        id => renders.iter().find(|e| e.id == id).map(|e| render_target(e, &captures)),
    }
}

/// Playback endpoints that must stay audible while "VR headset only" silences the speakers:
/// every known cable input, plus the device chosen as microphone target.
///
/// # Arguments
/// * `pref` - `Port.MicrophoneDevice`.
///
/// # Returns
/// Endpoint ids.
pub fn sink_ids(pref: &str) -> Vec<String> {
    list_endpoints(eRender)
        .into_iter()
        .filter(|e| e.id == pref || KNOWN_CABLES.iter().any(|(input, _)| e.name.eq_ignore_ascii_case(input)))
        .map(|e| e.id)
        .collect()
}

/// Playback devices the settings window offers as microphone target (known cables first).
///
/// # Returns
/// `(endpoint id, friendly name)` pairs, excluding the Virtual Desktop driver's own speakers.
pub fn targets() -> Vec<(String, String)> {
    let mut list: Vec<EndpointInfo> = list_endpoints(eRender).into_iter().filter(|e| e.interface != VDVAD_INTERFACE_NAME).collect();
    let rank = |e: &EndpointInfo| KNOWN_CABLES.iter().position(|(i, _)| e.name.eq_ignore_ascii_case(i)).unwrap_or(usize::MAX);
    list.sort_by_key(rank);
    list.into_iter().map(|e| (e.id.clone(), e.friendly_name())).collect()
}

/// Opens a shared-mode stream that accepts 48 kHz mono `s16` (converted by the audio engine).
///
/// # Arguments
/// * `enumerator` - device enumerator.
/// * `id` - playback endpoint id.
///
/// # Returns
/// `(client, render client, buffer size in frames)`.
///
/// # Errors
/// Win32 errors from WASAPI.
fn open_render(enumerator: &IMMDeviceEnumerator, id: &str) -> windows::core::Result<(IAudioClient, IAudioRenderClient, u32)> {
    // SAFETY: standard WASAPI call sequence on an MTA thread.
    unsafe {
        let device = enumerator.GetDevice(&HSTRING::from(id))?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let wfx = WAVEFORMATEX { wFormatTag: 1, nChannels: 1, nSamplesPerSec: 48_000, nAvgBytesPerSec: 96_000, nBlockAlign: 2, wBitsPerSample: 16, cbSize: 0 };
        // 100 ms buffer; the writer keeps it filled with what has arrived from the headset.
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, 1_000_000, 0, &wfx, None)?;
        let render: IAudioRenderClient = client.GetService()?;
        let frames = client.GetBufferSize()?;
        client.Start()?;
        Ok((client, render, frames))
    }
}

/// Body of the cable microphone thread.
///
/// # Arguments
/// * `id` - playback endpoint to play into.
/// * `pair` - recording endpoint to make the default, when `select_default`.
/// * `select_default` - "Auto-select microphone".
/// * `rx` - PCM chunks (gain already applied).
/// * `stop` - stop flag.
/// * `ready` - set once the stream is open.
fn render_thread(id: String, pair: Option<String>, select_default: bool, rx: std_mpsc::Receiver<Vec<u8>>, stop: Arc<AtomicBool>, ready: Arc<AtomicBool>) {
    // SAFETY: COM initialisation for this thread, balanced at the end.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    {
        // SAFETY: creating the enumerator on an initialised COM thread.
        let enumerator: Option<IMMDeviceEnumerator> = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.ok();
        let Some(enumerator) = enumerator else {
            warn!("cannot create MMDeviceEnumerator for the microphone");
            return;
        };
        let (client, render, frames) = match open_render(&enumerator, &id) {
            Ok(s) => s,
            Err(e) => {
                warn!(device = %id, error = %e, "cannot open the virtual cable for the microphone");
                return;
            }
        };
        // Restored when this block ends, before COM is released.
        let _switch = match (&pair, select_default) {
            (Some(p), true) => DefaultSwitch::activate(&enumerator, eCapture, p, false),
            _ => None,
        };
        ready.store(true, Ordering::Relaxed);
        let mut backlog: VecDeque<u8> = VecDeque::with_capacity(MIC_MAX_BACKLOG * 2);
        while !stop.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_millis(5)) {
                Ok(pcm) => backlog.extend(pcm),
                Err(std_mpsc::RecvTimeoutError::Timeout) => {}
                Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(more) = rx.try_recv() {
                backlog.extend(more);
            }
            // Bound latency like the driver path: keep only the newest 20 ms when far behind.
            if backlog.len() > MIC_MAX_BACKLOG {
                let drop = backlog.len() - MIC_CHUNK;
                backlog.drain(..drop);
            }
            // SAFETY: GetBuffer/ReleaseBuffer pair on a started render client; we write at most
            // the requested number of frames (2 bytes each).
            unsafe {
                let Ok(padding) = client.GetCurrentPadding() else { break };
                let n = (frames.saturating_sub(padding) as usize).min(backlog.len() / 2);
                if n == 0 {
                    continue;
                }
                let Ok(buf) = render.GetBuffer(n as u32) else { continue };
                let dst = std::slice::from_raw_parts_mut(buf, n * 2);
                for (d, s) in dst.iter_mut().zip(backlog.drain(..n * 2)) {
                    *d = s;
                }
                let _ = render.ReleaseBuffer(n as u32, 0);
            }
        }
        // SAFETY: stopping the stream we started.
        unsafe {
            let _ = client.Stop();
        }
        debug!("virtual cable microphone stopped");
    }
    if com {
        // SAFETY: balances the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
}

/// Microphone that plays into a virtual cable.
pub struct RenderMicrophone {
    tx: Option<std_mpsc::Sender<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RenderMicrophone {
    /// Starts playing into `id`.
    ///
    /// # Arguments
    /// * `id` - playback endpoint id.
    /// * `name` - name for the log.
    /// * `pair` - recording endpoint paired with it.
    /// * `select_default` - make `pair` the default recording device while running.
    ///
    /// # Returns
    /// The handle.
    ///
    /// # Errors
    /// [`crate::PlatformError::Io`] if the thread cannot be spawned.
    pub fn start(id: String, name: &str, pair: Option<String>, select_default: bool) -> Result<Self> {
        let (tx, rx) = std_mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(AtomicBool::new(false));
        let (s, r) = (Arc::clone(&stop), Arc::clone(&ready));
        let thread = std::thread::Builder::new().name("Microphone Thread".into()).spawn(move || render_thread(id, pair, select_default, rx, s, r))?;
        info!(device = name, "microphone passthrough via virtual cable");
        Ok(Self { tx: Some(tx), stop, ready, thread: Some(thread) })
    }

    /// Whether the cable stream is open.
    ///
    /// # Returns
    /// `true` once audio is being delivered.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    /// Queues PCM for the cable.
    ///
    /// # Arguments
    /// * `pcm` - 48 kHz mono `s16le` with gain applied.
    pub fn write(&self, pcm: Vec<u8>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(pcm);
        }
    }

    /// Stops the thread (restoring the default recording device).
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for RenderMicrophone {
    /// See [`RenderMicrophone::stop`].
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(id: &str, name: &str, iface: &str) -> EndpointInfo {
        EndpointInfo { id: id.into(), name: name.into(), interface: iface.into(), hidden: false }
    }

    #[test]
    fn pairs_cable_input_with_output_of_same_driver() {
        let input = ep("r1", "CABLE Input", "VB-Audio Virtual Cable");
        let captures = [ep("c0", "CABLE Output", "Other Driver"), ep("c1", "CABLE Output", "VB-Audio Virtual Cable"), ep("c2", "Microphone", "Realtek")];
        assert_eq!(paired_capture(&input, &captures).as_deref(), Some("c1"));
        assert_eq!(paired_capture(&ep("r2", "Speakers", "Realtek"), &captures), None);
    }
}
