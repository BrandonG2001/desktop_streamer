//! Audio capture and virtual microphone.
//!
//! C#: `VirtualDesktop.Streamer.LoopbackCapture` (WASAPI loopback of the default render device
//! or of the "Virtual Desktop Audio" device, converted to 48 kHz stereo s16 with gain 0.9 and
//! sent in 1152-byte packets), `VirtualDesktop.Streamer.Microphone` (48 kHz mono s16 from the
//! headset played into the virtual capture device with gain `lerp(0.1, 2, MicVolume)`) and
//! `IAudioService` (switching the Windows default devices to the `vdvad.sys` virtual devices).
//!
//! Linux implementation (PulseAudio or PipeWire with `pipewire-pulse`):
//!
//! * `AudioStreaming::HeadsetOnly` → load a `module-null-sink` named **Virtual Desktop Audio**,
//!   make it the default sink (so PC speakers go silent) and record its monitor;
//! * `HeadsetAndComputer` → record the monitor of the current default sink;
//! * microphone passthrough → a null sink + `module-remap-source` exposed as
//!   **Virtual Desktop Microphone** and made the default source.
//!
//! The helpers (`pactl`, `parec`, `pacat`) ship with every Pulse/PipeWire desktop.  The Windows
//! implementation (WASAPI + the original virtual audio driver) is not ported; use the original
//! streamer on Windows or extend [`LoopbackCapture`] with a WASAPI backend.

use std::process::Stdio;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use vd_protocol::channels::audio::{SAMPLE_RATE, SPEAKER_GAIN, SPEAKER_PACKET_BYTES};

use crate::error::{PlatformError, Result};

/// Name of the virtual speaker sink.
pub const VIRTUAL_SINK: &str = "virtual_desktop_audio";
/// Name of the sink the microphone is played into.
pub const MIC_SINK: &str = "virtual_desktop_mic_sink";
/// Name of the virtual microphone source.
pub const MIC_SOURCE: &str = "virtual_desktop_mic";

/// Multiplies signed 16-bit little-endian samples by `gain` with saturation.
///
/// # Arguments
/// * `pcm` - samples, modified in place.
/// * `gain` - linear gain.
pub fn apply_gain_s16(pcm: &mut [u8], gain: f32) {
    if (gain - 1.0).abs() < f32::EPSILON {
        return;
    }
    for s in pcm.chunks_exact_mut(2) {
        let v = f32::from(i16::from_le_bytes([s[0], s[1]])) * gain;
        s.copy_from_slice(&(v.clamp(-32768.0, 32767.0) as i16).to_le_bytes());
    }
}

