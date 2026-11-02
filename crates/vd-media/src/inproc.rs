//! In-process capture + encode pipeline through the libav* C API (feature `ffmpeg-lib`).
//!
//! The CLI backend ([`crate::cli`]) runs one `ffmpeg` process per monitor and can only change
//! anything by restarting it.  This pipeline runs the *same* capture filter graph (`ddagrab`,
//! plus the same scaling / format filters) and the *same* encoder with the same options inside
//! the streamer, which gives what the original's direct NVENC/AMF code had:
//!
//! * **on-demand key frames** — `DynamicSettings.ForceKeyFrame` marks the next frame as an
//!   I-frame (`AVFrame.pict_type`; with `forced-idr` NVENC emits an IDR) instead of restarting;
//! * **live bitrate changes** — `AVCodecContext.bit_rate` is updated in place; NVENC and QSV
//!   reconfigure the running encoder (`nvEncReconfigureEncoder` / `MFXVideoENCODE_Reset`);
//! * **real timestamps** — the capture time is taken when the frame leaves the capture graph and
//!   the encoder latency is measured, instead of being estimated from the CLI pipe.
//!
//! Frames stay on the GPU for `ddagrab` → NVENC/AMF/QSV (the encoder receives the D3D11 frames
//! context of the graph).  Only filter-graph sources are supported (`ddagrab`, the test pattern);
//! other captures, and builds without the feature, use the CLI backend.
//!
//! **FFmpeg upgrades:** `ffmpeg-sys-next`'s major version follows FFmpeg's (9.x ↔ FFmpeg 9);
//! bump it together with `packaging/windows/fetch-ffmpeg.ps1`.  This file is the only code that
//! touches the libav* C API.

use std::ffi::{CStr, CString, c_int};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use ffmpeg_sys_next as ff;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::capture::{CaptureSource, CaptureSpec};
use crate::codec::{Backend, EncoderConfig};
use crate::error::{MediaError, Result};
use crate::version::FfmpegVersion;
use crate::{EncodedFrame, now_ticks};

/// `AVERROR(EAGAIN)` (`EAGAIN` is 11 on Windows and Linux).
const AVERROR_EAGAIN: c_int = -11;
/// `AVERROR_EOF` = `-MKTAG('E','O','F',' ')`.
const AVERROR_EOF: c_int = -(0x45 | (0x4F << 8) | (0x46 << 16) | (0x20 << 24));
/// AV1 temporal delimiter OBU (`obu_type = 2`, `obu_has_size_field = 1`, size 0), as written by
/// FFmpeg's `obu` muxer before every temporal unit.
const AV1_TEMPORAL_DELIMITER: [u8; 2] = [0x12, 0x00];

