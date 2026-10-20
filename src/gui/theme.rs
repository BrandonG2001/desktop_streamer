//! Look of the settings window: the "MetroDark" palette of the original
//! (`themes/metrodark/theme.colors.xaml`, `MainWindow.xaml`) mapped onto egui, plus the
//! generated window / notification-area icons.

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Vec2};

use crate::state::ConnectionStatus;

/// Window background (`#101010`).
pub const BG: Color32 = Color32::from_rgb(0x10, 0x10, 0x10);
/// Header / footer / tab strip background (`#151515`).
pub const PANEL: Color32 = Color32::from_rgb(0x15, 0x15, 0x15);
/// Control background (`#1D1D1D`).
pub const CONTROL: Color32 = Color32::from_rgb(0x1D, 0x1D, 0x1D);
/// Hovered control background (`#333333`).
pub const CONTROL_HOVER: Color32 = Color32::from_rgb(0x33, 0x33, 0x33);
/// Border (`#444444`).
pub const BORDER: Color32 = Color32::from_rgb(0x44, 0x44, 0x44);
/// Main text (`#F0F0F0`).
pub const TEXT: Color32 = Color32::from_rgb(0xF0, 0xF0, 0xF0);
/// Secondary text (`#BABABA`).
pub const TEXT_DIM: Color32 = Color32::from_rgb(0xBA, 0xBA, 0xBA);
/// Inactive tab text (`#808080`).
pub const TEXT_TAB: Color32 = Color32::from_rgb(0x80, 0x80, 0x80);
/// Accent (`#00AADE`).
pub const ACCENT: Color32 = Color32::from_rgb(0x00, 0xAA, 0xDE);
/// Selection (`#2E79BD`).
pub const SELECTION: Color32 = Color32::from_rgb(0x2E, 0x79, 0xBD);
/// Errors (`#D0284C`).
pub const ERROR: Color32 = Color32::from_rgb(0xD0, 0x28, 0x4C);
/// Warnings (tray "connecting" colour).
pub const WARNING: Color32 = Color32::from_rgb(0xF0, 0xA0, 0x30);

/// Applies the palette, spacing and text sizes to every egui theme.
///
/// # Arguments
/// * `ctx` - egui context.
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
        ]
        .into();
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(10.0, 4.0);
        style.spacing.interact_size.y = 24.0;
        style.spacing.slider_width = 220.0;
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = Some(TEXT);
        v.panel_fill = BG;
        v.window_fill = PANEL;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_corner_radius = CornerRadius::ZERO;
        v.menu_corner_radius = CornerRadius::ZERO;
        v.extreme_bg_color = CONTROL;
        v.faint_bg_color = PANEL;
        v.hyperlink_color = ACCENT;
        v.error_fg_color = ERROR;
        v.warn_fg_color = WARNING;
        v.selection.bg_fill = SELECTION;
        v.selection.stroke = Stroke::new(1.0, TEXT);
        v.slider_trailing_fill = true;
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = BG;
        w.noninteractive.weak_bg_fill = BG;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        for (state, fill) in [(&mut w.inactive, CONTROL), (&mut w.hovered, CONTROL_HOVER), (&mut w.active, SELECTION), (&mut w.open, CONTROL_HOVER)] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.corner_radius = CornerRadius::ZERO;
            state.expansion = 0.0;
        }
        w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        w.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
        w.active.bg_stroke = Stroke::new(1.0, ACCENT);
        w.open.bg_stroke = Stroke::new(1.0, ACCENT);
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.hovered.fg_stroke = Stroke::new(1.0, TEXT);
        w.active.fg_stroke = Stroke::new(1.5, TEXT);
    });
}

/// Colour of the status dot / tray icon for a connection status.
///
/// # Arguments
/// * `status` - connection status.
///
/// # Returns
/// Grey when disconnected, amber while connecting or waiting, accent blue when connected.
pub fn status_color(status: ConnectionStatus) -> Color32 {
    match status {
        ConnectionStatus::Disconnected => TEXT_TAB,
        ConnectionStatus::Connecting | ConnectionStatus::Waiting => WARNING,
        ConnectionStatus::Connected => ACCENT,
    }
}

/// Draws the application icon (a monitor glyph on a coloured disc) as RGBA pixels.
///
/// The original ships `Disconnected.ico`, `ConnectedToServer.ico` and `ConnectedToPeer.ico`;
/// this port generates equivalents instead of redistributing them.
///
/// # Arguments
/// * `size` - width and height in pixels.
/// * `color` - disc colour.
///
/// # Returns
/// `size * size * 4` bytes, row-major RGBA.
pub fn icon_rgba(size: u32, color: Color32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    let (cx, cy, r) = (s / 2.0, s / 2.0, s / 2.0 - 0.5);
    // Screen: a white frame in the upper part of the disc, with a short stand below it.
    let (sx0, sx1, sy0, sy1) = (s * 0.24, s * 0.76, s * 0.27, s * 0.62);
    let frame = (s * 0.07).max(1.0);
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            // Anti-aliased disc edge.
            let cover = (r - d + 0.5).clamp(0.0, 1.0);
            if cover <= 0.0 {
                continue;
            }
            let in_screen = fx >= sx0 && fx <= sx1 && fy >= sy0 && fy <= sy1;
            let in_frame = in_screen && (fx < sx0 + frame || fx > sx1 - frame || fy < sy0 + frame || fy > sy1 - frame);
            let in_stand = (fx - cx).abs() <= frame * 0.75 && fy > sy1 && fy <= sy1 + s * 0.08
                || (fx - cx).abs() <= s * 0.14 && fy > sy1 + s * 0.08 && fy <= sy1 + s * 0.08 + frame;
            let c = if in_frame || in_stand { Color32::WHITE } else { color };
            let i = ((y * size + x) * 4) as usize;
            px[i] = c.r();
            px[i + 1] = c.g();
            px[i + 2] = c.b();
            px[i + 3] = (cover * 255.0) as u8;
        }
    }
    px
}

/// Window icon for eframe.
///
/// # Returns
/// 64×64 icon in the accent colour.
pub fn window_icon() -> egui::IconData {
    egui::IconData { rgba: icon_rgba(64, ACCENT), width: 64, height: 64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_transparent_corners_and_opaque_centre() {
        let px = icon_rgba(32, ACCENT);
        assert_eq!(px.len(), 32 * 32 * 4);
        assert_eq!(px[3], 0, "corner is transparent");
        let centre = ((16 * 32 + 16) * 4) as usize;
        assert_eq!(px[centre + 3], 255);
    }
}
