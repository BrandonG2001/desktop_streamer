//! Driver-free "VR headset only" (Windows): silence every playback device while streaming.
//!
//! Without the Virtual Desktop audio driver there is no device that plays into nowhere, so the
//! port records application audio with process loopback (see `wasapi`) and silences the PC by
//! muting **every** active render endpoint and turning its volume to zero.  Muting alone is not
//! enough: Bluetooth headphones with absolute volume can ignore the mute flag, and applications
//! that play on a non-default device would still be heard.  Loopback capture taps the audio
//! before endpoint volume, so the headset still receives it at full level.
//!
//! The original mute and volume of each endpoint are written to a file in the temp directory
//! before anything changes, so [`recover`] can restore them after a crash.

use std::path::PathBuf;

use tracing::{debug, info, warn};
use windows::Win32::Foundation::BOOL;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{DEVICE_STATE_ACTIVE, IMMDeviceEnumerator, eRender};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::core::HSTRING;

/// Saved state of one endpoint.
#[derive(Debug, Clone, PartialEq)]
struct Saved {
    /// Endpoint id.
    id: String,
    /// Mute flag before streaming.
    muted: bool,
    /// Master volume (0..1) before streaming.
    level: f32,
}

/// Restore file in the temp directory.
///
/// # Returns
/// The path.
fn restore_file() -> PathBuf {
    std::env::temp_dir().join("vd-rust-speaker-volume.txt")
}

/// Serialises saved states, one `muted<TAB>level<TAB>id` line each.
///
/// # Arguments
/// * `saved` - states.
///
/// # Returns
/// The text.
fn to_text(saved: &[Saved]) -> String {
    saved.iter().map(|s| format!("{}\t{}\t{}", u8::from(s.muted), s.level, s.id)).collect::<Vec<_>>().join("\n")
}

/// Parses [`to_text`] output (malformed lines are skipped).
///
/// # Arguments
/// * `text` - file contents.
///
/// # Returns
/// The states.
fn from_text(text: &str) -> Vec<Saved> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            let muted = it.next()? == "1";
            let level = it.next()?.parse().ok()?;
            Some(Saved { muted, level, id: it.next()?.to_owned() })
        })
        .collect()
}

/// Opens the volume control of an endpoint.
///
/// # Arguments
/// * `enumerator` - device enumerator.
/// * `id` - endpoint id.
///
/// # Returns
/// The control, or `None` if the device is gone.
fn volume(enumerator: &IMMDeviceEnumerator, id: &str) -> Option<IAudioEndpointVolume> {
    // SAFETY: COM calls on a thread where COM is initialised (caller's contract).
    unsafe { enumerator.GetDevice(&HSTRING::from(id)).ok()?.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok() }
}

/// Puts saved states back.
///
/// # Arguments
/// * `enumerator` - device enumerator.
/// * `saved` - states to restore.
fn restore(enumerator: &IMMDeviceEnumerator, saved: &[Saved]) {
    for s in saved {
        let Some(vol) = volume(enumerator, &s.id) else { continue };
        // SAFETY: valid endpoint volume interface.
        unsafe {
            let _ = vol.SetMasterVolumeLevelScalar(s.level, std::ptr::null());
            let _ = vol.SetMute(BOOL::from(s.muted), std::ptr::null());
        }
    }
}

/// While alive, every active playback device is muted with its volume at zero.
pub struct SpeakerSilencer {
    enumerator: IMMDeviceEnumerator,
    saved: Vec<Saved>,
    /// Endpoints left alone (virtual cables carrying the headset microphone).
    keep: Vec<String>,
}

impl SpeakerSilencer {
    /// Silences all active render endpoints.  COM must be initialised on this thread, and the
    /// silencer must be dropped on it.
    ///
    /// # Arguments
    /// * `enumerator` - device enumerator.
    /// * `keep` - endpoint ids that must stay audible (virtual-cable microphone sinks).
    ///
    /// # Returns
    /// The silencer (restores on drop).
    pub fn engage(enumerator: &IMMDeviceEnumerator, keep: Vec<String>) -> Self {
        let mut me = Self { enumerator: enumerator.clone(), saved: Vec::new(), keep };
        me.refresh();
        info!(devices = me.saved.len(), "PC speakers silenced while streaming");
        me
    }

    /// Silences endpoints that appeared since the last call (e.g. headphones plugged in).
    pub fn refresh(&mut self) {
        // SAFETY: COM calls on the owning thread.
        let ids: Vec<String> = unsafe {
            let Ok(list) = self.enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else { return };
            let n = list.GetCount().unwrap_or(0);
            (0..n).filter_map(|i| list.Item(i).ok()).map(|d| super::wasapi::device_id(&d)).filter(|id| !id.is_empty()).collect()
        };
        let mut added = false;
        for id in ids {
            if self.saved.iter().any(|s| s.id == id) || self.keep.contains(&id) {
                continue;
            }
            let Some(vol) = volume(&self.enumerator, &id) else { continue };
            // SAFETY: valid endpoint volume interface.
            let (muted, level) = unsafe { (vol.GetMute().map(|b| b.as_bool()).unwrap_or(false), vol.GetMasterVolumeLevelScalar().unwrap_or(1.0)) };
            self.saved.push(Saved { id: id.clone(), muted, level });
            added = true;
            // Record before changing anything so a crash can be undone at the next start.
            if let Err(e) = std::fs::write(restore_file(), to_text(&self.saved)) {
                debug!(error = %e, "cannot write the speaker restore file");
            }
            // SAFETY: valid endpoint volume interface.
            unsafe {
                if let Err(e) = vol.SetMute(BOOL::from(true), std::ptr::null()) {
                    warn!(device = %id, error = %e, "cannot mute playback device");
                }
                let _ = vol.SetMasterVolumeLevelScalar(0.0, std::ptr::null());
            }
        }
        if added {
            debug!(devices = self.saved.len(), "playback devices silenced");
        }
    }
}

impl Drop for SpeakerSilencer {
    /// Restores every endpoint's mute flag and volume.
    fn drop(&mut self) {
        restore(&self.enumerator, &self.saved);
        let _ = std::fs::remove_file(restore_file());
        info!("PC speaker volume restored");
    }
}

/// Restores speaker volumes left changed by a crashed session.  COM must be initialised on this
/// thread.
///
/// # Arguments
/// * `enumerator` - device enumerator.
pub fn recover(enumerator: &IMMDeviceEnumerator) {
    let path = restore_file();
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let saved = from_text(&text);
    if !saved.is_empty() {
        info!("restoring PC speaker volume after an unclean exit");
        restore(enumerator, &saved);
    }
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_volumes_round_trip() {
        let saved = vec![
            Saved { id: "{0.0.0.00000000}.{a}".into(), muted: false, level: 0.42 },
            Saved { id: "{0.0.0.00000000}.{b}".into(), muted: true, level: 1.0 },
        ];
        assert_eq!(from_text(&to_text(&saved)), saved);
    }
}
