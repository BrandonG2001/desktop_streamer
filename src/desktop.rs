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
//! * monitor count = `min(monitors, max_monitors(hmd), MonitorCount)` (`MonitorCount` 0 = all),
//!   and 1 without a hardware encoder;
//! * the headset's add/remove-monitor buttons change `MonitorCount` ([`change_monitor_count`]);
//!   beyond the physical monitors, [`crate::virtual_monitors`] adds virtual ones when the Parsec
//!   Virtual Display Driver is installed (the original's `IVirtualMonitor`);
//! * in virtual mode (`Port.VirtualMonitors`) only the virtual monitors are streamed;
//! * each monitor is scaled down (aspect preserved) to the pixel budget of
//!   `max_desktop_resolution(hmd, count)`;
//! * frame rate = `min(display refresh, SharedMobileSettings.DesktopFramerate)`;
//! * bitrate per monitor = `DesktopBitrate / count`.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use vd_media::EncodedFrame;
use vd_media::capture::{CaptureSource, CaptureSpec, Region};
use vd_media::pipeline::{Controls, VideoPipeline};
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
        Ok(_) | Err(_) => vec![MonitorInfo { id: "default".into(), index: 0, x: 0, y: 0, width: fallback.0 as u32, height: fallback.1 as u32, refresh_hz: 60, primary: true, is_virtual: false }],
    }
}

