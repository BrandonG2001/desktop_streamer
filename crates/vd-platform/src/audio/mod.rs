//! Audio streaming services.
//!
//! [`AudioSession`] is the platform-neutral entry point used by the streamer: it captures PC
//! audio as 48 kHz stereo `s16le` packets (C#: `LoopbackCapture`) and feeds headset microphone
//! audio into a virtual capture device (C#: `Microphone`).
//!
//! | platform | PC → headset                              | headset → PC microphone                         |
//! |----------|-------------------------------------------|-------------------------------------------------|
//! | Windows  | [`wasapi`]: WASAPI loopback; for headset-only either the Virtual Desktop speakers become the default device ([`endpoint`]) or, driver-free, process loopback with all speakers silenced ([`silence`]) | [`wasapi`]: `IKsControl` on the installed `vdvad.sys` driver, or playback into a virtual cable ([`cable`]) |
//! | Linux    | [`pulse`]: `parec` on a null sink monitor | [`pulse`]: `pacat` into a null sink + remap source |

#[cfg(windows)]
pub mod cable;
pub mod convert;
#[cfg(windows)]
pub mod endpoint;
#[cfg(windows)]
pub mod silence;
#[cfg(target_os = "linux")]
pub mod pulse;
#[cfg(windows)]
pub mod wasapi;

pub use convert::apply_gain_s16;

use tracing::warn;

use crate::error::{PlatformError, Result};

/// Undoes audio-device changes left behind by a session that did not end cleanly (Windows:
/// default-device switch and silenced speakers; no-op elsewhere).  Called at start-up and before
/// every session.
pub fn recover() {
    #[cfg(windows)]
    wasapi::recover_defaults();
}

/// `Port.MicrophoneDevice` value: choose the microphone target automatically.
pub const MIC_AUTO: &str = "auto";
/// `Port.MicrophoneDevice` value: always use the Virtual Desktop driver.
pub const MIC_VDVAD: &str = "vdvad";

/// Playback devices that can carry the headset microphone (Windows virtual cables; empty on
/// Linux, where the port creates its own virtual source).
///
/// # Returns
/// `(id, name)` pairs for the settings window, known cables first.
pub fn microphone_targets() -> Vec<(String, String)> {
    #[cfg(windows)]
    return cable::targets();
    #[allow(unreachable_code)]
    Vec::new()
}

/// Whether the Virtual Desktop audio driver's microphone is installed.
///
/// # Returns
/// `true` on Windows with `vdvad.sys`; always `false` elsewhere.
pub fn has_virtual_desktop_driver() -> bool {
    #[cfg(windows)]
    return endpoint::find_virtual_endpoint(windows::Win32::Media::Audio::eCapture).is_some();
    #[allow(unreachable_code)]
    false
}

/// What an [`AudioSession`] should provide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOptions {
    /// Capture PC audio for the headset.
    pub capture: bool,
    /// `AudioStreaming::HeadsetOnly` — silence the PC speakers while streaming.
    pub headset_only: bool,
    /// `StreamerSettings.UseVirtualAudioDriver` — route headset-only audio through the
    /// Virtual Desktop speaker device when that driver is installed (Windows); otherwise the
    /// driver-free route is used.
    pub virtual_speaker: bool,
    /// `StreamerSettings.VoiceMeeterMode` — record the virtual speaker device without making it
    /// the default (Windows).
    pub voicemeeter: bool,
    /// Forward the headset microphone (`SharedUserSettings.MicPassthrough`).
    pub microphone: bool,
    /// `StreamerSettings.AutoSelectMicrophone` — make the virtual microphone the default
    /// recording device while forwarding (Windows).
    pub select_microphone: bool,
    /// `Port.MicrophoneDevice` — [`MIC_AUTO`], [`MIC_VDVAD`] or a playback endpoint id of a
    /// virtual cable (Windows).
    pub microphone_device: String,
    /// Allowed to create/switch audio devices (Linux virtual sinks).
    pub manage_devices: bool,
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct Inner {
    routing: Option<pulse::AudioRouting>,
    capture: Option<pulse::LoopbackCapture>,
    mic: Option<pulse::MicrophonePlayback>,
}

