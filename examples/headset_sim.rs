//! Minimal headset emulator for end-to-end testing of the streamer in `run --direct` mode.
//!
//! It implements the headset side of the reverse-engineered protocol: connects the messaging,
//! video, audio and data channels, sends `MobileSettingsReload`/`UserSettingsReload`, streams
//! headset state packets, and writes the received video bitstream to a file so it can be
//! inspected with `ffprobe`.
//!
//! ```text
//! cargo run -- run --direct            # terminal 1 (set Port.CaptureSource = "test" to avoid a display)
//! cargo run --example headset_sim -- 127.0.0.1 out.h264 10
//! cargo run --example headset_sim -- 127.0.0.1 out.h264 10 length-prefixed   # like a Quest (UseAnnexB = false)
//! cargo run --example headset_sim -- 127.0.0.1 out.h264 10 length-prefixed remove-monitor
//! cargo run --example headset_sim -- 127.0.0.1 out.h264 10 length-prefixed restart-streaming
//! ```

use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use vd_protocol::channels::{AUDIO_PORT, DATA_PORT, MESSAGING_PORT, VIDEO_PORT, video_tag};
use vd_protocol::headset_state::HeadsetState;
use vd_protocol::math::Size2;
use vd_protocol::settings::{SharedMobileSettings, SharedUserSettings, reload_message};
use vd_protocol::types::HmdType;
use vd_protocol::wire::{FrameHeader, SecondaryFrameHeader, StreamDescriptor};

/// Connects to `host:port`, retrying for a few seconds while the streamer starts its listener.
///
/// # Arguments
/// * `host` - streamer address.
/// * `port` - TCP port.
///
/// # Returns
/// The connected stream.
///
/// # Errors
/// The last connection error after retries.
async fn connect(host: &str, port: u16) -> std::io::Result<TcpStream> {
    let mut last = None;
    for _ in 0..50 {
        match TcpStream::connect((host, port)).await {
            Ok(s) => {
                s.set_nodelay(true)?;
                return Ok(s);
            }
            Err(e) => last = Some(e),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("connect failed")))
}

