//! Audio back-end smoke test (no headset needed).
//!
//! Starts an [`AudioSession`] with loopback capture and the virtual microphone, prints how many
//! 48 kHz stereo packets were captured (play something on the PC while it runs), and pushes a
//! 440 Hz tone into the virtual microphone (record "Microphone (Virtual Desktop Audio)" on
//! Windows, or "Virtual Desktop Microphone" on Linux, to hear it).
//!
//! With `headset-only` the PC speakers are silenced the way "VR headset only" does it (on
//! Windows the Virtual Desktop speakers become the default device until the probe ends).
//!
//! ```text
//! cargo run --example audio_probe -- 5
//! cargo run --example audio_probe -- 10 headset-only
//! cargo run --example audio_probe -- 10 headset-only driver-free   # ignore the Virtual Desktop driver
//! cargo run --example audio_probe -- 10 "mic={0.0.0.00000000}.{…}"   # microphone into that playback device
//! ```

use std::time::{Duration, Instant};

use vd_platform::audio::{AudioOptions, AudioSession};

/// Entry point: `audio_probe [seconds] [headset-only] [driver-free] [mic=<device>]`.
///
/// # Errors
/// Never; failures are printed.
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter("debug").with_target(false).init();
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5);
    let headset_only = std::env::args().any(|a| a == "headset-only");
    let opts = AudioOptions {
        capture: true,
        headset_only,
        virtual_speaker: !std::env::args().any(|a| a == "driver-free"),
        voicemeeter: false,
        microphone: true,
        select_microphone: false,
        microphone_device: std::env::args().find_map(|a| a.strip_prefix("mic=").map(str::to_owned)).unwrap_or_else(|| "auto".to_owned()),
        manage_devices: true,
    };
    let mut session = match AudioSession::start(opts).await {
        Ok(s) => s,
        Err(e) => {
            println!("audio session failed: {e}");
            return;
        }
    };
    println!("capture={} microphone={} ({})", session.has_capture(), session.has_mic(), session.mic_name().unwrap_or("none"));
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let (mut packets, mut peak) = (0u64, 0i16);
    let mut phase = 0f32;
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    while Instant::now() < deadline {
        tokio::select! {
            p = session.next_capture() => {
                let Some(p) = p else { break };
                packets += 1;
                for s in p.chunks_exact(2) {
                    peak = peak.max(i16::from_le_bytes([s[0], s[1]]).saturating_abs());
                }
            }
            _ = tick.tick() => {
                // 20 ms of a 440 Hz tone at 48 kHz mono.
                let mut pcm = Vec::with_capacity(1920);
                for _ in 0..960 {
                    let v = (phase.sin() * 8000.0) as i16;
                    pcm.extend_from_slice(&v.to_le_bytes());
                    phase += 2.0 * std::f32::consts::PI * 440.0 / 48_000.0;
                }
                let _ = session.write_mic(&pcm, 1.0).await;
            }
        }
    }
    println!(
        "captured {packets} packets ({:.2} s of audio), peak level {peak}",
        packets as f64 * 1152.0 / (48_000.0 * 4.0)
    );
    session.stop().await;
}
