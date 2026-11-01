//! Tab pages of the settings window.  C#: the `TabItem`s of `MainWindow.xaml` and their click
//! handlers in `MainWindow.cs` (`OnSaveClick`, `OnChangeClick`, `OnRemoveAccountClick`,
//! `OnOptionsResetDefaultsClick`, `OnAdvancedOptionsResetDefaultsClick`,
//! `OnChangeScreenshotsLocation`, `OnChangeVideosLocation`, `OnAddVideoFolderClick`,
//! `OnRemoveFolderClick`, `OnShowLogsClick`) and `AddAccountWindow`.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Align, Layout, RichText, Vec2};
use vd_media::codec::Backend;
use vd_protocol::types::{AudioStreaming, GamepadEmulation, NetworkConnectionType, OpenXrRuntime, Platform, RoutingStatus, StreamingSource, VideoCodec};

use super::{AddAccount, Confirm, SettingsWindow, VideoFolder, dialog_frame, theme};
use crate::bindings::{BindingAction, KEYS, key_label, modifiers};
use crate::pairing::MAX_ACCOUNTS_PER_PLATFORM;
use crate::settings::{StreamerSettings, VIRTUAL_RESOLUTIONS};

/// Platforms offered by "Add account" (C#: `PlatformProvider`).
const ADDABLE_PLATFORMS: [Platform; 5] = [Platform::OculusQuest, Platform::Pico, Platform::Google, Platform::PlayForDream, Platform::Vive];

/// Codecs offered by "Preferred Codec" (C#: `VideoCodecProvider`).
const CODECS: [VideoCodec; 6] = [VideoCodec::Automatic, VideoCodec::H264, VideoCodec::H264Plus, VideoCodec::HEVC, VideoCodec::HEVC10bit, VideoCodec::AV110bit];

/// Display name of a platform (C#: `[EnumDisplayName]`).
///
/// # Arguments
/// * `p` - platform.
///
/// # Returns
/// The name.
fn platform_name(p: Platform) -> &'static str {
    match p {
        Platform::Oculus => "Oculus",
        Platform::OculusQuest => "Meta",
        Platform::Vive => "Viveport",
        Platform::Pico => "Pico",
        Platform::Google => "Google",
        Platform::PlayForDream => "Play for Dream",
        Platform::Apple => "Apple",
        Platform::Steam => "Steam",
    }
}

/// Display name of a codec (C#: `[EnumDisplayName]`).
///
/// # Arguments
/// * `c` - codec.
///
/// # Returns
/// The name.
fn codec_name(c: VideoCodec) -> &'static str {
    match c {
        VideoCodec::Automatic => "Automatic",
        VideoCodec::H264 => "H.264",
        VideoCodec::HEVC => "HEVC",
        VideoCodec::VP8 => "VP8",
        VideoCodec::VP9 => "VP9",
        VideoCodec::H264Plus => "H.264+",
        VideoCodec::HEVC10bit => "HEVC 10-bit",
        VideoCodec::AV1 => "AV1",
        VideoCodec::AV110bit => "AV1 10-bit",
    }
}

/// Bold section label (the `FontWeight="Bold" FontSize="14"` labels of the original).
///
/// # Arguments
/// * `ui` - target UI.
/// * `text` - label text.
fn section(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).strong().size(14.0));
}

/// Small muted explanatory text.
///
/// # Arguments
/// * `ui` - target UI.
/// * `text` - text.
fn note(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(theme::TEXT_DIM));
}

/// An explanatory line with a drawn "i" icon (C#: `Resources\Info.png`).
///
/// # Arguments
/// * `ui` - target UI.
/// * `text` - text.
/// * `color` - icon and text colour.
fn info_line(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), egui::Sense::hover());
        let c = rect.center();
        ui.painter().circle_stroke(c, 6.0, egui::Stroke::new(1.2, color));
        ui.painter().circle_filled(c + Vec2::new(0.0, -2.8), 0.9, color);
        ui.painter().line_segment([c + Vec2::new(0.0, -0.8), c + Vec2::new(0.0, 3.2)], egui::Stroke::new(1.4, color));
        ui.label(RichText::new(text).size(12.0).color(color));
    });
}

