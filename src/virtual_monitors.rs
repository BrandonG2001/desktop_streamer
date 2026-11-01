//! Virtual monitors.  C#: the `IVirtualMonitor` calls of `StreamerManager` and `DesktopStreamer`.
//!
//! The original adds virtual monitors (its IddCx driver `vdvmd.dll`) when a session asks for more
//! monitors than the PC has (`AddIfNecessary(MonitorCount)` at session start, `Add` from the
//! headset's add-monitor button once every physical monitor is streamed) and removes them again
//! (`Remove`).  The port uses the Parsec Virtual Display Driver ([`vd_platform::virtual_display`])
//! and has two modes (`Port.VirtualMonitors`):
//!
//! * **physical** (default, as the original): virtual monitors fill up to `MonitorCount` when the
//!   PC has fewer physical monitors, and are streamed after them;
//! * **virtual**: only virtual monitors are streamed, `MonitorCount` of them ("All" = the
//!   headset's maximum), a separate workspace while the physical monitors stay as they are.
//!
//! The monitors exist only while a headset is connected.  They get the resolution chosen in
//! OPTIONS, at the lowest driver refresh rate that is at least the headset's desktop frame rate.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};
use vd_platform::display::{self, MonitorInfo};
use vd_platform::virtual_display::VirtualDisplays;
use vd_protocol::hmd;
use vd_protocol::types::HmdType;

use crate::app::App;
use crate::desktop;

/// How long Windows may take to attach an added display.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
/// Time Windows needs to rearrange the desktop after displays were removed.
const DETACH_SETTLE: Duration = Duration::from_millis(800);

/// Target state of the virtual monitors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Target {
    /// Number of virtual monitors.
    count: usize,
    /// Resolution.
    size: (u32, u32),
    /// Wanted refresh rate (the headset's desktop frame rate).
    framerate: u32,
}

/// Number of virtual monitors the session needs (C#: `AddIfNecessary(MonitorCount)`, plus the
/// port's virtual mode).
///
/// # Arguments
/// * `app` - application context.
///
/// # Returns
/// The count; 0 without a session or off Windows.
fn wanted(app: &App) -> usize {
    target(app).count
}

/// Whether the virtual monitors match what the session needs, so the desktop stream can be
/// planned with the final layout.
///
/// # Arguments
/// * `app` - application context.
///
/// # Returns
/// `true` when nothing is pending.
pub fn settled(app: &App) -> bool {
    let wanted = wanted(app);
    let d = app.state.dynamic.borrow();
    !d.virtual_pending && (d.virtual_failed || d.virtual_monitors == wanted)
}

/// Number of virtual monitors and their mode for the current session.
///
/// # Arguments
/// * `app` - application context.
///
/// # Returns
/// The target; `count` is 0 without a session or off Windows.
fn target(app: &App) -> Target {
    let (size, virtual_mode, wanted) = {
        let s = app.settings.lock().expect("poisoned");
        (s.port.virtual_resolution(), s.port.virtual_monitors, s.monitor_count)
    };
    let framerate = app.state.mobile.borrow().desktop_framerate.max(1) as u32;
    let mut t = Target { count: 0, size, framerate };
    let hmd_type = app.state.mobile.borrow().hmd_type;
    let d = app.state.dynamic.borrow();
    if !cfg!(windows) || !d.is_connected || hmd_type == HmdType::UNKNOWN {
        return t;
    }
    // Multi-monitor needs a hardware encoder (C#: HasHardwareEncoder ? MaxMonitors(hmd) : 1).
    let hardware = app.media.get().is_some_and(|m| m.backend.is_hardware());
    let cap = if hardware { hmd::max_monitors(hmd_type) } else { 1 };
    t.count = if virtual_mode {
        if wanted <= 0 { cap } else { wanted.min(cap) }
    } else {
        let physical = d.monitors.iter().filter(|m| !m.is_virtual).count() as i32;
        if wanted <= 0 { 0 } else { wanted.min(cap) - physical }
    }
    .max(0) as usize;
    t
}