/// Windows microphone back-ends.
#[cfg(windows)]
enum WinMic {
    /// The Virtual Desktop driver.
    Ks(wasapi::KsMicrophone),
    /// A virtual audio cable.
    Cable(cable::RenderMicrophone),
}

#[cfg(windows)]
impl WinMic {
    /// Queues PCM.
    ///
    /// # Arguments
    /// * `pcm` - 48 kHz mono `s16le` with gain applied.
    fn write(&self, pcm: Vec<u8>) {
        match self {
            WinMic::Ks(m) => m.write(pcm),
            WinMic::Cable(m) => m.write(pcm),
        }
    }

    /// Stops the back-end.
    fn stop(&mut self) {
        match self {
            WinMic::Ks(m) => m.stop(),
            WinMic::Cable(m) => m.stop(),
        }
    }
}

#[cfg(windows)]
#[derive(Default)]
struct Inner {
    capture: Option<wasapi::WasapiCapture>,
    mic: Option<WinMic>,
}

#[cfg(not(any(windows, target_os = "linux")))]
#[derive(Default)]
struct Inner;

/// A running audio session.
pub struct AudioSession {
    inner: Inner,
    /// Where microphone audio goes, for display (e.g. "Virtual Desktop driver").
    mic_name: Option<String>,
}

impl AudioSession {
    /// Starts capture and/or the virtual microphone.  Components that fail are logged and left
    /// out, as the original keeps streaming video when audio devices are unavailable.
    ///
    /// # Arguments
    /// * `opts` - requested components.
    ///
    /// # Returns
    /// The session.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when no component could be started at all.
    pub async fn start(opts: AudioOptions) -> Result<Self> {
        let mut inner = Inner::default();
        #[allow(unused_mut)]
        let mut mic_name: Option<String> = None;
        #[cfg(target_os = "linux")]
        {
            if opts.manage_devices {
                match pulse::AudioRouting::setup(opts.capture && opts.headset_only, opts.microphone).await {
                    Ok(r) => inner.routing = Some(r),
                    Err(e) => warn!(error = %e, "audio device routing unavailable"),
                }
            }
            if opts.capture {
                let dev = inner.routing.as_ref().and_then(|r| r.capture_device.clone());
                match pulse::LoopbackCapture::start(dev.as_deref()) {
                    Ok(c) => inner.capture = Some(c),
                    Err(e) => warn!(error = %e, "PC audio capture unavailable"),
                }
            }
            if let Some(sink) = inner.routing.as_ref().and_then(|r| r.mic_sink.clone()) {
                match pulse::MicrophonePlayback::start(&sink) {
                    Ok(m) => inner.mic = Some(m),
                    Err(e) => warn!(error = %e, "virtual microphone unavailable"),
                }
            }
        }
        #[cfg(windows)]
        {
            recover();
            if opts.capture {
                // C#: LoopbackCapture._0003() and IAudioService.SetVirtualAudioDeviceRender.
                let route = match (opts.virtual_speaker, opts.voicemeeter, opts.headset_only) {
                    (true, true, _) => wasapi::RenderRoute::VirtualOnly,
                    (true, false, true) => wasapi::RenderRoute::VirtualDefault,
                    (false, _, true) => wasapi::RenderRoute::Silenced,
                    (_, _, false) => wasapi::RenderRoute::Shared,
                };
                match wasapi::WasapiCapture::start(route, cable::sink_ids(&opts.microphone_device)) {
                    Ok(c) => inner.capture = Some(c),
                    Err(e) => warn!(error = %e, "PC audio capture unavailable"),
                }
            }
            if opts.microphone {
                let started = match cable::resolve(&opts.microphone_device) {
                    Some(cable::MicTarget::Vdvad) => wasapi::KsMicrophone::start(opts.select_microphone).map(|m| (WinMic::Ks(m), "Virtual Desktop driver".to_owned())),
                    Some(cable::MicTarget::Render { id, name, pair }) => {
                        cable::RenderMicrophone::start(id, &name, pair, opts.select_microphone).map(|m| (WinMic::Cable(m), name))
                    }
                    None => {
                        warn!(
                            "no virtual microphone: install VB-Audio Virtual Cable (or the Virtual Desktop driver) to use microphone passthrough"
                        );
                        Err(PlatformError::Unsupported("virtual microphone"))
                    }
                };
                match started {
                    Ok((m, name)) => {
                        inner.mic = Some(m);
                        mic_name = Some(name);
                    }
                    Err(e) => tracing::debug!(error = %e, "virtual microphone unavailable"),
                }
            }
        }
        #[cfg(target_os = "linux")]
        if inner.mic.is_some() {
            mic_name = Some("Virtual Desktop Microphone (PulseAudio)".to_owned());
        }
        let session = Self { inner, mic_name };
        if !session.has_capture() && !session.has_mic() {
            #[cfg(target_os = "linux")]
            if let Some(r) = session.inner.routing {
                r.restore().await;
            }
            let _ = opts;
            return Err(PlatformError::Unsupported("no audio component could be started"));
        }
        Ok(session)
    }