async fn pactl(args: &[&str]) -> Result<String> {
    let out = Command::new("pactl").args(args).stdin(Stdio::null()).output().await.map_err(|e| PlatformError::Command(format!("pactl: {e}")))?;
    if !out.status.success() {
        return Err(PlatformError::Command(format!("pactl {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

async fn default_device(kind: &str) -> Option<String> {
    if let Ok(v) = pactl(&[&format!("get-default-{kind}")]).await {
        return Some(v);
    }
    let info = pactl(&["info"]).await.ok()?;
    let key = if kind == "sink" { "Default Sink:" } else { "Default Source:" };
    info.lines().find_map(|l| l.trim().strip_prefix(key)).map(|s| s.trim().to_owned())
}

/// Virtual devices created for a session; restore with [`AudioRouting::restore`].
#[derive(Debug, Default)]
pub struct AudioRouting {
    modules: Vec<String>,
    previous_sink: Option<String>,
    previous_source: Option<String>,
    /// Device whose monitor should be recorded (`None` = default sink monitor).
    pub capture_device: Option<String>,
    /// Sink to play microphone audio into (`None` when mic passthrough is off).
    pub mic_sink: Option<String>,
}

impl AudioRouting {
    /// Creates the virtual devices.  C#: `IAudioService.SetVirtualAudioDeviceRender/Capture`.
    ///
    /// # Arguments
    /// * `headset_only` - route PC audio exclusively to the headset.
    /// * `microphone` - expose the headset microphone as the default input.
    ///
    /// # Returns
    /// The routing state.
    ///
    /// # Errors
    /// [`PlatformError::Command`] if `pactl` is missing or refuses to load modules;
    /// [`PlatformError::Unsupported`] on non-Linux platforms.
    pub async fn setup(headset_only: bool, microphone: bool) -> Result<Self> {
        if !cfg!(target_os = "linux") {
            return Err(PlatformError::Unsupported("audio routing"));
        }
        let mut r = Self::default();
        if headset_only {
            r.previous_sink = default_device("sink").await;
            let m = pactl(&["load-module", "module-null-sink", &format!("sink_name={VIRTUAL_SINK}"), "sink_properties=device.description=\"Virtual Desktop Audio\""]).await?;
            r.modules.push(m);
            pactl(&["set-default-sink", VIRTUAL_SINK]).await?;
            r.capture_device = Some(format!("{VIRTUAL_SINK}.monitor"));
        }
        if microphone {
            r.previous_source = default_device("source").await;
            let m = pactl(&["load-module", "module-null-sink", &format!("sink_name={MIC_SINK}"), "sink_properties=device.description=\"Virtual Desktop Microphone Sink\""]).await?;
            r.modules.push(m);
            let m = pactl(&[
                "load-module",
                "module-remap-source",
                &format!("master={MIC_SINK}.monitor"),
                &format!("source_name={MIC_SOURCE}"),
                "source_properties=device.description=\"Virtual Desktop Microphone\"",
            ])
            .await?;
            r.modules.push(m);
            pactl(&["set-default-source", MIC_SOURCE]).await?;
            r.mic_sink = Some(MIC_SINK.to_owned());
        }
        info!(modules = ?r.modules, "audio routing set up");
        Ok(r)
    }

    /// Restores the previous defaults and unloads the modules.  C#: `IAudioService.RestoreDefaultAudioDevices`.
    pub async fn restore(self) {
        if let Some(s) = &self.previous_sink {
            let _ = pactl(&["set-default-sink", s]).await;
        }
        if let Some(s) = &self.previous_source {
            let _ = pactl(&["set-default-source", s]).await;
        }
        for m in self.modules.iter().rev() {
            if let Err(e) = pactl(&["unload-module", m]).await {
                warn!(module = m, error = %e, "failed to unload audio module");
            }
        }
    }
}

/// Records PC audio as 48 kHz stereo s16le packets.
pub struct LoopbackCapture {
    child: Child,
    packets: mpsc::Receiver<Vec<u8>>,
}

impl LoopbackCapture {
    /// Starts recording.
    ///
    /// # Arguments
    /// * `device` - source to record (`<sink>.monitor`), or `None` for `@DEFAULT_MONITOR@`.
    ///
    /// # Returns
    /// The running capture.
    ///
    /// # Errors
    /// [`PlatformError::Command`] if `parec` cannot be started; [`PlatformError::Unsupported`]
    /// on non-Linux platforms.
    pub fn start(device: Option<&str>) -> Result<Self> {
        if !cfg!(target_os = "linux") {
            return Err(PlatformError::Unsupported("loopback capture"));
        }
        let dev = device.unwrap_or("@DEFAULT_MONITOR@");
        let mut child = Command::new("parec")
            .args([&format!("--device={dev}"), "--format=s16le", &format!("--rate={SAMPLE_RATE}"), "--channels=2", "--latency-msec=6", "--process-time-msec=3"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| PlatformError::Command(format!("parec: {e}")))?;
        let mut out = child.stdout.take().ok_or_else(|| PlatformError::Command("parec stdout".into()))?;
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut buf = vec![0u8; SPEAKER_PACKET_BYTES];
            while out.read_exact(&mut buf).await.is_ok() {
                let mut pkt = buf.clone();
                apply_gain_s16(&mut pkt, SPEAKER_GAIN);
                if tx.send(pkt).await.is_err() {
                    break;
                }
            }
            debug!("loopback capture ended");
        });
        info!(device = dev, "loopback capture started");
        Ok(Self { child, packets: rx })
    }

    /// Waits for the next 1152-byte packet.
    ///
    /// # Returns
    /// The packet, or `None` when recording stopped.
    pub async fn next(&mut self) -> Option<Vec<u8>> {
        self.packets.recv().await
    }

    /// Stops recording.
    pub async fn stop(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

/// Plays headset microphone audio into the virtual microphone sink.
pub struct MicrophonePlayback {
    child: Child,
    stdin: ChildStdin,
}

impl MicrophonePlayback {
    /// Starts playback.
    ///
    /// # Arguments
    /// * `sink` - sink created by [`AudioRouting::setup`].
    ///
    /// # Returns
    /// The running playback.
    ///
    /// # Errors
    /// [`PlatformError::Command`] if `pacat` cannot be started.
    pub fn start(sink: &str) -> Result<Self> {
        let mut child = Command::new("pacat")
            .args(["--playback", &format!("--device={sink}"), "--format=s16le", &format!("--rate={SAMPLE_RATE}"), "--channels=1", "--latency-msec=20", "--raw"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| PlatformError::Command(format!("pacat: {e}")))?;
        let stdin = child.stdin.take().ok_or_else(|| PlatformError::Command("pacat stdin".into()))?;
        Ok(Self { child, stdin })
    }

    /// Writes one microphone buffer.
    ///
    /// # Arguments
    /// * `pcm` - 48 kHz mono s16le samples.
    /// * `gain` - linear gain ([`vd_protocol::channels::audio::mic_gain`]).
    ///
    /// # Errors
    /// [`PlatformError::Io`] if the player exited.
    pub async fn write(&mut self, pcm: &[u8], gain: f32) -> Result<()> {
        let mut buf = pcm.to_vec();
        apply_gain_s16(&mut buf, gain);
        self.stdin.write_all(&buf).await?;
        Ok(())
    }

    /// Stops playback.
    pub async fn stop(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_saturates() {
        let mut pcm = 30000i16.to_le_bytes().to_vec();
        apply_gain_s16(&mut pcm, 2.0);
        assert_eq!(i16::from_le_bytes([pcm[0], pcm[1]]), 32767);
    }
}