/// Virtual monitors currently attached to the desktop.
///
/// # Returns
/// Their device names.
fn attached() -> Vec<String> {
    display::monitors().unwrap_or_default().into_iter().filter(|m| m.is_virtual).map(|m| m.id).collect()
}

/// Refresh rate for a virtual monitor: the lowest mode at least `framerate`, else the highest.
///
/// # Arguments
/// * `modes` - modes of the display.
/// * `size` - wanted resolution.
/// * `framerate` - headset desktop frame rate.
///
/// # Returns
/// The refresh rate, or `None` when the display has no mode of that resolution.
fn refresh_for(modes: &[(u32, u32, u32)], size: (u32, u32), framerate: u32) -> Option<u32> {
    let mut rates: Vec<u32> = modes.iter().filter(|m| (m.0, m.1) == size).map(|m| m.2).collect();
    rates.sort_unstable();
    rates.iter().copied().find(|&r| r >= framerate).or(rates.last().copied())
}

/// Gives every virtual monitor the target mode.
///
/// # Arguments
/// * `target` - wanted resolution and frame rate.
fn apply_mode(target: Target) {
    for m in display::monitors().unwrap_or_default().into_iter().filter(|m| m.is_virtual) {
        let Some(hz) = refresh_for(&display::modes(&m.id), target.size, target.framerate) else {
            warn!(monitor = %m.id, width = target.size.0, height = target.size.1, "the virtual monitor has no mode of this resolution");
            continue;
        };
        if (m.width, m.height, m.refresh_hz) == (target.size.0, target.size.1, hz) {
            continue;
        }
        match display::set_mode(&m.id, target.size.0, target.size.1, hz) {
            Ok(()) => info!(monitor = %m.id, width = target.size.0, height = target.size.1, hz, "virtual monitor mode set"),
            Err(e) => warn!(error = %e, monitor = %m.id, "cannot set the virtual monitor mode"),
        }
    }
}

/// Adds or removes displays until `target.count` exist, and applies the mode (blocking).
///
/// # Arguments
/// * `app` - application context (driver availability is recorded).
/// * `current` - the open driver, if any.
/// * `target` - wanted state.
///
/// # Returns
/// The driver, or `None` when no virtual monitor is wanted or the driver is unavailable.
fn reconcile(app: &App, current: Option<VirtualDisplays>, target: Target) -> Option<VirtualDisplays> {
    if target.count == 0 {
        if let Some(vd) = current {
            let n = vd.count();
            drop(vd);
            std::thread::sleep(DETACH_SETTLE);
            info!(count = n, "virtual monitors removed");
        }
        return None;
    }
    let mut vd = match current {
        Some(vd) => vd,
        None => match VirtualDisplays::open() {
            Ok(vd) => {
                app.virtual_available.store(true, Ordering::Relaxed);
                info!(version = %vd.version(), "Parsec Virtual Display Driver opened");
                vd
            }
            Err(e) => {
                app.virtual_available.store(false, Ordering::Relaxed);
                warn!(error = %e, "virtual monitors unavailable (install it with: winget install Parsec.ParsecVDD)");
                return None;
            }
        },
    };
    let mut removed = false;
    while vd.count() > target.count {
        match vd.remove() {
            Ok(_) => removed = true,
            Err(e) => {
                warn!(error = %e, "cannot remove a virtual monitor");
                break;
            }
        }
    }
    if removed {
        std::thread::sleep(DETACH_SETTLE);
    }
    if vd.count() < target.count {
        // All requests first: an add sent while Windows is still attaching the previous display
        // waits about 3 s for it, while several at once attach together.
        let before = attached().len();
        let mut added = 0;
        while vd.count() < target.count {
            if let Err(e) = vd.add() {
                warn!(error = %e, "cannot add a virtual monitor");
                break;
            }
            added += 1;
        }
        let started = Instant::now();
        while attached().len() < before + added && started.elapsed() < ATTACH_TIMEOUT {
            std::thread::sleep(Duration::from_millis(100));
        }
        debug!(added, elapsed_ms = started.elapsed().as_millis() as u64, "virtual monitors attached");
    }
    let started = Instant::now();
    apply_mode(target);
    debug!(elapsed_ms = started.elapsed().as_millis() as u64, "virtual monitor modes applied");
    info!(count = vd.count(), width = target.size.0, height = target.size.1, "virtual monitors ready");
    Some(vd)
}

