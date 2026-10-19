//! Audio streaming services.
//!
//! [`AudioSession`] is the platform-neutral entry point used by the streamer: it captures PC
//! audio as 48 kHz stereo `s16le` packets (C#: `LoopbackCapture`) and feeds headset microphone
//! audio into a virtual capture device (C#: `Microphone`).
//!
//! | platform | PC → headset                              | headset → PC microphone                         |
//! |----------|-------------------------------------------|-------------------------------------------------|
//! | Windows  | [`wasapi`]: WASAPI loopback (+ endpoint mute for headset-only) | [`wasapi`]: `IKsControl` on the installed `vdvad.sys` driver |
//! | Linux    | [`pulse`]: `parec` on a null sink monitor | [`pulse`]: `pacat` into a null sink + remap source |

pub mod convert;
#[cfg(target_os = "linux")]
pub mod pulse;
#[cfg(windows)]
pub mod wasapi;

pub use convert::apply_gain_s16;

use tracing::warn;

use crate::error::{PlatformError, Result};

/// What an [`AudioSession`] should provide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioOptions {
    /// Capture PC audio for the headset.
    pub capture: bool,
    /// `AudioStreaming::HeadsetOnly` — silence the PC speakers while streaming.
    pub headset_only: bool,
    /// Forward the headset microphone (`SharedUserSettings.MicPassthrough`).
    pub microphone: bool,
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

#[cfg(windows)]
#[derive(Default)]
struct Inner {
    capture: Option<wasapi::WasapiCapture>,
    mic: Option<wasapi::KsMicrophone>,
}

#[cfg(not(any(windows, target_os = "linux")))]
#[derive(Default)]
struct Inner;

/// A running audio session.
pub struct AudioSession {
    inner: Inner,
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
            if opts.capture {
                match wasapi::WasapiCapture::start(opts.headset_only) {
                    Ok(c) => inner.capture = Some(c),
                    Err(e) => warn!(error = %e, "PC audio capture unavailable"),
                }
            }
            if opts.microphone {
                match wasapi::KsMicrophone::start() {
                    Ok(m) => inner.mic = Some(m),
                    Err(e) => warn!(error = %e, "virtual microphone unavailable"),
                }
            }
        }
        let session = Self { inner };
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