    /// Whether PC audio is being captured.
    ///
    /// # Returns
    /// `true` if a capture back-end is running.
    pub fn has_capture(&self) -> bool {
        #[cfg(any(windows, target_os = "linux"))]
        return self.inner.capture.is_some();
        #[allow(unreachable_code)]
        false
    }

    /// Where microphone audio goes.
    ///
    /// # Returns
    /// A display name, or `None` without a microphone.
    pub fn mic_name(&self) -> Option<&str> {
        self.mic_name.as_deref()
    }

    /// Whether a virtual microphone is available.
    ///
    /// # Returns
    /// `true` if microphone audio can be written.
    pub fn has_mic(&self) -> bool {
        #[cfg(any(windows, target_os = "linux"))]
        return self.inner.mic.is_some();
        #[allow(unreachable_code)]
        false
    }

    /// Waits for the next PC audio packet (1152 bytes, 48 kHz stereo `s16le`).
    ///
    /// # Returns
    /// The packet; never resolves when capture is not running; `None` when capture ended.
    pub async fn next_capture(&mut self) -> Option<Vec<u8>> {
        #[cfg(any(windows, target_os = "linux"))]
        if let Some(c) = self.inner.capture.as_mut() {
            return c.next().await;
        }
        std::future::pending().await
    }

    /// Writes headset microphone audio.
    ///
    /// # Arguments
    /// * `pcm` - 48 kHz mono `s16le`.
    /// * `gain` - linear gain ([`vd_protocol::channels::audio::mic_gain`]).
    ///
    /// # Errors
    /// [`PlatformError::Io`] if the Linux player exited.
    pub async fn write_mic(&mut self, pcm: &[u8], gain: f32) -> Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(m) = self.inner.mic.as_mut() {
            return m.write(pcm, gain).await;
        }
        #[cfg(windows)]
        if let Some(m) = self.inner.mic.as_ref() {
            let mut buf = pcm.to_vec();
            apply_gain_s16(&mut buf, gain);
            m.write(buf);
        }
        let _ = (pcm, gain);
        Ok(())
    }

    /// Stops everything and restores the previous audio configuration.
    pub async fn stop(self) {
        #[cfg(target_os = "linux")]
        {
            let mut inner = self.inner;
            if let Some(c) = inner.capture.as_mut() {
                c.stop().await;
            }
            if let Some(m) = inner.mic.as_mut() {
                m.stop().await;
            }
            if let Some(r) = inner.routing {
                r.restore().await;
            }
        }
        #[cfg(windows)]
        {
            let mut inner = self.inner;
            tokio::task::spawn_blocking(move || {
                if let Some(c) = inner.capture.as_mut() {
                    c.stop();
                }
                if let Some(m) = inner.mic.as_mut() {
                    m.stop();
                }
            })
            .await
            .ok();
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = self;
    }
}
