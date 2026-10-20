//! Desktop streaming.  C#: `VirtualDesktop.Streamer.DesktopStreamer` and the desktop branch of the
//! `StreamerManager` update loop.
//!
//! For every streamed monitor the original opens the shared texture captured by the service,
//! renders it through the "Supersample" effect into the encoder's input texture, encodes it, and
//! sends monitor 0 with a full [`FrameHeader`] and the others as secondary frames.  Before the
//! first frame a [`StreamDescriptor`] announces codec, frame rate and per-monitor sizes.
//!
//! Sizing rules reproduced here:
//!
//! * monitor count = `min(monitors, max_monitors(hmd), max(1, MonitorCount))`, and 1 without a
//!   hardware encoder;
//! * each monitor is scaled down (aspect preserved) to the pixel budget of
//!   `max_desktop_resolution(hmd, count)`;
//! * frame rate = `min(display refresh, SharedMobileSettings.DesktopFramerate)`;
//! * bitrate per monitor = `DesktopBitrate / count`.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{info, warn};
use vd_media::EncodedFrame;
use vd_media::capture::{CaptureSource, CaptureSpec, Region};
use vd_media::cli::CliVideoPipeline;
use vd_media::codec::{EncoderConfig, resolve_codec};
use vd_platform::display::MonitorInfo;
use vd_protocol::hmd;
use vd_protocol::math::{Rect, Size2};
use vd_protocol::types::{Features, StreamingSource, VideoCodec};
use vd_protocol::wire::{FrameHeader, HapticFeedback, StreamDescriptor};

use crate::app::{App, MediaEnv};

/// Everything that determines the encoder pipelines; a change restarts them.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamPlan {
    /// Selected codec.
    pub codec: VideoCodec,
    /// Frames per second.
    pub framerate: u32,
    /// Bitrate of each monitor (bit/s).
    pub bitrate: u32,
    /// `(monitor, encoded size)` per streamed monitor.
    pub monitors: Vec<(MonitorInfo, Size2)>,
    /// Bake the cursor into the video (headsets) or not (flat devices).
    pub draw_cursor: bool,
    /// Send Annex-B (`true`) or length-prefixed NAL units.
    pub use_annex_b: bool,
    /// Key-frame request counter at planning time.
    pub keyframe_generation: u64,
}

/// Lists monitors, falling back to one virtual monitor at the headset's optimal resolution when
/// enumeration is unavailable (e.g. Wayland without XWayland).
///
/// # Arguments
/// * `fallback` - size of the fallback monitor.
///
/// # Returns
/// The monitors.
pub fn list_monitors(fallback: (i32, i32)) -> Vec<MonitorInfo> {
    match vd_platform::display::monitors() {
        Ok(m) if !m.is_empty() => m,
        Ok(_) | Err(_) => vec![MonitorInfo { id: "default".into(), index: 0, x: 0, y: 0, width: fallback.0 as u32, height: fallback.1 as u32, refresh_hz: 60, primary: true }],
    }
}

/// Builds the plan from the current state.
///
/// # Arguments
/// * `app` - application context.
/// * `media` - encoder environment.
///
/// # Returns
/// The plan, or `None` when nothing can be streamed.
pub fn plan(app: &App, media: &MediaEnv) -> Option<StreamPlan> {
    let mobile = app.state.mobile.borrow().clone();
    let user = app.state.user.borrow().clone();
    let dynamic = app.state.dynamic.borrow().clone();
    let (preferred, monitor_count) = {
        let s = app.settings.lock().expect("poisoned");
        (s.preferred_codec, s.monitor_count)
    };
    let hmd_type = mobile.hmd_type;
    if dynamic.monitors.is_empty() {
        return None;
    }
    let max = hmd::max_monitors(hmd_type).min(dynamic.monitors.len() as i32);
    let count = if media.backend.is_hardware() { max.min(monitor_count.max(1)).max(1) } else { 1 } as usize;
    let (mw, mh) = hmd::max_desktop_resolution(hmd_type, count as i32);
    let budget = i64::from(mw) * i64::from(mh);
    let mut monitors = Vec::with_capacity(count);
    for i in 0..count {
        let m = dynamic.monitors[(dynamic.monitor_start_index + i) % dynamic.monitors.len()].clone();
        let size = Size2::new(m.width as i32, m.height as i32).clamp_to_pixels(budget);
        monitors.push((m, size));
    }
    let refresh = monitors[0].0.refresh_hz.max(1);
    let framerate = refresh.min(mobile.desktop_framerate.max(1) as u32);
    let total = mobile.desktop_bitrate(&user).max(2_000_000);
    let bitrate = (total / count as i32).max(1_000_000) as u32;
    let features: Features = mobile.features;
    let codec = resolve_codec(preferred, hmd_type, features, &media.caps);
    Some(StreamPlan {
        codec,
        framerate,
        bitrate,
        monitors,
        draw_cursor: !hmd::is_flat_device(hmd_type),
        use_annex_b: mobile.use_annex_b,
        keyframe_generation: dynamic.keyframe_requests,
    })
}