/// A small button with a drawn cross (C#: `Resources/Clear20.png`).
///
/// # Arguments
/// * `ui` - target UI.
///
/// # Returns
/// The button response (with a "Remove" tooltip).
fn remove_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(18.0), egui::Sense::click());
    let color = if resp.hovered() { theme::ERROR } else { theme::TEXT_TAB };
    let c = rect.center();
    let stroke = egui::Stroke::new(1.5, color);
    ui.painter().line_segment([c + Vec2::new(-4.0, -4.0), c + Vec2::new(4.0, 4.0)], stroke);
    ui.painter().line_segment([c + Vec2::new(-4.0, 4.0), c + Vec2::new(4.0, -4.0)], stroke);
    resp.on_hover_text("Remove")
}

/// A combo box over a fixed list of values.
///
/// # Arguments
/// * `ui` - target UI.
/// * `id` - widget id salt.
/// * `value` - bound value.
/// * `options` - `(value, label)` pairs.
fn combo<T: PartialEq + Copy>(ui: &mut egui::Ui, id: &str, value: &mut T, options: &[(T, &str)]) {
    let current = options.iter().find(|(v, _)| v == value).map_or("", |(_, l)| *l);
    egui::ComboBox::from_id_salt(id).width(180.0).selected_text(current).show_ui(ui, |ui| {
        for (v, label) in options {
            ui.selectable_value(value, *v, *label);
        }
    });
}

/// A slider showing its value as a percentage (`StringFormat="0%"`).
///
/// # Arguments
/// * `ui` - target UI.
/// * `value` - bound value (1.0 = 100 %).
/// * `range` - allowed range.
fn percent_slider(ui: &mut egui::Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>) {
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(value, range).step_by(0.01).show_value(false));
        ui.label(format!("{:.0}%", *value * 100.0));
    });
}

/// Folder shown for an optional path setting.
///
/// # Arguments
/// * `path` - configured path.
/// * `fallback` - default folder.
///
/// # Returns
/// The configured path, else the default.
fn folder_or(path: &Option<String>, fallback: Option<PathBuf>) -> PathBuf {
    path.clone().map(PathBuf::from).or(fallback).unwrap_or_else(|| PathBuf::from("."))
}

/// Opens a folder picker.
///
/// # Arguments
/// * `title` - dialog title.
/// * `start` - initially selected folder.
///
/// # Returns
/// The chosen folder, or `None` when cancelled.
fn pick_folder(title: &str, start: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title(title);
    if let Some(s) = start.filter(|s| s.is_dir()) {
        dialog = dialog.set_directory(s);
    }
    dialog.pick_folder()
}

/// Lists the folder links inside the video library root.
///
/// # Arguments
/// * `root` - videos root.
///
/// # Returns
/// Every symbolic link / junction with its target.
fn list_video_folders(root: &Path) -> Vec<VideoFolder> {
    let Ok(dir) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<VideoFolder> = dir
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_symlink()))
        .filter_map(|e| Some(VideoFolder { target: std::fs::read_link(e.path()).ok()?, link: e.path() }))
        .collect();
    out.sort_by(|a, b| a.link.cmp(&b.link));
    out
}

/// Links `target` into the video library (C#: `IServiceManager.CreateSymbolicLink`, run by the
/// elevated service; a directory junction needs no privileges on Windows).
///
/// # Arguments
/// * `root` - videos root.
/// * `target` - folder to add.
///
/// # Returns
/// `Ok(false)` if a link with that name already exists.
///
/// # Errors
/// A message for the user.
fn add_video_folder(root: &Path, target: &Path) -> Result<bool, String> {
    // Name the link after the folder, or the drive for a drive root (C#: DriveInfo.VolumeLabel).
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| target.to_string_lossy().chars().next().map(|c| c.to_string()))
        .ok_or("invalid folder")?;
    let link = root.join(name);
    if link.exists() || link.symlink_metadata().is_ok() {
        return Ok(false);
    }
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        let out = crate::command("cmd").arg("/C").arg("mklink").arg("/J").arg(&link).arg(target).output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
        }
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, &link).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Removes a video-library link without touching the folder it points to.
///
/// # Arguments
/// * `link` - the link.
///
/// # Errors
/// I/O errors.
fn remove_video_folder(link: &Path) -> std::io::Result<()> {
    if cfg!(windows) { std::fs::remove_dir(link).or_else(|_| std::fs::remove_file(link)) } else { std::fs::remove_file(link) }
}