/// Entry point: `headset_sim <host> <output file> <seconds> [length-prefixed|annex-b] [remove-monitor|restart-streaming|bandwidth-drop]`.
///
/// # Errors
/// I/O errors.
#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let host = args.get(1).cloned().unwrap_or_else(|| "127.0.0.1".into());
    let out_path = args.get(2).cloned().unwrap_or_else(|| "headset_sim.h264".into());
    let seconds: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(10);
    // `length-prefixed` asks for length-prefixed NAL units like a Quest does (UseAnnexB = false).
    let annex_b = args.get(4).map(String::as_str) != Some("length-prefixed");

    let mut messaging = connect(&host, MESSAGING_PORT).await?;
    println!("messaging connected");
    let mobile = SharedMobileSettings {
        hmd_type: HmdType::QUEST3,
        resolution: Size2::new(2064, 2208),
        desktop_framerate: 30,
        measured_bandwidth: 20_000_000,
        use_annex_b: annex_b,
        ..Default::default()
    };
    for msg in [reload_message(&mobile).unwrap(), reload_message(&SharedUserSettings::default()).unwrap()] {
        messaging.write_all(&msg.encode().unwrap()).await?;
    }

    let mut video = connect(&host, VIDEO_PORT).await?;
    let mut audio = connect(&host, AUDIO_PORT).await?;
    let mut data = connect(&host, DATA_PORT).await?;
    println!("all channels connected");

    // Bandwidth probe: i32 BE length + random bytes.
    let n = data.read_i32().await?;
    let mut probe = vec![0u8; n.max(0) as usize];
    data.read_exact(&mut probe).await?;
    println!("bandwidth probe: {n} bytes");

    // Headset state sender (also keeps the data channel alive).
    let (mut data_r, mut data_w) = data.into_split();
    tokio::spawn(async move {
        let mut s = HeadsetState::new();
        loop {
            s.frame_index += 1;
            if data_w.write_all(&s.encode_packet()).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(14)).await;
        }
    });
    tokio::spawn(async move {
        let mut sink = [0u8; 4096];
        while matches!(data_r.read(&mut sink).await, Ok(n) if n > 0) {}
    });
    // Audio sink.
    let audio_bytes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ab = audio_bytes.clone();
    tokio::spawn(async move {
        loop {
            let Ok(len) = audio.read_i32_le().await else { return };
            let mut buf = vec![0u8; len.max(0) as usize];
            if audio.read_exact(&mut buf).await.is_err() {
                return;
            }
            ab.fetch_add(buf.len(), std::sync::atomic::Ordering::Relaxed);
        }
    });
    // Control messages from the streamer.
    let (mut msg_r, mut msg_w) = messaging.into_split();
    let can_remove = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cr = can_remove.clone();
    tokio::spawn(async move {
        loop {
            let mut header = [0u8; 5];
            if msg_r.read_exact(&mut header).await.is_err() {
                return;
            }
            let (ty, len) = vd_protocol::NetMessage::parse_header(&header).unwrap();
            let mut payload = vec![0u8; len];
            if msg_r.read_exact(&mut payload).await.is_err() {
                return;
            }
            let msg = vd_protocol::NetMessage { raw_type: ty, payload };
            let text = msg.payload_text().unwrap_or_default();
            // CanRemoveMonitor arrives in the full reload or as a property change.
            if text.contains("\"CanRemoveMonitor\":true") {
                cr.store(true, std::sync::atomic::Ordering::Relaxed);
            } else if text.contains("\"CanRemoveMonitor\":false") {
                cr.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            if text.len() < 200 {
                println!("message from streamer: {msg} {text}");
            } else {
                println!("message from streamer: {msg} ({} bytes)", text.len());
            }
        }
    });
    // `remove-monitor` presses the headset's remove-monitor button after 3 s; `restart-streaming`
    // sends the key-frame request the headset makes e.g. after recentring.
    // `bandwidth-drop` reports a lower measured bandwidth (the streamer must lower its bitrate).
    let action = match args.get(5).map(String::as_str) {
        Some("remove-monitor") => Some(vd_protocol::NetMessage::new(vd_protocol::MessageType::RemoveMonitor)),
        Some("restart-streaming") => Some(vd_protocol::NetMessage::new(vd_protocol::MessageType::RestartStreaming)),
        Some("bandwidth-drop") => Some(vd_protocol::settings::property_message::<SharedMobileSettings, _>("MeasuredBandwidth", &5_000_000i32).unwrap()),
        _ => None,
    };
    if let Some(msg) = action {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            println!("sending {msg}");
            let _ = msg_w.write_all(&msg.encode().unwrap()).await;
            tokio::time::sleep(Duration::from_secs(3600)).await;
            drop(msg_w);
        });
    }

    let mut out = tokio::fs::File::create(&out_path).await?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    // Frames of monitors 1, 2, … (monitor 0 uses full frame headers).
    let mut secondary: std::collections::BTreeMap<i32, u64> = std::collections::BTreeMap::new();
    let mut secondary_bad = 0u64;
    let mut descriptors = 0u32;
    // Monitor-0 video bytes per second of the session (shows bitrate changes).
    let mut per_second: Vec<u64> = Vec::new();
    let mut secondary_unpaired = 0u64;
    let mut primary_times: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let (mut frames, mut keyframes, mut bytes) = (0u64, 0u64, 0u64);
    let mut latency_sum = 0f64;
    while Instant::now() < deadline {
        let tag = match tokio::time::timeout(Duration::from_secs(5), video.read_u8()).await {
            Ok(Ok(t)) => t,
            _ => break,
        };
        match tag {
            video_tag::FRAME => {
                let mut h = [0u8; FrameHeader::SIZE];
                video.read_exact(&mut h).await?;
                let header = FrameHeader::from_bytes(&h).unwrap();
                primary_times.insert(header.timestamp);
                let mut payload = vec![0u8; header.payload_size as usize];
                video.read_exact(&mut payload).await?;
                if !annex_b {
                    // Primary frames follow UseAnnexB: convert back for the output file.
                    payload = vd_protocol::nal::length_prefixed_to_annex_b(&payload).unwrap_or_default();
                }
                let nals = vd_protocol::nal::split_annex_b(&payload);
                if nals.iter().any(|n| vd_protocol::nal::is_keyframe_nal(vd_protocol::nal::Bitstream::H264, n) || vd_protocol::nal::is_keyframe_nal(vd_protocol::nal::Bitstream::Hevc, n)) {
                    keyframes += 1;
                }
                frames += 1;
                bytes += payload.len() as u64;
                let second = (Instant::now() + Duration::from_secs(seconds)).saturating_duration_since(deadline).as_secs() as usize;
                if per_second.len() <= second {
                    per_second.resize(second + 1, 0);
                }
                per_second[second] += payload.len() as u64;
                latency_sum += f64::from(header.encode_latency_ms);
                out.write_all(&payload).await?;
            }
            video_tag::SECONDARY_FRAME => {
                let mut h = [0u8; SecondaryFrameHeader::SIZE];
                video.read_exact(&mut h).await?;
                let header: SecondaryFrameHeader = bytemuck::pod_read_unaligned(&h);
                let mut payload = vec![0u8; header.payload_size as usize];
                video.read_exact(&mut payload).await?;
                *secondary.entry(header.monitor_index).or_insert(0u64) += 1;
                // The headset pairs secondary frames with the primary frame of the same timestamp.
                if !primary_times.contains(&header.timestamp) {
                    secondary_unpaired += 1;
                }
                // The headset decodes secondary monitors as Annex B (start codes), always.
                if !(payload.starts_with(&[0, 0, 0, 1]) || payload.starts_with(&[0, 0, 1])) {
                    secondary_bad += 1;
                }
            }
            video_tag::DESCRIPTOR => {
                descriptors += 1;
                let mut d = [0u8; StreamDescriptor::SIZE];
                video.read_exact(&mut d).await?;
                let desc: StreamDescriptor = bytemuck::pod_read_unaligned(&d);
                println!(
                    "stream descriptor: codec={:?} fps={} size={}x{} views={}",
                    desc.video_codec(),
                    desc.framerate,
                    desc.size0.width,
                    desc.size0.height,
                    desc.view_count
                );
            }
            video_tag::TEXT => {
                let len = video.read_i32_le().await?;
                let mut t = vec![0u8; len.max(0) as usize];
                video.read_exact(&mut t).await?;
            }
            other => {
                println!("unknown video tag {other}");
                break;
            }
        }
    }
    out.flush().await?;
    println!("headset sees CanRemoveMonitor = {}", can_remove.load(std::sync::atomic::Ordering::Relaxed));
    println!("stream descriptors received: {descriptors}");
    println!("monitor 0 kbit/s per second: {:?}", per_second.iter().map(|b| b * 8 / 1000).collect::<Vec<_>>());
    for (monitor, n) in &secondary {
        println!("monitor {monitor}: {n} secondary frames");
    }
    if !secondary.is_empty() {
        println!("secondary frames without an Annex B start code: {secondary_bad}");
        println!("secondary frames without a primary frame of the same timestamp: {secondary_unpaired}");
    }
    println!(
        "received {frames} frames ({keyframes} key frames), {:.1} kB video, {} bytes audio, mean encoder latency {:.1} ms -> {out_path}",
        bytes as f64 / 1000.0,
        audio_bytes.load(std::sync::atomic::Ordering::Relaxed),
        if frames > 0 { latency_sum / frames as f64 } else { 0.0 }
    );
    Ok(())
}