/// Maps the configured capture source to a [`CaptureSource`] for one monitor.
///
/// # Arguments
/// * `app` - application context.
/// * `media` - encoder environment (available grabbers).
/// * `m` - monitor.
///
/// # Returns
/// The capture source.
pub fn capture_source(app: &App, media: &MediaEnv, m: &MonitorInfo) -> CaptureSource {
    let port = app.settings.lock().expect("poisoned").port.clone();
    let region = Region { x: m.x, y: m.y, width: m.width, height: m.height };
    let display = if port.x11_display.is_empty() { std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into()) } else { port.x11_display.clone() };
    let kind = port.capture_source.to_ascii_lowercase();
    match kind.as_str() {
        "test" => CaptureSource::TestPattern { width: m.width, height: m.height },
        "custom" => CaptureSource::Custom { args: port.custom_capture_args.clone() },
        "x11" | "x11grab" => CaptureSource::X11 { display, region },
        "kms" | "kmsgrab" => CaptureSource::KmsGrab { device: port.drm_card.clone(), plane: None },
        "ddagrab" => CaptureSource::DdaGrab { output_index: m.index },
        "gdigrab" => CaptureSource::GdiGrab { region },
        "avfoundation" => CaptureSource::AvFoundation { screen: m.index },
        _ => {
            if cfg!(windows) {
                if media.ffmpeg.has_input("ddagrab") { CaptureSource::DdaGrab { output_index: m.index } } else { CaptureSource::GdiGrab { region } }
            } else if cfg!(target_os = "macos") {
                CaptureSource::AvFoundation { screen: m.index }
            } else if std::env::var_os("DISPLAY").is_some() && media.ffmpeg.has_input("x11grab") {
                CaptureSource::X11 { display, region }
            } else {
                CaptureSource::KmsGrab { device: port.drm_card.clone(), plane: None }
            }
        }
    }
}

/// Starts one pipeline per monitor and forwards frames to `tx` tagged with the monitor index.
///
/// # Arguments
/// * `app` - application context.
/// * `media` - encoder environment.
/// * `plan` - stream plan.
/// * `tx` - frame sink.
///
/// # Returns
/// The started pipelines (kept to stop them later).
fn start_pipelines(app: &App, media: &MediaEnv, plan: &StreamPlan, tx: &mpsc::Sender<(usize, EncodedFrame)>) -> Vec<tokio::task::JoinHandle<()>> {
    let (gop_secs, render_node, aq, two_pass) = {
        let s = app.settings.lock().expect("poisoned");
        (s.port.keyframe_interval_seconds.max(1), s.port.render_node.clone(), s.enable_aq, s.enable_2pass)
    };
    let mut tasks = Vec::new();
    for (i, (m, size)) in plan.monitors.iter().enumerate() {
        let capture = CaptureSpec {
            source: capture_source(app, media, m),
            framerate: plan.framerate,
            draw_cursor: plan.draw_cursor,
            output_size: (size.width as u32, size.height as u32),
            source_size: Some((m.width, m.height)),
        };
        let enc = EncoderConfig {
            backend: media.backend,
            codec: plan.codec,
            width: size.width as u32,
            height: size.height as u32,
            framerate: plan.framerate,
            bitrate: plan.bitrate,
            enable_aq: aq,
            yuv444: false,
            two_pass: two_pass && media.caps.two_pass,
            gop: plan.framerate * gop_secs,
            render_node: render_node.clone(),
        };
        let mut pipeline = match CliVideoPipeline::start(&media.ffmpeg, &capture, &enc) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, monitor = %m.id, "cannot start encoder pipeline");
                continue;
            }
        };
        let tx = tx.clone();
        tasks.push(tokio::spawn(async move {
            while let Some(frame) = pipeline.next_frame().await {
                if tx.send((i, frame)).await.is_err() {
                    break;
                }
            }
            pipeline.stop().await;
        }));
    }
    tasks
}

/// Sends the stream descriptor for `plan`.
///
/// # Arguments
/// * `app` - application context.
/// * `plan` - stream plan.
async fn send_descriptor(app: &App, plan: &StreamPlan) {
    let monitors: Vec<(Size2, Rect)> = plan
        .monitors
        .iter()
        .map(|(m, s)| (*s, Rect { x: m.x, y: m.y, width: m.width as i32, height: m.height as i32 }))
        .collect();
    let d = StreamDescriptor::desktop(plan.codec, plan.framerate as i32, &monitors);
    if let Err(e) = app.channels.video.send_descriptor(&d).await {
        warn!(error = %e, "cannot send stream descriptor");
    }
}