impl SettingsWindow {
    /// ACCOUNTS tab: the store accounts allowed to connect (C#: `StreamerSettings.Accounts`).
    ///
    /// # Arguments
    /// * `ui` - page UI.
    /// * `s` - settings being edited.
    pub(super) fn accounts_tab(&mut self, ui: &mut egui::Ui, s: &mut StreamerSettings) {
        note(ui, "Only headsets signed in with one of these accounts can connect to this computer.");
        ui.add_space(10.0);
        let mut remove: Option<(Platform, String)> = None;
        let mut any = false;
        egui::Grid::new("accounts").num_columns(2).spacing([24.0, 8.0]).min_col_width(120.0).show(ui, |ui| {
            for (key, names) in &s.accounts {
                let Some(platform) = Platform::parse(key) else { continue };
                if names.is_empty() {
                    continue;
                }
                any = true;
                ui.label(RichText::new(platform_name(platform)).strong());
                ui.vertical(|ui| {
                    for name in names {
                        ui.horizontal(|ui| {
                            ui.label(name);
                            if self.editing_accounts && remove_button(ui).clicked() {
                                remove = Some((platform, name.clone()));
                            }
                        });
                    }
                });
                ui.end_row();
            }
        });
        if !any {
            ui.label(RichText::new("No account has been added yet.").color(theme::TEXT_DIM));
        }
        if let Some((p, name)) = remove {
            s.remove_account(p, &name);
        }
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            if self.editing_accounts {
                ui.menu_button("Add account...", |ui| {
                    for p in ADDABLE_PLATFORMS {
                        if ui.button(platform_name(p)).clicked() {
                            self.add_account = Some(AddAccount { platform: p, name: String::new(), error: None, focused: false });
                            ui.close();
                        }
                    }
                });
                if ui.add_enabled(!s.access_tokens().is_empty(), egui::Button::new("Save")).clicked() {
                    // C#: OnSaveClick — stop editing and register again with the new accounts.
                    self.editing_accounts = false;
                    self.dirty_since = Some(std::time::Instant::now() - super::SAVE_DELAY);
                    self.app.restart_connection();
                }
            } else if ui.button("Change").clicked() {
                if self.app.state.dynamic.borrow().is_connected {
                    self.ask("Changing the account information will disconnect you and prevent future connections.\n\nAre you sure you want to continue?", Confirm::ChangeAccounts);
                } else {
                    self.editing_accounts = true;
                }
            }
        });
    }

    /// The "Add Account" dialog.
    ///
    /// # Arguments
    /// * `ctx` - egui context.
    /// * `s` - settings being edited.
    pub(super) fn add_account_dialog(&mut self, ctx: &egui::Context, s: &mut StreamerSettings) {
        let Some(mut dlg) = self.add_account.take() else { return };
        let mut done = false;
        egui::Modal::new(egui::Id::unique("add_account")).frame(dialog_frame()).show(ctx, |ui| {
            ui.set_width(420.0);
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.label(RichText::new("Add Account").strong().size(16.0));
            ui.add_space(6.0);
            ui.label(format!("{} account name", platform_name(dlg.platform)));
            let edit = ui.add(egui::TextEdit::singleline(&mut dlg.name).desired_width(f32::INFINITY).hint_text("User name"));
            if !dlg.focused {
                edit.request_focus();
                dlg.focused = true;
            }
            if let Some(e) = &dlg.error {
                ui.label(RichText::new(e).color(theme::ERROR));
            }
            ui.add_space(10.0);
            let submit = enter;
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if ui.add_sized([80.0, 26.0], egui::Button::new("Cancel")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = true;
                }
                let name = dlg.name.trim().to_owned();
                let add = ui.add_enabled_ui(!name.is_empty(), |ui| ui.add_sized([80.0, 26.0], egui::Button::new("Add"))).inner;
                if (add.clicked() || submit) && !name.is_empty() {
                    let existing = s.accounts.get(dlg.platform.member_name()).cloned().unwrap_or_default();
                    if existing.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                        dlg.error = Some("This account already exists".into());
                    } else if existing.len() >= MAX_ACCOUNTS_PER_PLATFORM {
                        dlg.error = Some(format!("You can only add {MAX_ACCOUNTS_PER_PLATFORM} accounts per platform"));
                    } else {
                        s.add_account(dlg.platform, &name);
                        done = true;
                    }
                }
            });
        });
        if !done {
            self.add_account = Some(dlg);
        }
    }

    /// BINDINGS tab: system-wide keyboard shortcuts (C#: `BindingSettings`).
    ///
    /// # Arguments
    /// * `ui` - page UI.
    pub(super) fn bindings_tab(&mut self, ui: &mut egui::Ui) {
        section(ui, "Keyboard shortcuts");
        ui.add_space(6.0);
        let mut changed = false;
        egui::Grid::new("bindings").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            for action in BindingAction::ALL {
                ui.label(action.label());
                let mut sc = self.bindings.get(action);
                ui.horizontal(|ui| {
                    for (bit, name) in [(modifiers::WINDOWS, if cfg!(windows) { "Win" } else { "Super" }), (modifiers::CONTROL, "Ctrl"), (modifiers::ALT, "Alt"), (modifiers::SHIFT, "Shift")] {
                        if ui.selectable_label(sc.modifiers & bit != 0, name).clicked() {
                            sc.modifiers ^= bit;
                        }
                    }
                    egui::ComboBox::from_id_salt(("key", action)).width(56.0).selected_text(key_label(sc.key_code)).show_ui(ui, |ui| {
                        for (code, label) in KEYS {
                            ui.selectable_value(&mut sc.key_code, *code, *label);
                        }
                    });
                });
                if sc != self.bindings.get(action) {
                    self.bindings.bindings.insert(action, sc);
                    changed = true;
                }
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        if self.hotkeys.is_none() {
            note(ui, "System-wide shortcuts are not available in this session (e.g. Wayland without XWayland).");
        }
        for e in &self.hotkey_errors {
            ui.label(RichText::new(format!("Unavailable: {e}")).size(12.0).color(theme::ERROR));
        }
        ui.add_space(8.0);
        if ui.button("Reset to Defaults").clicked() {
            self.bindings.reset_to_defaults();
            changed = true;
        }
        if changed {
            self.apply_bindings();
        }
    }

    /// OPTIONS tab: encoder, runtime, audio and connection options.
    ///
    /// # Arguments
    /// * `ui` - page UI.
    /// * `s` - settings being edited.
    pub(super) fn options_tab(&mut self, ui: &mut egui::Ui, s: &mut StreamerSettings) {
        let nvenc = self.app.media.get().is_some_and(|m| m.backend == Backend::Nvenc);
        ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
            ui.vertical(|ui| {
                ui.set_width(246.0);
                section(ui, "Preferred Codec");
                combo(ui, "codec", &mut s.preferred_codec, &CODECS.map(|c| (c, codec_name(c))));
                ui.checkbox(&mut s.enable_aq, "Adaptive quantization").on_hover_text("Reduces compression artifacts in dark scenes (recommended)");
                // C#: visible only when DynamicSettings.Supports2Pass (NVENC).
                if nvenc {
                    ui.checkbox(&mut s.enable_2pass, "2-Pass encoding")
                        .on_hover_text("Improves compression quality but uses some GPU horsepower to do so\nOnly use when you have performance headroom to spare");
                }
                ui.add_space(8.0);
                section(ui, "Monitors");
                combo(ui, "monitors", &mut s.monitor_count, &[(0, "All"), (1, "1"), (2, "2"), (3, "3")]);
                ui.label(RichText::new("Monitors shown in the headset at once (also changed with + / − in VR)").size(11.0).color(theme::TEXT_DIM));
                if cfg!(windows) {
                    self.virtual_monitor_options(ui, s);
                }
                ui.add_space(8.0);
                section(ui, "OpenXR Runtime");
                combo(ui, "openxr", &mut s.openxr_runtime, &[(OpenXrRuntime::Automatic, "Automatic"), (OpenXrRuntime::SteamVR, "SteamVR"), (OpenXrRuntime::VDXR, "VDXR")]);
                ui.add_space(8.0);
                section(ui, "Gamepad Emulation");
                combo(ui, "gamepad", &mut s.gamepad_emulation, &[(GamepadEmulation::Automatic, "Automatic"), (GamepadEmulation::Xbox, "Xbox"), (GamepadEmulation::Dualshock, "Dualshock")]);
                ui.add_space(8.0);
                section(ui, "Audio Streaming");
                combo(
                    ui,
                    "audio",
                    &mut s.audio_streaming,
                    &[(AudioStreaming::Disabled, "Computer only"), (AudioStreaming::HeadsetOnly, "VR headset only"), (AudioStreaming::HeadsetAndComputer, "VR headset & computer")],
                );
                ui.checkbox(&mut s.use_virtual_audio_driver, "Use virtual audio driver")
                    .on_hover_text("Route PC audio through the Virtual Desktop audio driver when it is installed.\nWhen off (or without the driver), all PC speakers are muted while streaming instead.");
                ui.add_enabled(s.use_virtual_audio_driver, egui::Checkbox::new(&mut s.voicemeeter_mode, "VoiceMeeter mode"))
                    .on_hover_text("Enable this option to allow Virtual Desktop to work with VoiceMeeter");
                if cfg!(windows) {
                    ui.add_space(8.0);
                    self.microphone_picker(ui, s);
                }
            });
            ui.vertical(|ui| {
                ui.checkbox(&mut s.allow_remote_connections, "Allow remote connections");
                ui.checkbox(&mut s.encrypt_local_traffic, "Encrypt local traffic");
                // Turning bitrate adaptation off asks first (OnAutoAdjustBitratePreviewMouseDown).
                let mut auto = s.auto_adjust_bitrate;
                if ui.checkbox(&mut auto, "Automatically adjust bitrate").on_hover_text("Adjusts the video bitrate based on the available bandwidth (requires re-connection)").changed() {
                    if auto {
                        s.auto_adjust_bitrate = true;
                    } else {
                        self.ask("Disabling 'Automatically adjust bitrate' can result in a laggy experience, black screens and additional latency.\nYou should only uncheck this option when using a cloud computer and the available bandwidth isn't accurately measured.\n\nAre you sure you want to continue?", Confirm::DisableAutoBitrate);
                    }
                }
                ui.checkbox(&mut s.start_with_windows, if cfg!(windows) { "Start with Windows" } else { "Start on login" });
                ui.checkbox(&mut s.start_minimized_in_tray, "Start minimized in tray");
                ui.checkbox(&mut s.use_touch_input, "Use touch input").on_hover_text("Enable to allow touch gestures. Disable if clicks don't work correctly");
                ui.checkbox(&mut s.lock_computer, "Lock computer on disconnect");
                ui.checkbox(&mut s.auto_select_microphone, "Auto-select microphone")
                    .on_hover_text("Automatically selects the Virtual Desktop Microphone in Windows when Microphone passthrough is enabled in VR");
                ui.checkbox(&mut s.show_pairing_requests, "Ask for computer access").on_hover_text("Shows a prompt when a new headset is searching for computers in your local network");
            });
        });
        ui.add_space(14.0);
        if ui.button("Reset to Defaults").clicked() {
            s.reset_options();
        }
    }

    /// "Microphone" choice: where the headset microphone goes on Windows (`Port.MicrophoneDevice`).
    ///
    /// # Arguments
    /// * `ui` - column UI.
    /// * `s` - settings being edited.
    fn microphone_picker(&mut self, ui: &mut egui::Ui, s: &mut StreamerSettings) {
        use vd_platform::audio::{MIC_AUTO, MIC_VDVAD};
        let (driver, targets) = self.mic_targets.get_or_insert_with(|| (vd_platform::audio::has_virtual_desktop_driver(), vd_platform::audio::microphone_targets())).clone();
        let mut options: Vec<(String, String)> = vec![(MIC_AUTO.to_owned(), "Automatic".to_owned())];
        if driver {
            options.push((MIC_VDVAD.to_owned(), "Virtual Desktop driver".to_owned()));
        }
        options.extend(targets.iter().cloned());
        let current = options.iter().find(|(id, _)| *id == s.port.microphone_device).map_or_else(|| "Unavailable device".to_owned(), |(_, n)| n.clone());
        section(ui, "Microphone");
        egui::ComboBox::from_id_salt("microphone")
            .width(180.0)
            .truncate()
            .selected_text(current)
            .show_ui(ui, |ui| {
                for (id, name) in &options {
                    ui.selectable_value(&mut s.port.microphone_device, id.clone(), name.as_str());
                }
            })
            .response
            .on_hover_text(
                "Where the headset microphone goes.\nAutomatic uses the Virtual Desktop driver if it is installed, otherwise a virtual audio cable\n(e.g. VB-Audio Virtual Cable: apps then record from \"CABLE Output\").",
            );
        let has_cable = targets.iter().any(|(_, n)| n.contains("CABLE") || n.contains("VoiceMeeter"));
        if !driver && !has_cable {
            ui.label(RichText::new("No virtual microphone found.").size(12.0).color(theme::WARNING));
            ui.hyperlink_to(RichText::new("Get VB-Audio Virtual Cable (free)").size(12.0), "https://vb-audio.com/Cable/");
        }
    }

    /// ADVANCED tab: VR rendering overrides.
    ///
    /// # Arguments
    /// * `ui` - page UI.
    /// * `s` - settings being edited.
    pub(super) fn advanced_tab(&mut self, ui: &mut egui::Ui, s: &mut StreamerSettings) {
        info_line(ui, "Do NOT change the settings below unless you know what you are doing", theme::WARNING);
        note(ui, "Requires restart of game / SteamVR to take effect");
        ui.add_space(10.0);
        section(ui, "Horizontal FOV Tangent");
        percent_slider(ui, &mut s.horizontal_fov_tangent, 0.4..=1.0);
        section(ui, "Vertical FOV Tangent");
        percent_slider(ui, &mut s.vertical_fov_tangent, 0.4..=1.0);
        section(ui, "VDXR Render Resolution");
        percent_slider(ui, &mut s.render_resolution_override, 0.5..=2.0);
        note(ui, "Best to use 100% and control the resolution via the VR Graphics Quality in the Streaming tab in VR");
        ui.add_space(4.0);
        section(ui, "Fovea Size");
        ui.scope(|ui| percent_slider(ui, &mut s.fovea_size, 0.15..=0.35)).response.on_hover_text("Only applies to eye-tracked headsets with Foveated streaming enabled");
        ui.add_space(4.0);
        ui.checkbox(&mut s.use_sharpening, "Additional sharpening").on_hover_text("Adds Contrast Adaptive Sharpening (CAS) before video encoding.");
        ui.add_enabled_ui(s.use_sharpening, |ui| percent_slider(ui, &mut s.sharpening, 0.05..=1.0));
        ui.add_space(4.0);
        ui.checkbox(&mut s.use_fov_stencil, "Use Fov Stencil")
            .on_hover_text("Optimizes game rendering by not rendering the corners. Only uncheck when recording gameplay (requires restart of game/SteamVR)");
        // Turning the priority boost on asks first (OnBoostGamePriorityPreviewMouseDown).
        let mut boost = s.boost_priority;
        if ui
            .checkbox(&mut boost, "Boost game priority")
            .on_hover_text("Only enable if experiencing game freezes. Increases the GPU priority of games (requires restart of game/SteamVR)")
            .changed()
        {
            if boost {
                self.ask("Enabling 'Boost Game Priority' can result in a laggy experience.\nIt is recommended to leave it off.\n\nAre you sure you want to continue?", Confirm::EnableBoostPriority);
            } else {
                s.boost_priority = false;
            }
        }
        ui.add_space(14.0);
        if ui.button("Reset to Defaults").clicked() {
            s.reset_advanced();
        }
    }

    /// MEDIA tab: screenshot folder, video library root and linked video folders.
    ///
    /// # Arguments
    /// * `ui` - page UI.
    /// * `s` - settings being edited.
    pub(super) fn media_tab(&mut self, ui: &mut egui::Ui, s: &mut StreamerSettings) {
        let screenshots = folder_or(&s.screenshots_root_path, dirs::picture_dir());
        let videos = folder_or(&s.videos_root_path, dirs::video_dir());

        section(ui, "Screenshots");
        ui.horizontal(|ui| {
            ui.label(RichText::new(screenshots.display().to_string()).color(theme::TEXT_DIM));
        });
        if ui.button("Change location...").clicked() {
            if let Some(p) = pick_folder("Select the folder where your screenshots will be saved", Some(&screenshots)) {
                s.screenshots_root_path = Some(p.display().to_string());
            }
        }
        info_line(ui, "Screenshots taken in VR will be copied to this folder", theme::TEXT_DIM);
        ui.add_space(14.0);

        section(ui, "Videos");
        ui.label(RichText::new(videos.display().to_string()).color(theme::TEXT_DIM));
        if ui.button("Change location...").clicked() {
            if let Some(p) = pick_folder("Select the root folder for your videos", Some(&videos)) {
                s.videos_root_path = Some(p.display().to_string());
                self.video_folders = None;
            }
        }
        info_line(ui, "Videos from this location will be accessible in VR", theme::TEXT_DIM);
        ui.add_space(8.0);

        let folders = self.video_folders.get_or_insert_with(|| list_video_folders(&videos)).clone();
        let mut refresh = false;
        for f in &folders {
            ui.horizontal(|ui| {
                ui.label(f.link.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).on_hover_text(f.target.display().to_string());
                ui.label(RichText::new(f.target.display().to_string()).size(12.0).color(theme::TEXT_TAB));
                if remove_button(ui).clicked() {
                    if let Err(e) = remove_video_folder(&f.link) {
                        self.info(format!("Unable to remove folder:\n{e}"));
                    }
                    refresh = true;
                }
            });
        }
        if ui.button("Add video folder...").clicked() {
            if let Some(target) = pick_folder("Select a folder containing videos", None) {
                if target == videos {
                    self.info("The selected folder is already included");
                } else if let Err(e) = add_video_folder(&videos, &target) {
                    self.info(format!("Failed to add folder:\n{e}"));
                }
                refresh = true;
            }
        }
        if refresh {
            self.video_folders = None;
        }
    }

    /// Virtual-monitor choices under "Monitors" (Windows, Parsec Virtual Display Driver).
    ///
    /// # Arguments
    /// * `ui` - column UI.
    /// * `s` - settings being edited.
    fn virtual_monitor_options(&mut self, ui: &mut egui::Ui, s: &mut StreamerSettings) {
        let available = *self.virtual_driver.get_or_insert_with(vd_platform::virtual_display::is_available);
        ui.add_space(4.0);
        ui.add_enabled_ui(available, |ui| {
            combo(ui, "monitor_kind", &mut s.port.virtual_monitors, &[(false, "Physical monitors"), (true, "Virtual monitors")]);
            let labels: Vec<((u32, u32), String)> = VIRTUAL_RESOLUTIONS.iter().map(|&(w, h)| ((w, h), format!("{w} × {h}"))).collect();
            let options: Vec<((u32, u32), &str)> = labels.iter().map(|(v, l)| (*v, l.as_str())).collect();
            let mut size = s.port.virtual_resolution();
            combo(ui, "virtual_resolution", &mut size, &options);
            if size != s.port.virtual_resolution() {
                s.port.virtual_monitor_resolution = format!("{}x{}", size.0, size.1);
            }
            let hint = if s.port.virtual_monitors {
                "Only virtual monitors of this size are shown; they exist while the headset is connected"
            } else {
                "Virtual monitors of this size are added when you ask for more monitors than the PC has"
            };
            ui.label(RichText::new(hint).size(11.0).color(theme::TEXT_DIM));
        });
        if !available {
            ui.label(RichText::new("Virtual monitors need the Parsec Virtual Display Driver:\nwinget install Parsec.ParsecVDD").size(11.0).color(theme::TEXT_DIM));
        }
    }

    /// ABOUT tab: system and connection diagnostics.
    ///
    /// # Arguments
    /// * `ui` - page UI.
    /// * `s` - current settings.
    pub(super) fn about_tab(&mut self, ui: &mut egui::Ui, s: &StreamerSettings) {
        let d = self.app.state.dynamic.borrow().clone();
        let source = self.app.state.streamer.borrow().streaming_source;
        let sys = self.system.get().cloned().unwrap_or_default();
        let dash = || "--".to_owned();
        let yes_no = |b: bool| if b { "Yes" } else { "No" }.to_owned();
        // GPU name, with the encoder the port selected as a tooltip.
        let encoder = if !d.media_ready {
            "Detecting the video encoder...".to_owned()
        } else if d.adapter_name.is_empty() {
            "No working video encoder".to_owned()
        } else {
            format!("Encoder: {}", d.adapter_name)
        };
        let adapter = sys.gpu.clone().unwrap_or_else(|| if d.adapter_name.is_empty() { dash() } else { d.adapter_name.clone() });
        // C#: LatencyConverter.
        let latency_ms = self.app.stats.latency_ms.load(std::sync::atomic::Ordering::Relaxed);
        let latency = if d.is_connected && latency_ms > 0 {
            format!("{latency_ms} ms ({})", if source == StreamingSource::VR { "VR" } else { "Desktop" })
        } else {
            dash()
        };
        let live = self.stream_rates();
        if d.is_connected {
            // Keep the figures moving while the tab is open.
            ui.ctx().request_repaint_after(super::RATE_INTERVAL);
        }
        let (frame_rate, bitrate, encode) = match live {
            Some(r) if d.is_connected => (
                format!("{:.0} fps", r.fps),
                format!("{:.1} Mbit/s", r.mbps),
                format!("{:.1} ms ({})", r.encode_ms, *self.app.stats.encoder.lock().expect("poisoned")),
            ),
            _ => (dash(), dash(), dash()),
        };
        // C#: PCEthernetConverter / RoutingStatusConverter / BooleanToEnabledConverter.
        let ethernet = match d.connection_type {
            NetworkConnectionType::None | NetworkConnectionType::Wireless => "No",
            NetworkConnectionType::Ethernet => "Not Gigabit",
            NetworkConnectionType::GigabitEthernet => "Yes",
        };
        let routing = match d.routing_status {
            RoutingStatus::PublicIP => "Public IP",
            RoutingStatus::NAT => "Regular NAT",
            RoutingStatus::DoubleNAT => "Possible Double NAT",
            RoutingStatus::CGNAT => "Carrier-grade NAT",
        };
        let rows: Vec<(&str, String, Option<&str>)> = vec![
            ("Graphics Card:", adapter, Some(encoder.as_str())),
            ("Headset:", s.device_name.clone().unwrap_or_else(dash), None),
            ("Codec:", s.codec_name.clone().unwrap_or_else(dash), None),
            ("Latency:", latency, None),
            ("Frame Rate:", frame_rate, Some("Desktop frames sent to the headset per second")),
            ("Bitrate:", bitrate, Some("Video sent to the headset, all monitors")),
            ("Encode Time:", encode, Some("Time from screen capture to encoded frame, for the main monitor.\nThe ffmpeg program reports one frame interval instead of a measurement.")),
            ("Processor:", sys.processor.clone(), None),
            ("Hardware Encoder:", yes_no(d.has_hardware_encoder), Some("A hardware video encoder is required for multi-monitor or VR game streaming")),
            ("Memory:", sys.memory.clone(), None),
            ("Operating System:", sys.os.clone(), None),
            (
                "Secure Boot:",
                sys.secure_boot.map_or_else(dash, |b| if b { "Enabled" } else { "Disabled" }.to_owned()),
                Some("When Secure Boot is disabled in BIOS, it can solve compatibility with some games"),
            ),
            ("PC Ethernet:", ethernet.to_owned(), None),
            ("Auto Adjust Bitrate:", yes_no(s.auto_adjust_bitrate), None),
            ("Remote Routing:", routing.to_owned(), None),
            (
                "Microphone:",
                match &d.microphone {
                    None => dash(),
                    Some(m) if m.is_empty() => "Unavailable (no virtual microphone)".to_owned(),
                    Some(m) => m.clone(),
                },
                Some("Where microphone passthrough from the headset is sent"),
            ),
        ];
        egui::Grid::new("about").num_columns(2).spacing([16.0, 5.0]).min_col_width(140.0).show(ui, |ui| {
            for (label, value, tip) in rows {
                ui.label(RichText::new(label).color(theme::TEXT_DIM));
                let r = ui.label(value);
                if let Some(t) = tip {
                    r.on_hover_text(t);
                }
                ui.end_row();
            }
            ui.label(RichText::new("Website:").color(theme::TEXT_DIM));
            ui.hyperlink_to("www.vrdesktop.net", "https://www.vrdesktop.net");
            ui.end_row();
            ui.label(RichText::new("Discord:").color(theme::TEXT_DIM));
            ui.hyperlink_to("discord.vrdesktop.net", "https://discord.vrdesktop.net");
            ui.end_row();
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            if ui.button("Show logs").clicked() {
                // C#: OnShowLogsClick opens the log file with the default viewer.
                let path = crate::log_path();
                if let Err(e) = open::that_detached(&path) {
                    self.info(format!("Unable to open {}:\n{e}", path.display()));
                }
            }
            // C#: btnCheckForUpdates, disabled while a check runs.
            let idle = self.update_task.is_none();
            let label = if idle { "Check for updates" } else { "Checking for updates..." };
            if ui.add_enabled(idle, egui::Button::new(label)).clicked() {
                let ctx = ui.ctx().clone();
                self.check_for_updates(&ctx, true);
            }
        });
    }
}