/// Describes a libav* error code.
///
/// # Arguments
/// * `code` - negative `AVERROR` value.
///
/// # Returns
/// The message.
fn av_err(code: c_int) -> String {
    let mut buf = [0 as std::ffi::c_char; 256];
    // SAFETY: the buffer and its length are valid; av_strerror NUL-terminates.
    unsafe {
        ff::av_strerror(code, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

/// Converts a negative return code into an error.
///
/// # Arguments
/// * `code` - libav* return value.
/// * `what` - call description.
///
/// # Returns
/// `code` when non-negative.
///
/// # Errors
/// [`MediaError::Libav`] for negative codes.
fn check(code: c_int, what: &str) -> Result<c_int> {
    if code < 0 { Err(MediaError::Libav(format!("{what}: {}", av_err(code)))) } else { Ok(code) }
}

/// Version of the linked FFmpeg libraries (decides option spellings, like the CLI's banner).
///
/// # Returns
/// Parsed from `av_version_info()` (`n9.0.2-…`, `9.0`, …); FFmpeg 9.0 if unparsable.
pub fn linked_version() -> FfmpegVersion {
    // SAFETY: av_version_info returns a static NUL-terminated string.
    let info = unsafe { CStr::from_ptr(ff::av_version_info()) }.to_string_lossy().into_owned();
    let digits = info.trim_start_matches(|c: char| !c.is_ascii_digit());
    let mut it = digits.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse::<u32>().ok());
    match (it.next(), it.next()) {
        (Some(major), Some(minor)) => FfmpegVersion { major, minor },
        _ => FfmpegVersion { major: 9, minor: 0 },
    }
}

/// Whether in-process capture is possible for a source.
///
/// # Arguments
/// * `source` - capture source.
///
/// # Returns
/// `true` for filter-graph sources (`ddagrab`, test pattern).
pub fn supports(source: &CaptureSource) -> bool {
    matches!(source, CaptureSource::DdaGrab { .. } | CaptureSource::TestPattern { .. })
}

/// Filter-graph description for a capture (source filter + the CLI backend's filter chain).
///
/// # Arguments
/// * `capture` - capture request.
/// * `enc` - encoder configuration (decides GPU vs CPU frames and the pixel format).
///
/// # Returns
/// The graph description, or `None` for sources that are not filters.
fn graph_description(capture: &CaptureSpec, enc: &EncoderConfig) -> Option<String> {
    let fr = capture.framerate.max(1);
    let source = match &capture.source {
        CaptureSource::DdaGrab { output_index } => {
            format!("ddagrab=output_idx={output_index}:framerate={fr}:draw_mouse={}", u8::from(capture.draw_cursor))
        }
        // `realtime` paces the synthetic source like `-re` does on the command line.
        CaptureSource::TestPattern { width, height } => format!("testsrc2=size={width}x{height}:rate={fr},realtime"),
        _ => return None,
    };
    let filter = capture.ffmpeg_args(enc.backend, enc.pixel_format(), &enc.render_node).filter;
    Some(if filter.is_empty() { source } else { format!("{source},{filter}") })
}

/// Encoder private options from the CLI arguments of [`EncoderConfig::encoder_args`], so both
/// backends use identical tuning.  Generic options handled through `AVCodecContext` fields are
/// skipped.
///
/// # Arguments
/// * `args` - `-key value` pairs.
///
/// # Returns
/// `(key, value)` pairs for an `AVDictionary`.
fn private_options(args: &[String]) -> Vec<(String, String)> {
    const FIELDS: [&str; 7] = ["-c:v", "-g", "-bf", "-b:v", "-maxrate", "-bufsize", "-pix_fmt"];
    args.as_chunks::<2>()
        .0
        .iter()
        .filter(|kv| !FIELDS.contains(&kv[0].as_str()))
        .map(|kv| {
            let key = kv[0].trim_start_matches('-');
            let key = key.strip_suffix(":v").unwrap_or(key);
            (key.to_owned(), kv[1].clone())
        })
        .collect()
}

/// Run-time controls shared between the pipeline handle and its thread.
#[derive(Debug, Default)]
struct Shared {
    stop: AtomicBool,
    force_keyframe: AtomicBool,
    bitrate: AtomicU32,
}

/// Controls of a running in-process pipeline (cheap to clone).
#[derive(Debug, Clone)]
pub struct Controller {
    shared: Arc<Shared>,
    dynamic_bitrate: bool,
}

impl Controller {
    /// Makes the next frame a key frame (C#: `DynamicSettings.ForceKeyFrame = true`).
    pub fn force_keyframe(&self) {
        self.shared.force_keyframe.store(true, Ordering::Relaxed);
    }

    /// Changes the target bitrate of the running encoder.
    ///
    /// # Arguments
    /// * `bps` - new bitrate in bit/s.
    ///
    /// # Returns
    /// `false` when the encoder cannot change bitrate while running (AMF); restart it instead.
    pub fn set_bitrate(&self, bps: u32) -> bool {
        if self.dynamic_bitrate {
            self.shared.bitrate.store(bps, Ordering::Relaxed);
        }
        self.dynamic_bitrate
    }
}

/// libav* objects owned by the pipeline thread.
struct Native {
    graph: *mut ff::AVFilterGraph,
    sink: *mut ff::AVFilterContext,
    codec: *mut ff::AVCodecContext,
    frame: *mut ff::AVFrame,
    packet: *mut ff::AVPacket,
}

impl Drop for Native {
    /// Frees every libav* object.
    fn drop(&mut self) {
        // SAFETY: each pointer is either null or owned by this struct; the free functions accept
        // pointers to null.
        unsafe {
            ff::av_packet_free(&mut self.packet);
            ff::av_frame_free(&mut self.frame);
            ff::avcodec_free_context(&mut self.codec);
            ff::avfilter_graph_free(&mut self.graph);
        }
    }
}

impl Native {
    /// Builds the capture graph and opens the encoder.
    ///
    /// # Arguments
    /// * `desc` - filter-graph description ending in one unconnected output.
    /// * `enc` - encoder configuration.
    /// * `version` - FFmpeg version (option spellings).
    ///
    /// # Returns
    /// The opened objects.
    ///
    /// # Errors
    /// [`MediaError::Libav`] when a filter or the encoder cannot be created.
    fn open(desc: &str, enc: &EncoderConfig, version: FfmpegVersion) -> Result<Self> {
        let name = enc.backend.encoder_name(enc.codec).ok_or_else(|| MediaError::Libav("no encoder for this codec".into()))?;
        // SAFETY: standard libavfilter/libavcodec set-up; every allocation is checked and owned
        // by `me`, whose Drop frees it on any early return.
        unsafe {
            let mut me = Self { graph: ff::avfilter_graph_alloc(), sink: ptr::null_mut(), codec: ptr::null_mut(), frame: ff::av_frame_alloc(), packet: ff::av_packet_alloc() };
            if me.graph.is_null() || me.frame.is_null() || me.packet.is_null() {
                return Err(MediaError::Libav("out of memory".into()));
            }
            // Capture graph: "<source>,<filters>" -> buffersink.
            let sink_name = CString::new("out").expect("static");
            check(
                ff::avfilter_graph_create_filter(&mut me.sink, ff::avfilter_get_by_name(c"buffersink".as_ptr()), sink_name.as_ptr(), ptr::null(), ptr::null_mut(), me.graph),
                "buffersink",
            )?;
            let cdesc = CString::new(desc).map_err(|_| MediaError::Libav("invalid filter description".into()))?;
            let mut inputs: *mut ff::AVFilterInOut = ptr::null_mut();
            let mut outputs: *mut ff::AVFilterInOut = ptr::null_mut();
            let parsed = ff::avfilter_graph_parse2(me.graph, cdesc.as_ptr(), &mut inputs, &mut outputs);
            ff::avfilter_inout_free(&mut inputs);
            if parsed < 0 || outputs.is_null() {
                ff::avfilter_inout_free(&mut outputs);
                check(parsed.min(-1), &format!("filter graph `{desc}`"))?;
            }
            let linked = ff::avfilter_link((*outputs).filter_ctx, (*outputs).pad_idx as u32, me.sink, 0);
            ff::avfilter_inout_free(&mut outputs);
            check(linked, "link buffersink")?;
            check(ff::avfilter_graph_config(me.graph, ptr::null_mut()), "configure filter graph")?;

            // Encoder fed with the graph's output (GPU frames context when it has one).
            let cname = CString::new(name).expect("static");
            let codec = ff::avcodec_find_encoder_by_name(cname.as_ptr());
            if codec.is_null() {
                return Err(MediaError::Libav(format!("encoder {name} not available")));
            }
            me.codec = ff::avcodec_alloc_context3(codec);
            if me.codec.is_null() {
                return Err(MediaError::Libav("out of memory".into()));
            }
            let c = &mut *me.codec;
            let fps = enc.framerate.max(1) as c_int;
            c.width = ff::av_buffersink_get_w(me.sink);
            c.height = ff::av_buffersink_get_h(me.sink);
            c.time_base = ff::AVRational { num: 1, den: fps };
            c.framerate = ff::AVRational { num: fps, den: 1 };
            c.pix_fmt = std::mem::transmute::<c_int, ff::AVPixelFormat>(ff::av_buffersink_get_format(me.sink));
            c.gop_size = enc.gop.max(1) as c_int;
            c.max_b_frames = 0;
            let bitrate = i64::from(enc.bitrate);
            c.bit_rate = bitrate;
            c.rc_max_rate = bitrate;
            // One-frame VBV like the original (vbvBufferSize = averageBitRate / fps).
            c.rc_buffer_size = (bitrate / i64::from(fps)).max(1) as c_int;
            c.flags |= ff::AV_CODEC_FLAG_LOW_DELAY as c_int;
            let hw = ff::av_buffersink_get_hw_frames_ctx(me.sink);
            if !hw.is_null() {
                c.hw_frames_ctx = ff::av_buffer_ref(hw);
            }
            let mut opts: *mut ff::AVDictionary = ptr::null_mut();
            for (k, v) in private_options(&enc.encoder_args(version)) {
                let (ck, cv) = (CString::new(k).expect("option"), CString::new(v).expect("value"));
                ff::av_dict_set(&mut opts, ck.as_ptr(), cv.as_ptr(), 0);
            }
            let opened = ff::avcodec_open2(me.codec, codec, &mut opts);
            let mut unused = ptr::null();
            loop {
                let e = ff::av_dict_get(opts, c"".as_ptr(), unused, ff::AV_DICT_IGNORE_SUFFIX as c_int);
                if e.is_null() {
                    break;
                }
                debug!(option = %CStr::from_ptr((*e).key).to_string_lossy(), encoder = name, "encoder option not used");
                unused = e;
            }
            ff::av_dict_free(&mut opts);
            check(opened, &format!("open {name}"))?;
            Ok(me)
        }
    }
}

/// Pipeline thread: pulls frames from the graph, encodes them, delivers packets.
///
/// # Arguments
/// * `native` - opened graph and encoder.
/// * `enc` - encoder configuration.
/// * `shared` - run-time controls.
/// * `tx` - encoded-frame sink.
fn run(native: Native, enc: EncoderConfig, shared: Arc<Shared>, tx: mpsc::Sender<EncodedFrame>) {
    let av1 = enc.codec.is_av1();
    let mut bitrate = enc.bitrate;
    let mut pts: i64 = 0;
    let mut pending: std::collections::VecDeque<i64> = std::collections::VecDeque::new();
    // SAFETY: the pointers in `native` are valid for the whole loop and only used on this thread.
    unsafe {
        while !shared.stop.load(Ordering::Relaxed) {
            let got = ff::av_buffersink_get_frame(native.sink, native.frame);
            if got == AVERROR_EAGAIN {
                continue;
            }
            if got < 0 {
                if got != AVERROR_EOF {
                    warn!(error = %av_err(got), "capture graph stopped");
                }
                break;
            }
            let capture_time = now_ticks();
            (*native.frame).pts = pts;
            pts += 1;
            if shared.force_keyframe.swap(false, Ordering::Relaxed) {
                (*native.frame).pict_type = ff::AVPictureType::AV_PICTURE_TYPE_I;
            } else {
                (*native.frame).pict_type = ff::AVPictureType::AV_PICTURE_TYPE_NONE;
            }
            let wanted = shared.bitrate.load(Ordering::Relaxed);
            if wanted != 0 && wanted != bitrate {
                // NVENC / QSV pick this up on the next frame and reconfigure in place.
                bitrate = wanted;
                let c = &mut *native.codec;
                c.bit_rate = i64::from(bitrate);
                c.rc_max_rate = i64::from(bitrate);
                c.rc_buffer_size = (i64::from(bitrate) / i64::from(enc.framerate.max(1))).max(1) as c_int;
                debug!(bitrate, "encoder bitrate changed in place");
            }
            pending.push_back(capture_time);
            let sent = ff::avcodec_send_frame(native.codec, native.frame);
            ff::av_frame_unref(native.frame);
            if sent < 0 && sent != AVERROR_EAGAIN {
                warn!(error = %av_err(sent), "encoder rejected a frame");
                break;
            }
            loop {
                let r = ff::avcodec_receive_packet(native.codec, native.packet);
                if r == AVERROR_EAGAIN || r == AVERROR_EOF {
                    break;
                }
                if r < 0 {
                    warn!(error = %av_err(r), "encoder failed");
                    return;
                }
                let p = &*native.packet;
                let bytes = std::slice::from_raw_parts(p.data, p.size.max(0) as usize);
                let mut data = Vec::with_capacity(bytes.len() + 2);
                if av1 && bytes.first().is_none_or(|b| (b >> 3) & 0x0F != crate::obu::OBU_TEMPORAL_DELIMITER) {
                    data.extend_from_slice(&AV1_TEMPORAL_DELIMITER);
                }
                data.extend_from_slice(bytes);
                let keyframe = p.flags & ff::AV_PKT_FLAG_KEY as c_int != 0;
                ff::av_packet_unref(native.packet);
                // Low-delay encoders return one packet per frame, in order.
                let captured = pending.pop_front().unwrap_or(capture_time);
                let latency_ms = (now_ticks() - captured) as f32 / 10_000.0;
                if tx.blocking_send(EncodedFrame { data, keyframe, capture_time: captured, encode_latency_ms: latency_ms }).is_err() {
                    return;
                }
            }
        }
    }
    debug!("in-process encoder stopped");
}

/// A running in-process capture + encode pipeline.
pub struct InprocVideoPipeline {
    frames: mpsc::Receiver<EncodedFrame>,
    controller: Controller,
    thread: Option<JoinHandle<()>>,
}

impl InprocVideoPipeline {
    /// Builds the graph, opens the encoder and starts encoding on a dedicated thread.
    ///
    /// # Arguments
    /// * `version` - FFmpeg version of the linked libraries' option set.
    /// * `capture` - capture request (must satisfy [`supports`]).
    /// * `enc` - encoder configuration.
    ///
    /// # Returns
    /// The running pipeline.
    ///
    /// # Errors
    /// [`MediaError::Libav`] if the source is unsupported or the graph / encoder cannot be opened.
    pub fn start(version: FfmpegVersion, capture: &CaptureSpec, enc: &EncoderConfig) -> Result<Self> {
        let desc = graph_description(capture, enc).ok_or_else(|| MediaError::Libav("capture source needs the CLI backend".into()))?;
        // Like `-loglevel error` on the command line: libav* messages go to stderr otherwise.
        static QUIET: std::sync::Once = std::sync::Once::new();
        // SAFETY: setting a global log level.
        QUIET.call_once(|| unsafe { ff::av_log_set_level(ff::AV_LOG_ERROR as c_int) });
        info!(graph = %desc, encoder = enc.backend.encoder_name(enc.codec).unwrap_or("?"), "starting in-process encoder pipeline");
        let shared = Arc::new(Shared::default());
        let (tx, frames) = mpsc::channel(4);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (thread_shared, thread_enc) = (Arc::clone(&shared), enc.clone());
        let thread = std::thread::Builder::new().name("Encoder Thread".into()).spawn(move || match Native::open(&desc, &thread_enc, version) {
            Ok(native) => {
                let _ = ready_tx.send(Ok(()));
                run(native, thread_enc, thread_shared, tx);
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        })?;
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                shared.stop.store(true, Ordering::Relaxed);
                return Err(MediaError::Libav("encoder start-up timed out".into()));
            }
        }
        // NVENC and QSV reconfigure the bitrate of a running session; AMF needs a restart.
        let dynamic_bitrate = matches!(enc.backend, Backend::Nvenc | Backend::Qsv);
        Ok(Self { frames, controller: Controller { shared, dynamic_bitrate }, thread: Some(thread) })
    }

    /// Run-time controls (key frames, bitrate).
    ///
    /// # Returns
    /// A cloneable controller.
    pub fn controller(&self) -> Controller {
        self.controller.clone()
    }

    /// Waits for the next encoded frame.
    ///
    /// # Returns
    /// The frame, or `None` when the pipeline stopped.
    pub async fn next_frame(&mut self) -> Option<EncodedFrame> {
        self.frames.recv().await
    }

    /// Stops the thread and releases the capture and encoder.
    pub async fn stop(&mut self) {
        self.controller.shared.stop.store(true, Ordering::Relaxed);
        // Unblock a thread waiting to deliver a frame.
        self.frames.close();
        while self.frames.try_recv().is_ok() {}
        if let Some(t) = self.thread.take() {
            let _ = tokio::task::spawn_blocking(move || t.join()).await;
        }
    }
}

impl Drop for InprocVideoPipeline {
    /// Signals the thread to stop (it exits after the frame it is waiting for).
    fn drop(&mut self) {
        self.controller.shared.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_options_skip_context_fields() {
        let args: Vec<String> =
            ["-c:v", "hevc_nvenc", "-g", "60", "-b:v", "1", "-preset", "p1", "-profile:v", "main10", "-pix_fmt", "p010le"].iter().map(|s| s.to_string()).collect();
        assert_eq!(private_options(&args), [("preset".to_owned(), "p1".to_owned()), ("profile".to_owned(), "main10".to_owned())]);
    }

    #[test]
    fn only_filter_sources_run_in_process() {
        assert!(supports(&CaptureSource::DdaGrab { output_index: 0 }));
        assert!(supports(&CaptureSource::TestPattern { width: 64, height: 64 }));
        assert!(!supports(&CaptureSource::GdiGrab { region: crate::capture::Region { x: 0, y: 0, width: 1, height: 1 } }));
    }

    #[test]
    fn eof_constant_matches_ffmpeg() {
        assert_eq!(AVERROR_EOF, -541_478_725);
    }
}