/// The monitors that are streamed, in order: the virtual ones in virtual mode (falling back to
/// the physical ones when none exist), otherwise the physical ones followed by any virtual ones.
///
/// # Arguments
/// * `app` - application context (the mode setting).
/// * `monitors` - all monitors.
///
/// # Returns
/// The streamable monitors.
pub fn streamable(app: &App, monitors: &[MonitorInfo]) -> Vec<MonitorInfo> {
    let virtual_mode = app.settings.lock().expect("poisoned").port.virtual_monitors;
    if virtual_mode && monitors.iter().any(|m| m.is_virtual) {
        return monitors.iter().filter(|m| m.is_virtual).cloned().collect();
    }
    let mut list = monitors.to_vec();
    // Stable: physical monitors keep their order and come first.
    list.sort_by_key(|m| m.is_virtual);
    list
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
    let preferred = app.settings.lock().expect("poisoned").preferred_codec;
    let hmd_type = mobile.hmd_type;
    let streamed = streamable(app, &dynamic.monitors);
    if streamed.is_empty() || !crate::virtual_monitors::settled(app) {
        return None;
    }
    let count = (monitor_count(app, media.backend.is_hardware()).1 as usize).min(streamed.len());
    let (mw, mh) = hmd::max_desktop_resolution(hmd_type, count as i32);
    let budget = i64::from(mw) * i64::from(mh);
    let mut monitors = Vec::with_capacity(count);
    for i in 0..count {
        let m = streamed[(dynamic.monitor_start_index + i) % streamed.len()].clone();
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

/// Monitors that can be streamed and how many are, from `MonitorCount`.
/// C#: `MonitorCount = HasHardwareEncoder ? min(MonitorCount, MaxMonitors(hmd)) : 1`, with
/// 0 filled in as "all monitors" (`DynamicSettings.Monitors` setter).  With the virtual display
/// driver the maximum is the headset's, as monitors can be added.
///
/// # Arguments
/// * `app` - application context.
/// * `hardware` - a hardware encoder is in use (multi-monitor needs one).
///
/// # Returns
/// `(maximum, current)`, both at least 1.
pub fn monitor_count(app: &App, hardware: bool) -> (i32, i32) {
    let streamed = streamable(app, &app.state.dynamic.borrow().monitors).len() as i32;
    let cap = if hardware { hmd::max_monitors(app.state.mobile.borrow().hmd_type) } else { 1 };
    let growable = app.virtual_available.load(Ordering::Relaxed);
    let max = if growable { cap } else { cap.min(streamed) }.max(1);
    let (wanted, virtual_mode) = {
        let s = app.settings.lock().expect("poisoned");
        (s.monitor_count, s.port.virtual_monitors)
    };
    // "All": every physical monitor, or as many virtual ones as the headset shows.
    let all = if virtual_mode && growable { cap } else { cap.min(streamed) };
    let current = if wanted <= 0 { all } else { wanted.min(max) };
    (max, current.max(1))
}

/// Handles the headset's add/remove-monitor buttons (C#: `DesktopStreamer._0005()` /
/// `_0006()`): changes `MonitorCount` and restarts the stream with the new layout.
///
/// # Arguments
/// * `app` - application context.
/// * `delta` - `+1` (AddMonitor) or `-1` (RemoveMonitor).
pub fn change_monitor_count(app: &App, delta: i32) {
    let hardware = app.media.get().is_some_and(|m| m.backend.is_hardware());
    let (max, current) = monitor_count(app, hardware);
    let new = (current + delta).clamp(1, max);
    if new == current {
        if delta > 0 {
            if app.virtual_available.load(Ordering::Relaxed) {
                warn!(max, "cannot add a monitor: the headset shows at most this many");
            } else {
                warn!(max, "cannot add a monitor: all physical monitors are streamed (install the Parsec Virtual Display Driver for more)");
            }
        }
        return;
    }
    info!(from = current, to = new, "monitor count changed by the headset");
    app.settings.lock().expect("poisoned").monitor_count = new;
    app.save_settings();
    // Wake the streaming loop so it re-plans with the new monitor count.
    app.state.dynamic.send_modify(|_| {});
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

/// A running encoder pipeline: its task and the signal that stops it.
struct Pipeline {
    task: tokio::task::JoinHandle<()>,
    stop: tokio::sync::oneshot::Sender<()>,
    /// Key-frame / bitrate controls (in-process encoder only).
    controls: Controls,
}

/// Output of a pipeline task: a frame, or `None` when its encoder process ended.
type PipelineEvent = (usize, Option<EncodedFrame>);

/// Starts one pipeline per monitor and forwards frames to `tx` tagged with the monitor index.
///
/// # Arguments
/// * `app` - application context.
/// * `media` - encoder environment.
/// * `plan` - stream plan.
/// * `tx` - event sink.
///
/// # Returns
/// The started pipelines (stopped with [`stop_pipelines`]).
fn start_pipelines(app: &App, media: &MediaEnv, plan: &StreamPlan, tx: &mpsc::Sender<PipelineEvent>) -> Vec<Pipeline> {
    let (gop_secs, render_node, aq, two_pass) = {
        let s = app.settings.lock().expect("poisoned");
        (s.port.keyframe_interval_seconds.max(1), s.port.render_node.clone(), s.enable_aq, s.enable_2pass)
    };
    let mut pipelines = Vec::new();
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
        let mut pipeline = match VideoPipeline::start(&media.ffmpeg, &capture, &enc) {
            Ok(p) => {
                info!(monitor = %m.id, backend = p.kind(), "encoder pipeline started");
                if i == 0 {
                    *app.stats.encoder.lock().expect("poisoned") = p.kind();
                }
                p
            }
            Err(e) => {
                warn!(error = %e, monitor = %m.id, "cannot start encoder pipeline");
                continue;
            }
        };
        let tx = tx.clone();
        let controls = pipeline.controls();
        let (stop, mut stop_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    f = pipeline.next_frame() => {
                        let ended = f.is_none();
                        if tx.send((i, f)).await.is_err() || ended {
                            break;
                        }
                    }
                    _ = &mut stop_rx => break,
                }
            }
            // Wait for the process to exit so the screen capture is released before a restart.
            pipeline.stop().await;
        });
        pipelines.push(Pipeline { task, stop, controls });
    }
    pipelines
}

/// Stops pipelines and waits until their encoder processes have exited (otherwise a restarted
/// ddagrab fails with "Device or resource busy" while the old process still holds the output).
///
/// # Arguments
/// * `pipelines` - pipelines to stop.
async fn stop_pipelines(pipelines: Vec<Pipeline>) {
    let tasks: Vec<_> = pipelines
        .into_iter()
        .map(|p| {
            let _ = p.stop.send(());
            p.task
        })
        .collect();
    for t in tasks {
        let _ = tokio::time::timeout(Duration::from_secs(3), t).await;
    }
}

/// Frames a lagging monitor may fall behind before the others are sent without it.
const MAX_MONITOR_SKEW: usize = 4;

