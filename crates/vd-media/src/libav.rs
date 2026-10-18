//! In-process encoder using libavcodec through `ffmpeg-next` (feature `ffmpeg-lib`).
//!
//! **Version pinning:** `ffmpeg-next` follows FFmpeg's major version (7.x ↔ FFmpeg 7).  When
//! upgrading FFmpeg, bump `ffmpeg-next` in `crates/vd-media/Cargo.toml`; this file is the only
//! code that touches the libav* C API, so API changes are contained here.
//!
//! Unlike the CLI backend this encoder supports what the original NVENC/AMF code did at run time:
//! forcing an IDR frame (`DynamicSettings.ForceKeyFrame`) and changing the bitrate
//! (`_0014._0005` bitrate setters → `nvEncReconfigureEncoder`) without restarting.
//!
//! Frames are supplied as CPU NV12 buffers (e.g. from a PipeWire or `-f rawvideo` capture).

use ffmpeg_next as ff;

use crate::codec::EncoderConfig;
use crate::error::{MediaError, Result};
use crate::{EncodedFrame, now_ticks};

fn err(e: ff::Error) -> MediaError {
    MediaError::Libav(e.to_string())
}

/// libavcodec video encoder.
pub struct LibEncoder {
    encoder: ff::encoder::Video,
    config: EncoderConfig,
    frame: ff::frame::Video,
    pts: i64,
    force_key: bool,
    pending_capture: std::collections::VecDeque<i64>,
}

impl LibEncoder {
    /// Opens the encoder.
    ///
    /// # Arguments
    /// * `config` - encoder configuration (backend selects the libavcodec encoder name).
    ///
    /// # Returns
    /// The opened encoder.
    ///
    /// # Errors
    /// [`MediaError::NoEncoder`] if the encoder is not compiled into libavcodec,
    /// [`MediaError::Libav`] if it fails to open.
    pub fn open(config: &EncoderConfig) -> Result<Self> {
        ff::init().map_err(err)?;
        let name = config.backend.encoder_name(config.codec).ok_or(MediaError::NoEncoder("backend"))?;
        let codec = ff::encoder::find_by_name(name).ok_or(MediaError::NoEncoder("libavcodec"))?;
        let ctx = ff::codec::context::Context::new_with_codec(codec);
        let mut video = ctx.encoder().video().map_err(err)?;
        video.set_width(config.width);
        video.set_height(config.height);
        video.set_format(ff::format::Pixel::NV12);
        video.set_time_base(ff::Rational::new(1, config.framerate as i32));
        video.set_frame_rate(Some(ff::Rational::new(config.framerate as i32, 1)));
        video.set_bit_rate(config.bitrate as usize);
        video.set_max_bit_rate(config.bitrate as usize);
        video.set_gop(u32::MAX >> 1);
        video.set_max_b_frames(0);
        let mut opts = ff::Dictionary::new();
        // Reuse the CLI option table: every `-key value` pair after the encoder name is a
        // private option of the encoder.
        let args = config.encoder_args(crate::version::FfmpegVersion { major: 7, minor: 0 });
        let mut it = args.iter().skip(2);
        while let (Some(k), Some(v)) = (it.next(), it.next()) {
            let key = k.trim_start_matches('-');
            if !matches!(key, "g" | "bf" | "b:v" | "maxrate" | "bufsize" | "pix_fmt") {
                opts.set(key, v);
            }
        }
        let encoder = video.open_with(opts).map_err(err)?;
        let frame = ff::frame::Video::new(ff::format::Pixel::NV12, config.width, config.height);
        Ok(Self { encoder, config: config.clone(), frame, pts: 0, force_key: true, pending_capture: Default::default() })
    }

    /// Requests that the next frame be an IDR frame.
    pub fn force_keyframe(&mut self) {
        self.force_key = true;
    }

    /// Encodes one NV12 frame.
    ///
    /// # Arguments
    /// * `y` - luma plane (`width × height` bytes, tightly packed).
    /// * `uv` - interleaved chroma plane (`width × height / 2` bytes).
    /// * `capture_time` - capture time in ticks.
    ///
    /// # Returns
    /// Zero or more encoded frames that became available.
    ///
    /// # Errors
    /// [`MediaError::Libav`] on encoder errors.
    pub fn encode(&mut self, y: &[u8], uv: &[u8], capture_time: i64) -> Result<Vec<EncodedFrame>> {
        let (w, h) = (self.config.width as usize, self.config.height as usize);
        copy_plane(self.frame.data_mut(0), self.frame.stride(0), y, w, h);
        let uv_stride = self.frame.stride(1);
        copy_plane(self.frame.data_mut(1), uv_stride, uv, w, h / 2);
        self.frame.set_pts(Some(self.pts));
        self.pts += 1;
        self.frame.set_kind(if std::mem::take(&mut self.force_key) { ff::picture::Type::I } else { ff::picture::Type::None });
        self.encoder.send_frame(&self.frame).map_err(err)?;
        self.pending_capture.push_back(capture_time);
        let mut out = Vec::new();
        let mut packet = ff::Packet::empty();
        while self.encoder.receive_packet(&mut packet).is_ok() {
            let captured = self.pending_capture.pop_front().unwrap_or(capture_time);
            out.push(EncodedFrame {
                data: packet.data().map(<[u8]>::to_vec).unwrap_or_default(),
                keyframe: packet.is_key(),
                capture_time: captured,
                encode_latency_ms: (now_ticks() - captured) as f32 / 10_000.0,
            });
        }
        Ok(out)
    }
}

fn copy_plane(dst: &mut [u8], dst_stride: usize, src: &[u8], width: usize, rows: usize) {
    for r in 0..rows {
        let s = &src[r * width..(r + 1) * width];
        dst[r * dst_stride..r * dst_stride + width].copy_from_slice(s);
    }
}
