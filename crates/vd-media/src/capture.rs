//! Desktop capture sources expressed as FFmpeg inputs.
//!
//! The original captured monitors in `VirtualDesktop.Server.exe` (running as SYSTEM so it can
//! also capture the secure desktop) with DXGI Desktop Duplication and shared the frames with the
//! streamer through a keyed-mutex D3D11 texture (`IMonitorDuplication.BeginCapture` returns the
//! shared handle).  The port lets FFmpeg do the capture:
//!
//! | platform | source                     | zero-copy path                                  |
//! |----------|----------------------------|-------------------------------------------------|
//! | Windows  | `ddagrab` (Desktop Dup.)   | D3D11 frames straight into NVENC / AMF / QSV    |
//! | Windows  | `gdigrab`                  | CPU fallback                                    |
//! | Linux    | `kmsgrab` (DRM/KMS)        | DMA-BUF → VA-API (`hwmap`), needs CAP_SYS_ADMIN |
//! | Linux    | `x11grab`                  | CPU, X11 sessions                               |
//! | macOS    | `avfoundation`             | CPU                                             |
//! | any      | test pattern / custom args | for development and Wayland portal pipelines    |
//!
//! Wayland compositors only expose screen capture through the xdg-desktop-portal / PipeWire;
//! FFmpeg has no stable PipeWire grabber yet, so on Wayland use `kmsgrab` or [`CaptureSource::Custom`]
//! (see `docs/REVERSE_ENGINEERING.md`, "Porting notes").

use crate::codec::Backend;

/// A rectangle of the virtual desktop, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Region {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

/// Where frames come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureSource {
    /// Windows Desktop Duplication through the `ddagrab` filter (FFmpeg ≥ 6.0).
    DdaGrab {
        /// DXGI output index (monitor).
        output_index: u32,
    },
    /// Windows GDI grabber.
    GdiGrab {
        /// Desktop region.
        region: Region,
    },
    /// X11 grabber.
    X11 {
        /// X display (e.g. `:0`).
        display: String,
        /// Desktop region.
        region: Region,
    },
    /// DRM/KMS framebuffer grabber.
    KmsGrab {
        /// DRM card (e.g. `/dev/dri/card0`).
        device: String,
        /// Explicit plane id, if the default primary plane is wrong.
        plane: Option<u32>,
    },
    /// macOS AVFoundation screen.
    AvFoundation {
        /// Screen index as listed by `-list_devices`.
        screen: u32,
    },
    /// Synthetic moving test pattern (`testsrc2`) — useful without a display server.
    TestPattern {
        /// Pattern size.
        width: u32,
        /// Pattern height.
        height: u32,
    },
    /// Arbitrary input arguments, e.g. a PipeWire/GStreamer bridge.  Frames must be CPU frames.
    Custom {
        /// Arguments inserted before the output options (must include `-i`).
        args: Vec<String>,
    },
}

/// Complete capture request for one monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSpec {
    /// Source.
    pub source: CaptureSource,
    /// Capture frame rate.
    pub framerate: u32,
    /// Draw the mouse cursor into the video (the original passes `drawCursor = !is_flat_device`:
    /// headsets get the cursor baked in, phones/tablets draw it locally from `Cursor` messages).
    pub draw_cursor: bool,
    /// Size the encoder expects; the capture is scaled when it differs from the source.
    pub output_size: (u32, u32),
    /// Source size when known (used to skip scaling).
    pub source_size: Option<(u32, u32)>,
}

/// FFmpeg arguments produced for a capture.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureArgs {
    /// Arguments up to and including the `-i` input.
    pub input: Vec<String>,
    /// `-vf` filter chain (empty for none).
    pub filter: String,
    /// Frames reach the encoder as GPU surfaces (`d3d11`/`qsv`/`vaapi`).  The encoder then
    /// performs the colour conversion itself (BGRA → NV12/P010 inside NVENC/AMF), so no
    /// `-pix_fmt` may be forced — doing so requests an impossible CPU conversion.
    pub gpu_frames: bool,
}

fn s(v: &str) -> String {
    v.to_owned()
}

impl CaptureSpec {
    fn needs_scale(&self) -> bool {
        self.source_size.is_none_or(|sz| sz != self.output_size)
    }

    fn cpu_tail(&self, backend: Backend, pix_fmt: &str) -> String {
        let (w, h) = self.output_size;
        let mut f = if self.needs_scale() { format!("scale={w}:{h}:flags=bicubic,format={pix_fmt}") } else { format!("format={pix_fmt}") };
        match backend {
            Backend::Vaapi => f.push_str(",hwupload"),
            Backend::Qsv if cfg!(target_os = "linux") => f.push_str(",hwupload=extra_hw_frames=16"),
            _ => {}
        }
        f
    }