/// Groups frames of all monitors into ticks, like the original's `DesktopStreamer.Update()`:
/// every tick sends one frame per monitor, and all of them carry the timestamp of monitor 0's
/// frame.  The headset pairs secondary-monitor frames with the primary frame by timestamp;
/// independent timestamps per encoder stall its video pipeline (frozen screens).
#[derive(Debug)]
struct Lockstep {
    queues: Vec<std::collections::VecDeque<EncodedFrame>>,
}

impl Lockstep {
    /// Creates the grouper.
    ///
    /// # Arguments
    /// * `monitors` - number of monitors.
    ///
    /// # Returns
    /// An empty grouper.
    fn new(monitors: usize) -> Self {
        Self { queues: (0..monitors).map(|_| std::collections::VecDeque::new()).collect() }
    }

    /// Queues a frame of monitor `idx`.
    ///
    /// # Arguments
    /// * `idx` - monitor index.
    /// * `frame` - encoded frame.
    fn push(&mut self, idx: usize, frame: EncodedFrame) {
        if let Some(q) = self.queues.get_mut(idx) {
            q.push_back(frame);
        }
    }

    /// Takes the next tick, if one is complete.
    ///
    /// A tick is complete when every monitor has a frame.  If a secondary monitor falls more than
    /// [`MAX_MONITOR_SKEW`] frames behind monitor 0, the tick is sent without it so one slow
    /// encoder cannot freeze the others.
    ///
    /// # Returns
    /// `(monitor index, frame)` pairs, monitor 0 first, all to be sent with monitor 0's timestamp.
    fn next_tick(&mut self) -> Option<Vec<(usize, EncodedFrame)>> {
        let primary_ready = !self.queues.first()?.is_empty();
        let all_ready = self.queues.iter().all(|q| !q.is_empty());
        let lagging = primary_ready && self.queues[0].len() > MAX_MONITOR_SKEW;
        if !(all_ready || lagging) {
            return None;
        }
        Some(self.queues.iter_mut().enumerate().filter_map(|(i, q)| q.pop_front().map(|f| (i, f))).collect())
    }
}

/// Sends the stream descriptor for `plan` unless the headset already has the same one.
///
/// The original sends the descriptor only when its encoders are (re)created
/// (`DesktopStreamer` initialisation); a key-frame request (`RestartStreaming` →
/// `ForceKeyFrame`) does not send it.  This port restarts FFmpeg to get a key frame, but must not
/// re-send an identical descriptor: the headset treats it as a new stream and rebuilds the scene,
/// resetting the environment to the black void (e.g. after recentring with the Meta button).
///
/// # Arguments
/// * `app` - application context.
/// * `plan` - stream plan.
/// * `last` - descriptor last sent in this session (`None` at the start of a session).
async fn send_descriptor(app: &App, plan: &StreamPlan, last: &mut Option<StreamDescriptor>) {
    let monitors: Vec<(Size2, Rect)> = plan
        .monitors
        .iter()
        .map(|(m, s)| (*s, Rect { x: m.x, y: m.y, width: m.width as i32, height: m.height as i32 }))
        .collect();
    let d = StreamDescriptor::desktop(plan.codec, plan.framerate as i32, &monitors);
    if last.as_ref() == Some(&d) {
        debug!("stream parameters unchanged; not re-sending the stream descriptor");
        return;
    }
    match app.channels.video.send_descriptor(&d).await {
        Ok(()) => *last = Some(d),
        Err(e) => warn!(error = %e, "cannot send stream descriptor"),
    }
}

/// Sends one tick: monitor 0 with a full frame header, the others as secondary frames with the
/// same timestamp.
///
/// # Arguments
/// * `app` - application context.
/// * `plan` - stream plan.
/// * `tick` - frames of the tick, monitor 0 first (if present).
///
/// # Errors
/// Video channel send errors.
async fn send_tick(app: &App, plan: &StreamPlan, tick: Vec<(usize, EncodedFrame)>) -> vd_net::error::Result<()> {
    let timestamp = tick.first().map(|(_, f)| f.capture_time).unwrap_or_else(vd_media::now_ticks);
    app.stats.frames.fetch_add(1, Ordering::Relaxed);
    for (idx, frame) in tick {
        app.stats.bytes.fetch_add(frame.data.len() as u64, Ordering::Relaxed);
        if idx == 0 {
            app.stats.encode_us.store((frame.encode_latency_ms * 1000.0) as u32, Ordering::Relaxed);
            let mut header = FrameHeader::desktop(timestamp, app.headset_frame(), StreamingSource::Desktop, HapticFeedback::default(), HapticFeedback::default());
            header.encode_latency_ms = frame.encode_latency_ms;
            app.channels.video.send_frame(header, &frame.data, plan.use_annex_b).await?;
        } else {
            app.channels.video.send_secondary_frame(timestamp, idx as i32, &frame.data).await?;
        }
    }
    Ok(())
}

