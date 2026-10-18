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

/// Entry point: `headset_sim <host> <output file> <seconds>`.
///
/// # Errors
/// I/O errors.
#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let host = args.get(1).cloned().unwrap_or_else(|| "127.0.0.1".into());
    let out_path = args.get(2).cloned().unwrap_or_else(|| "headset_sim.h264".into());
    let seconds: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(10);

    let mut messaging = connect(&host, MESSAGING_PORT).await?;
    println!("messaging connected");
    let mut mobile = SharedMobileSettings::default();
    mobile.hmd_type = HmdType::QUEST3;
    mobile.resolution = Size2::new(2064, 2208);
    mobile.desktop_framerate = 30;
    mobile.measured_bandwidth = 20_000_000;
    mobile.use_annex_b = true;
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
    tokio::spawn(async move {
        loop {
            let mut header = [0u8; 5];
            if messaging.read_exact(&mut header).await.is_err() {
                return;
            }
            let (ty, len) = vd_protocol::NetMessage::parse_header(&header).unwrap();
            let mut payload = vec![0u8; len];
            if messaging.read_exact(&mut payload).await.is_err() {
                return;
            }
            let msg = vd_protocol::NetMessage { raw_type: ty, payload };
            println!("message from streamer: {msg} {}", msg.payload_text().unwrap_or_default());
        }
    });

    let mut out = tokio::fs::File::create(&out_path).await?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
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
                let mut payload = vec![0u8; header.payload_size as usize];
                video.read_exact(&mut payload).await?;
                let nals = vd_protocol::nal::split_annex_b(&payload);
                if nals.iter().any(|n| vd_protocol::nal::is_keyframe_nal(vd_protocol::nal::Bitstream::H264, n) || vd_protocol::nal::is_keyframe_nal(vd_protocol::nal::Bitstream::Hevc, n)) {
                    keyframes += 1;
                }
                frames += 1;
                bytes += payload.len() as u64;
                latency_sum += f64::from(header.encode_latency_ms);
                out.write_all(&payload).await?;
            }
            video_tag::SECONDARY_FRAME => {
                let mut h = [0u8; SecondaryFrameHeader::SIZE];
                video.read_exact(&mut h).await?;
                let header: SecondaryFrameHeader = bytemuck::pod_read_unaligned(&h);
                let mut payload = vec![0u8; header.payload_size as usize];
                video.read_exact(&mut payload).await?;
            }
            video_tag::DESCRIPTOR => {
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
    println!(
        "received {frames} frames ({keyframes} key frames), {:.1} kB video, {} bytes audio, mean encoder latency {:.1} ms -> {out_path}",
        bytes as f64 / 1000.0,
        audio_bytes.load(std::sync::atomic::Ordering::Relaxed),
        if frames > 0 { latency_sum / frames as f64 } else { 0.0 }
    );
    Ok(())
}