    /// Builds the input arguments and filter chain for `backend`.
    ///
    /// # Arguments
    /// * `backend` - encoder backend (decides whether frames stay on the GPU).
    /// * `pix_fmt` - encoder input format for CPU paths (`nv12`, `p010le`, `yuv444p`).
    /// * `render_node` - VA-API / QSV render node (Linux).
    ///
    /// # Returns
    /// The arguments.
    pub fn ffmpeg_args(&self, backend: Backend, pix_fmt: &str, render_node: &str) -> CaptureArgs {
        let fr = self.framerate.max(1).to_string();
        let (w, h) = self.output_size;
        let mut input = vec![s("-fflags"), s("nobuffer"), s("-flags"), s("low_delay")];
        let device_args = |input: &mut Vec<String>| match backend {
            Backend::Vaapi => input.extend([s("-vaapi_device"), s(render_node)]),
            Backend::Qsv if cfg!(target_os = "linux") => input.extend([s("-init_hw_device"), format!("qsv=hw:{render_node}"), s("-filter_hw_device"), s("hw")]),
            _ => {}
        };
        let filter = match &self.source {
            CaptureSource::DdaGrab { output_index } => {
                let mouse = u8::from(self.draw_cursor);
                input.extend([s("-f"), s("lavfi"), s("-i"), format!("ddagrab=output_idx={output_index}:framerate={fr}:draw_mouse={mouse}")]);
                match backend {
                    Backend::Nvenc | Backend::Amf if !self.needs_scale() => String::new(),
                    Backend::Qsv if !self.needs_scale() => s("hwmap=derive_device=qsv,format=qsv"),
                    _ => format!("hwdownload,format=bgra,{}", self.cpu_tail(backend, pix_fmt)),
                }
            }
            CaptureSource::GdiGrab { region } => {
                input.extend([
                    s("-f"), s("gdigrab"), s("-framerate"), fr, s("-draw_mouse"), u8::from(self.draw_cursor).to_string(),
                    s("-offset_x"), region.x.to_string(), s("-offset_y"), region.y.to_string(),
                    s("-video_size"), format!("{}x{}", region.width, region.height), s("-i"), s("desktop"),
                ]);
                self.cpu_tail(backend, pix_fmt)
            }
            CaptureSource::X11 { display, region } => {
                device_args(&mut input);
                input.extend([
                    s("-f"), s("x11grab"), s("-framerate"), fr, s("-draw_mouse"), u8::from(self.draw_cursor).to_string(),
                    s("-video_size"), format!("{}x{}", region.width, region.height), s("-i"), format!("{display}+{},{}", region.x, region.y),
                ]);
                self.cpu_tail(backend, pix_fmt)
            }
            CaptureSource::KmsGrab { device, plane } => {
                input.extend([s("-device"), device.clone(), s("-f"), s("kmsgrab"), s("-framerate"), fr]);
                if let Some(p) = plane {
                    input.extend([s("-plane_id"), p.to_string()]);
                }
                input.extend([s("-i"), s("-")]);
                let fmt = if pix_fmt == "p010le" { "p010" } else { "nv12" };
                let base = format!("hwmap=derive_device=vaapi,scale_vaapi=w={w}:h={h}:format={fmt}");
                if backend == Backend::Vaapi { base } else { format!("{base},hwdownload,format={fmt}") }
            }
            CaptureSource::AvFoundation { screen } => {
                input.extend([
                    s("-f"), s("avfoundation"), s("-framerate"), fr, s("-capture_cursor"), u8::from(self.draw_cursor).to_string(),
                    s("-i"), format!("{screen}:none"),
                ]);
                self.cpu_tail(backend, pix_fmt)
            }
            CaptureSource::TestPattern { width, height } => {
                device_args(&mut input);
                input.extend([s("-re"), s("-f"), s("lavfi"), s("-i"), format!("testsrc2=size={width}x{height}:rate={fr}")]);
                self.cpu_tail(backend, pix_fmt)
            }
            CaptureSource::Custom { args } => {
                device_args(&mut input);
                input.extend(args.iter().cloned());
                self.cpu_tail(backend, pix_fmt)
            }
        };
        let gpu_frames = match &self.source {
            CaptureSource::DdaGrab { .. } => !filter.starts_with("hwdownload"),
            CaptureSource::KmsGrab { .. } => backend == Backend::Vaapi,
            _ => matches!(backend, Backend::Vaapi) || (backend == Backend::Qsv && cfg!(target_os = "linux")),
        };
        CaptureArgs { input, filter, gpu_frames }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_vaapi_uploads() {
        let spec = CaptureSpec {
            source: CaptureSource::X11 { display: ":0".into(), region: Region { x: 0, y: 0, width: 2560, height: 1440 } },
            framerate: 60,
            draw_cursor: true,
            output_size: (1920, 1080),
            source_size: Some((2560, 1440)),
        };
        let a = spec.ffmpeg_args(Backend::Vaapi, "nv12", "/dev/dri/renderD128");
        assert!(a.input.contains(&"x11grab".to_string()));
        assert!(a.input.contains(&"-vaapi_device".to_string()));
        assert_eq!(a.filter, "scale=1920:1080:flags=bicubic,format=nv12,hwupload");
    }

    #[test]
    fn ddagrab_nvenc_is_zero_copy() {
        let spec = CaptureSpec {
            source: CaptureSource::DdaGrab { output_index: 0 },
            framerate: 72,
            draw_cursor: true,
            output_size: (1920, 1080),
            source_size: Some((1920, 1080)),
        };
        let a = spec.ffmpeg_args(Backend::Nvenc, "p010le", "");
        assert!(a.filter.is_empty());
        assert!(a.gpu_frames);
    }
}