/// Streams with `plan` until the plan becomes stale or the session ends.
///
/// # Arguments
/// * `app` - application context.
/// * `media` - encoder environment.
/// * `plan` - stream plan.
/// * `last_descriptor` - descriptor last sent in this session (see [`send_descriptor`]).
async fn stream(app: &Arc<App>, media: &MediaEnv, mut plan: StreamPlan, last_descriptor: &mut Option<StreamDescriptor>) {
    info!(codec = plan.codec.display_name(), fps = plan.framerate, bitrate = plan.bitrate, monitors = plan.monitors.len(), "desktop streaming");
    app.state.streamer.send_modify(|s| {
        s.active_codec = plan.codec;
        s.can_switch_monitor = streamable(app, &app.state.dynamic.borrow().monitors).len() > 1;
        // C#: DesktopStreamer._0008() — buttons only with a hardware encoder; adding is limited to
        // physical monitors here.
        let hardware = media.backend.is_hardware();
        let (max, current) = monitor_count(app, hardware);
        s.can_add_monitor = hardware && current < max;
        s.can_remove_monitor = hardware && current > 1;
    });
    app.settings.lock().expect("poisoned").codec_name = Some(plan.codec.display_name().to_owned());
    send_descriptor(app, &plan, last_descriptor).await;
    let (tx, mut rx) = mpsc::channel::<PipelineEvent>(8 * plan.monitors.len().max(1));
    let pipelines = start_pipelines(app, media, &plan, &tx);
    drop(tx);
    if pipelines.len() != plan.monitors.len() {
        stop_pipelines(pipelines).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        return;
    }
    let mut dyn_rx = app.state.dynamic.subscribe();
    let mut mob_rx = app.state.mobile.subscribe();
    let mut user_rx = app.state.user.subscribe();
    let mut str_rx = app.state.streamer.subscribe();
    let mut lockstep = Lockstep::new(plan.monitors.len());
    let started = std::time::Instant::now();
    let mut frames_sent: u64 = 0;
    'session: loop {
        tokio::select! {
            ev = rx.recv() => {
                let Some((idx, Some(frame))) = ev else {
                    // One encoder exited: restart them all so no monitor stays frozen.
                    if frames_sent == 0 {
                        // The encoder died before producing anything (bad options, or the screen
                        // capture still held by a just-stopped process): back off briefly instead
                        // of respawning FFmpeg in a tight loop.
                        warn!("encoder pipeline produced no frames; retrying in 1 s");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    } else {
                        warn!(monitor = ev.map(|(i, _)| i), "encoder pipeline stopped; restarting desktop streaming");
                    }
                    break;
                };
                lockstep.push(idx, frame);
                while let Some(tick) = lockstep.next_tick() {
                    frames_sent += 1;
                    if frames_sent == 1 {
                        info!(bytes = tick[0].1.data.len(), keyframe = tick[0].1.keyframe, "first video frame sent");
                    }
                    if send_tick(app, &plan, tick).await.is_err() {
                        break 'session;
                    }
                }
            }
            _ = dyn_rx.changed() => {}
            _ = mob_rx.changed() => {}
            _ = user_rx.changed() => {}
            _ = str_rx.changed() => {}
        }
        if !app.state.dynamic.borrow().is_connected || app.state.streaming_source() != StreamingSource::Desktop {
            break;
        }
        let change = plan_change(app, media, &plan, &pipelines, started);
        if change.restart {
            break;
        }
        // In-process encoders take key frames and bitrate changes without a restart (C#:
        // ForceKeyFrame / nvEncReconfigureEncoder); otherwise restart the pipelines.
        if let Some(bitrate) = change.bitrate {
            if !pipelines.iter().all(|p| p.controls.set_bitrate(bitrate)) {
                break;
            }
            info!(from = plan.bitrate, to = bitrate, "encoder bitrate changed");
            plan.bitrate = bitrate;
        }
        if let Some(generation) = change.keyframe {
            if !pipelines.iter().all(|p| p.controls.force_keyframe()) {
                break;
            }
            debug!("key frame requested");
            plan.keyframe_generation = generation;
        }
    }
    stop_pipelines(pipelines).await;
}