/// Streams with `plan` until the plan becomes stale or the session ends.
///
/// # Arguments
/// * `app` - application context.
/// * `media` - encoder environment.
/// * `plan` - stream plan.
async fn stream(app: &Arc<App>, media: &MediaEnv, plan: StreamPlan) {
    info!(codec = plan.codec.display_name(), fps = plan.framerate, bitrate = plan.bitrate, monitors = plan.monitors.len(), "desktop streaming");
    app.state.streamer.send_modify(|s| {
        s.active_codec = plan.codec;
        s.can_switch_monitor = app.state.dynamic.borrow().monitors.len() > 1;
        s.can_add_monitor = false;
        s.can_remove_monitor = false;
    });
    app.settings.lock().expect("poisoned").codec_name = Some(plan.codec.display_name().to_owned());
    send_descriptor(app, &plan).await;
    let (tx, mut rx) = mpsc::channel(8);
    let tasks = start_pipelines(app, media, &plan, &tx);
    drop(tx);
    if tasks.is_empty() {
        tokio::time::sleep(Duration::from_secs(2)).await;
        return;
    }
    let mut dyn_rx = app.state.dynamic.subscribe();
    let mut mob_rx = app.state.mobile.subscribe();
    let mut user_rx = app.state.user.subscribe();
    let mut str_rx = app.state.streamer.subscribe();
    let mut frames_sent: u64 = 0;
    loop {
        tokio::select! {
            f = rx.recv() => {
                let Some((idx, frame)) = f else {
                    if frames_sent == 0 {
                        // The encoder died before producing anything (bad options, capture
                        // unavailable…): back off instead of respawning FFmpeg in a tight loop.
                        warn!("encoder pipeline produced no frames; retrying in 3 s");
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    }
                    break;
                };
                frames_sent += 1;
                if frames_sent == 1 {
                    info!(bytes = frame.data.len(), keyframe = frame.keyframe, "first video frame sent");
                }
                let res = if idx == 0 {
                    let mut header = FrameHeader::desktop(frame.capture_time, app.headset_frame(), StreamingSource::Desktop, HapticFeedback::default(), HapticFeedback::default());
                    header.encode_latency_ms = frame.encode_latency_ms;
                    app.channels.video.send_frame(header, &frame.data, plan.use_annex_b).await
                } else {
                    app.channels.video.send_secondary_frame(frame.capture_time, idx as i32, &frame.data, plan.use_annex_b).await
                };
                if res.is_err() {
                    break;
                }
            }
            _ = dyn_rx.changed() => {}
            _ = mob_rx.changed() => {}
            _ = user_rx.changed() => {}
            _ = str_rx.changed() => {}
        }
        let still_valid = app.state.dynamic.borrow().is_connected
            && app.state.streaming_source() == StreamingSource::Desktop
            && plan_matches(app, media, &plan);
        if !still_valid {
            break;
        }
    }
    for t in tasks {
        t.abort();
    }
}

/// Reports whether a freshly computed plan equals `plan` (bitrate changes under 10 % are ignored
/// to avoid restarting the CLI encoder for every bandwidth update).
///
/// # Arguments
/// * `app` - application context.
/// * `media` - encoder environment.
/// * `plan` - current plan.
///
/// # Returns
/// `true` if streaming can continue unchanged.
fn plan_matches(app: &App, media: &MediaEnv, plan: &StreamPlan) -> bool {
    let Some(new) = self::plan(app, media) else { return false };
    let bitrate_close = (f64::from(new.bitrate) - f64::from(plan.bitrate)).abs() <= f64::from(plan.bitrate) * 0.1;
    new.codec == plan.codec
        && new.framerate == plan.framerate
        && new.monitors == plan.monitors
        && new.use_annex_b == plan.use_annex_b
        && new.keyframe_generation == plan.keyframe_generation
        && bitrate_close
}

/// Desktop streaming task.
///
/// # Arguments
/// * `app` - application context.
pub async fn run(app: Arc<App>) {
    // Encoder detection runs in the background at start-up.
    let mut ready_rx = app.state.dynamic.subscribe();
    if ready_rx.wait_for(|d| d.media_ready).await.is_err() {
        return;
    }
    let Some(media) = app.media.get().cloned() else {
        warn!("desktop streaming disabled (no encoder)");
        return;
    };
    let mut dyn_rx = app.state.dynamic.subscribe();
    let mut mob_rx = app.state.mobile.subscribe();
    let mut str_rx = app.state.streamer.subscribe();
    while !app.is_shutting_down() {
        let ready = app.state.dynamic.borrow().is_connected
            && app.state.streaming_source() == StreamingSource::Desktop
            && app.state.mobile.borrow().hmd_type != vd_protocol::types::HmdType::UNKNOWN;
        if ready {
            if app.state.dynamic.borrow().monitors.is_empty() {
                let fb = {
                    let m = app.state.mobile.borrow();
                    hmd::optimal_desktop_resolution(m.hmd_type, (m.resolution.width, m.resolution.height))
                };
                let monitors = list_monitors(fb);
                app.state.dynamic.send_modify(|d| d.monitors = monitors);
            }
            if let Some(p) = plan(&app, &media) {
                stream(&app, &media, p).await;
                continue;
            }
        }
        tokio::select! {
            _ = dyn_rx.changed() => {}
            _ = mob_rx.changed() => {}
            _ = str_rx.changed() => {}
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
}