/// Virtual-monitor task: keeps the displays in line with [`target`] and publishes the new
/// monitor layout.
///
/// # Arguments
/// * `app` - application context.
pub async fn run(app: Arc<App>) {
    if cfg!(windows) {
        let available = tokio::task::spawn_blocking(vd_platform::virtual_display::is_available).await.unwrap_or(false);
        app.virtual_available.store(available, Ordering::Relaxed);
    }
    let mut dyn_rx = app.state.dynamic.subscribe();
    let mut mob_rx = app.state.mobile.subscribe();
    let mut shutdown = app.shutdown.subscribe();
    let mut current: Option<VirtualDisplays> = None;
    let mut applied: Option<Target> = None;
    // A target the driver could not be opened for: not retried until the target changes.
    let mut failed: Option<Target> = None;
    loop {
        let t = target(&app);
        if t.count == 0 {
            // The next session tries the driver again.
            failed = None;
        }
        let have = current.as_ref().map_or(0, VirtualDisplays::count);
        let stale = if t.count == 0 { have > 0 } else { applied != Some(t) && failed != Some(t) };
        if stale {
            app.state.dynamic.send_modify(|d| d.virtual_pending = t.count > 0);
            let worker = Arc::clone(&app);
            let taken = current.take();
            current = tokio::task::spawn_blocking(move || reconcile(&worker, taken, t)).await.ok().flatten();
            let ok = t.count == 0 || current.is_some();
            applied = ok.then_some(t);
            failed = (!ok).then_some(t);
            let monitors = desktop::list_monitors((1920, 1080));
            let streamable = desktop::streamable(&app, &monitors).len().max(1);
            let count = current.as_ref().map_or(0, VirtualDisplays::count);
            app.state.dynamic.send_modify(|d| {
                d.virtual_pending = false;
                d.virtual_monitors = count;
                d.virtual_failed = !ok;
                d.monitors = monitors;
                d.monitor_start_index %= streamable;
            });
            if t.count == 0 {
                applied = None;
            }
            continue;
        }
        tokio::select! {
            r = dyn_rx.changed() => if r.is_err() { break },
            r = mob_rx.changed() => if r.is_err() { break },
            _ = shutdown.changed() => break,
        }
        if app.is_shutting_down() {
            break;
        }
    }
    // Remove the displays now rather than when the keep-alive stops with the process.
    if let Some(vd) = current {
        let _ = tokio::task::spawn_blocking(move || drop(vd)).await;
    }
}

/// Whether a monitor list differs in a way that matters for input mapping.
///
/// # Arguments
/// * `a`, `b` - monitor lists.
///
/// # Returns
/// `true` when the desktop rectangles differ.
pub fn layout_changed(a: &[MonitorInfo], b: &[MonitorInfo]) -> bool {
    display::virtual_desktop_bounds(a) != display::virtual_desktop_bounds(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_is_the_lowest_mode_at_or_above_the_frame_rate() {
        let modes = [(1920, 1080, 60), (1920, 1080, 240), (1920, 1080, 144), (1920, 1080, 30), (2560, 1440, 60)];
        assert_eq!(refresh_for(&modes, (1920, 1080), 60), Some(60));
        assert_eq!(refresh_for(&modes, (1920, 1080), 72), Some(144));
        assert_eq!(refresh_for(&modes, (1920, 1080), 300), Some(240));
        assert_eq!(refresh_for(&modes, (2560, 1440), 90), Some(60));
        assert_eq!(refresh_for(&modes, (3840, 2160), 60), None);
    }
}