/// How the current plan must change.
#[derive(Debug, Default, PartialEq, Eq)]
struct PlanChange {
    /// Codec, frame rate, monitors or bitstream format changed: restart.
    restart: bool,
    /// New per-monitor bitrate to apply.
    bitrate: Option<u32>,
    /// A key frame was requested (new `keyframe_requests` generation).
    keyframe: Option<u64>,
}

/// Minimum time a stream runs before a higher bitrate restarts CLI encoders.
const BITRATE_RAISE_INTERVAL: Duration = Duration::from_secs(30);

/// Compares a freshly computed plan with the current one.
///
/// Bitrate changes are applied live when every pipeline supports it (any change over 5 %).
/// CLI pipelines restart for each change (a visible hitch), so for them the bitrate the headset
/// ramps up while measuring bandwidth only changes for big drops (congestion) or for a big
/// increase at most every [`BITRATE_RAISE_INTERVAL`].
///
/// # Arguments
/// * `app` - application context.
/// * `media` - encoder environment.
/// * `plan` - current plan.
/// * `pipelines` - running pipelines (their capabilities).
/// * `started` - when streaming with `plan` began.
///
/// # Returns
/// What has to change.
fn plan_change(app: &App, media: &MediaEnv, plan: &StreamPlan, pipelines: &[Pipeline], started: std::time::Instant) -> PlanChange {
    let Some(new) = self::plan(app, media) else { return PlanChange { restart: true, ..PlanChange::default() } };
    let restart = new.codec != plan.codec || new.framerate != plan.framerate || new.monitors != plan.monitors || new.use_annex_b != plan.use_annex_b;
    let live = pipelines.iter().all(|p| !matches!(p.controls, Controls::None));
    let (old, now) = (f64::from(plan.bitrate), f64::from(new.bitrate));
    let bitrate_changed = if live {
        (now - old).abs() > old * 0.05
    } else if now < old {
        now < old * 0.75
    } else {
        now > old * 1.5 && started.elapsed() >= BITRATE_RAISE_INTERVAL
    };
    PlanChange {
        restart,
        bitrate: bitrate_changed.then_some(new.bitrate),
        keyframe: (new.keyframe_generation != plan.keyframe_generation).then_some(new.keyframe_generation),
    }
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
    let mut last_descriptor: Option<StreamDescriptor> = None;
    while !app.is_shutting_down() {
        let ready = app.state.dynamic.borrow().is_connected
            && app.state.streaming_source() == StreamingSource::Desktop
            && app.state.mobile.borrow().hmd_type != vd_protocol::types::HmdType::UNKNOWN;
        if !ready {
            // New session, or the video/VR streamer sent its own descriptor: send ours again.
            last_descriptor = None;
        }
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
                stream(&app, &media, p, &mut last_descriptor).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(t: i64) -> EncodedFrame {
        EncodedFrame { data: vec![0, 0, 0, 1], keyframe: false, capture_time: t, encode_latency_ms: 0.0 }
    }

    #[test]
    fn ticks_wait_for_every_monitor() {
        let mut l = Lockstep::new(3);
        l.push(0, frame(10));
        l.push(1, frame(11));
        assert!(l.next_tick().is_none(), "monitor 2 has no frame yet");
        l.push(2, frame(12));
        let tick = l.next_tick().unwrap();
        assert_eq!(tick.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [0, 1, 2]);
        assert!(l.next_tick().is_none());
    }

    #[test]
    fn a_lagging_monitor_does_not_freeze_the_others() {
        let mut l = Lockstep::new(2);
        for t in 0..=MAX_MONITOR_SKEW as i64 {
            l.push(0, frame(t));
        }
        let tick = l.next_tick().unwrap();
        assert_eq!(tick.len(), 1, "sent without the lagging monitor");
        assert_eq!(tick[0].0, 0);
    }
}
