//! Settings window.  C#: `VirtualDesktop.Streamer.MainWindow` (`MainWindow.xaml`), its
//! notification-area icon and the `AddAccountWindow` / `MessageBoxWindow` dialogs.
//!
//! Layout follows the original: a borderless 700×516 dark window with a logo header and
//! minimise / close buttons, a tab strip on the left (ACCOUNTS, BINDINGS, OPTIONS, ADVANCED,
//! MEDIA, ABOUT, see [`tabs`]) and a footer with the version and the "Paste URL" video download.
//! Closing the window hides it in the notification area ([`tray`]); "Exit" in the tray menu
//! quits.
//!
//! The streamer itself ([`crate::service`]) runs on the Tokio runtime while the window owns the
//! main thread.  Every frame the window edits a copy of [`StreamerSettings`] and hands changes to
//! [`App::update_settings`]; they are written to disk shortly after the last change.

mod tabs;
mod theme;
mod tray;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use eframe::egui::{self, Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke, Vec2, ViewportCommand};
use tokio::runtime::Runtime;
use tracing::{info, warn};
use vd_protocol::types::{DownloadState, Platform};

use crate::app::App;
use crate::bindings::BindingSettings;
use crate::hotkeys::Hotkeys;
use crate::settings::StreamerSettings;
use crate::state::ConnectionStatus;
use crate::{hotkeys, service, youtube};
use tray::{Tray, TrayEvent};

/// Window size of the original (`Width="700" Height="516"`, also the minimum size).
const WINDOW_SIZE: Vec2 = Vec2::new(700.0, 516.0);
/// Settings are written to disk this long after the last change (sliders change continuously).
const SAVE_DELAY: Duration = Duration::from_millis(600);
/// Loopback port owned by the running instance; a second launch connects to it to bring the
/// window to the front and exits (C#: single-instance `App` with a named mutex).
const INSTANCE_PORT: u16 = 38809;

/// The tabs, in the order of the original `TabControl` (`StreamerSettings.SelectedTab`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Accounts,
    Bindings,
    Options,
    Advanced,
    Media,
    About,
}

impl Tab {
    /// All tabs in display order.
    const ALL: [Tab; 6] = [Tab::Accounts, Tab::Bindings, Tab::Options, Tab::Advanced, Tab::Media, Tab::About];

    /// Header text.
    ///
    /// # Returns
    /// The upper-case title.
    fn title(self) -> &'static str {
        match self {
            Tab::Accounts => "ACCOUNTS",
            Tab::Bindings => "BINDINGS",
            Tab::Options => "OPTIONS",
            Tab::Advanced => "ADVANCED",
            Tab::Media => "MEDIA",
            Tab::About => "ABOUT",
        }
    }

    /// Tab for a persisted index.
    ///
    /// # Arguments
    /// * `i` - `SelectedTab`.
    ///
    /// # Returns
    /// The tab (ACCOUNTS when out of range).
    fn from_index(i: i32) -> Self {
        usize::try_from(i).ok().and_then(|i| Self::ALL.get(i).copied()).unwrap_or(Tab::Accounts)
    }

    /// Persisted index.
    ///
    /// # Returns
    /// Position in [`Tab::ALL`].
    fn index(self) -> i32 {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0) as i32
    }
}

/// An action awaiting confirmation in a message box.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Confirm {
    /// Uncheck "Automatically adjust bitrate" (`OnAutoAdjustBitratePreviewMouseDown`).
    DisableAutoBitrate,
    /// Check "Boost game priority" (`OnBoostGamePriorityPreviewMouseDown`).
    EnableBoostPriority,
    /// Edit the accounts while a headset is connected (`OnChangeClick`).
    ChangeAccounts,
    /// Add the account of a headset that searched the LAN (C#: the pairing prompt of
    /// `_0095_0002._0002`).
    AllowAccount(Platform, String),
}

/// A message box (C#: `MessageBoxWindow`).
#[derive(Debug, Clone)]
struct Dialog {
    /// Message text.
    text: String,
    /// `Some` for Yes/No questions, `None` for an OK-only notice.
    confirm: Option<Confirm>,
    /// State of a "Don't ask again" check box, when the message box has one.
    dont_ask: Option<bool>,
}

/// State of the "Add Account" dialog (C#: `AddAccountWindow`).
#[derive(Debug, Clone)]
struct AddAccount {
    /// Platform chosen in the "Add account" menu.
    platform: Platform,
    /// Account name being typed.
    name: String,
    /// Validation error shown under the text box.
    error: Option<String>,
    /// The text box has been given keyboard focus once.
    focused: bool,
}

/// Machine description for the ABOUT tab (C#: `UISettings.ProcessorName` / `Memory` /
/// `OSVersion`, `DynamicSettings.SecureBoot`), gathered once in the background.
#[derive(Debug, Clone, Default)]
struct SystemInfo {
    /// CPU brand string.
    processor: String,
    /// Installed memory, e.g. `32 GB`.
    memory: String,
    /// Operating system name and version.
    os: String,
    /// Secure Boot state, if known.
    secure_boot: Option<bool>,
    /// Name of the desktop GPU.
    gpu: Option<String>,
}

impl SystemInfo {
    /// Queries the system (takes a few hundred milliseconds on Windows).
    ///
    /// # Returns
    /// The description.
    fn gather() -> Self {
        use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};
        let sys = System::new_with_specifics(RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing()).with_memory(MemoryRefreshKind::everything()));
        let processor = sys.cpus().first().map(|c| c.brand().trim().to_owned()).unwrap_or_default();
        let gib = sys.total_memory() as f64 / (1024.0 * 1024.0 * 1024.0);
        Self {
            processor,
            memory: format!("{:.0} GB", gib.round()),
            os: System::long_os_version().unwrap_or_else(|| std::env::consts::OS.to_owned()),
            secure_boot: vd_platform::system::secure_boot(),
            gpu: vd_platform::system::gpu_name(),
        }
    }
}

/// Desktop streaming rates measured from [`crate::app::StreamStats`].
#[derive(Debug, Clone, Copy)]
struct StreamRates {
    /// When the counters were read.
    at: Instant,
    /// Frame counter at `at`.
    frames: u64,
    /// Byte counter at `at`.
    bytes: u64,
    /// Frames per second over the last interval.
    fps: f64,
    /// Megabits per second over the last interval.
    mbps: f64,
    /// Encode time of the last frame, in milliseconds.
    encode_ms: f64,
}

/// Interval over which [`StreamRates`] are averaged.
const RATE_INTERVAL: Duration = Duration::from_secs(1);

/// A video-library folder link (C#: `DynamicSettings.VideoFolders`).
#[derive(Debug, Clone)]
struct VideoFolder {
    /// Link inside the videos root.
    link: PathBuf,
    /// Folder it points to.
    target: PathBuf,
}

/// The settings window (eframe application).
struct SettingsWindow {
    /// Streamer context.
    app: Arc<App>,
    /// Runtime handle for async work started from the UI.
    rt: tokio::runtime::Handle,
    /// Selected tab.
    tab: Tab,
    /// Last settings change not yet written to disk.
    dirty_since: Option<Instant>,
    /// Keyboard shortcuts being edited.
    bindings: BindingSettings,
    /// Registered system-wide hotkeys.
    hotkeys: Option<Hotkeys>,
    /// Shortcuts that could not be registered.
    hotkey_errors: Vec<String>,
    /// Account list is editable (`DynamicSettings.IsEditingAccounts`).
    editing_accounts: bool,
    /// Open "Add Account" dialog.
    add_account: Option<AddAccount>,
    /// Open message box.
    dialog: Option<Dialog>,
    /// Notification-area icon (`None` when the platform has no tray).
    tray: Option<Tray>,
    /// Events from the tray icon.
    tray_rx: Receiver<TrayEvent>,
    /// Keeps the tray channel alive when no tray exists.
    _tray_tx: Sender<TrayEvent>,
    /// Status last shown by the tray icon.
    shown_status: Option<ConnectionStatus>,
    /// The window is visible (not hidden in the tray).
    visible: bool,
    /// Exit was requested; the next close is not intercepted.
    quitting: bool,
    /// ABOUT tab data, filled by a background thread.
    system: Arc<OnceLock<SystemInfo>>,
    /// Cached video-library folders.
    video_folders: Option<Vec<VideoFolder>>,
    /// Cached microphone choices: Virtual Desktop driver present, and cable playback devices.
    mic_targets: Option<(bool, Vec<(String, String)>)>,
    /// Register again once this frame's settings are applied (an account was added).
    reconnect_after_commit: bool,
    /// Last streaming rates shown in ABOUT.
    rates: Option<StreamRates>,
}

impl SettingsWindow {
    /// Builds the window state; runs on the event-loop thread inside eframe's app creator.
    ///
    /// # Arguments
    /// * `cc` - eframe creation context.
    /// * `app` - streamer context.
    /// * `rt` - runtime handle.
    /// * `hotkey_tx` - sink for triggered hotkey actions.
    /// * `events` - window event queue (tray icon and second launches) and its sender.
    /// * `ctx_slot` - receives the egui context so other threads can wake the window.
    /// * `start_hidden` - start in the notification area.
    ///
    /// # Returns
    /// The window.
    fn new(
        cc: &eframe::CreationContext<'_>,
        app: Arc<App>,
        rt: tokio::runtime::Handle,
        hotkey_tx: tokio::sync::mpsc::UnboundedSender<crate::bindings::BindingAction>,
        events: (Sender<TrayEvent>, Receiver<TrayEvent>),
        ctx_slot: &OnceLock<egui::Context>,
        start_hidden: bool,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();
        let _ = ctx_slot.set(ctx.clone());
        theme::apply(&ctx);
        let (tray_tx, tray_rx) = events;
        #[cfg(target_os = "linux")]
        let tray = Tray::new(tray_tx.clone(), ctx.clone(), &rt);
        #[cfg(not(target_os = "linux"))]
        let tray = Tray::new(tray_tx.clone(), ctx.clone());
        if start_hidden && tray.is_none() {
            // Nowhere to hide: start minimised instead.
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
        let bindings = BindingSettings::load();
        let mut hotkeys = Hotkeys::new(hotkey_tx);
        let hotkey_errors = hotkeys.as_mut().map(|h| h.apply(&bindings)).unwrap_or_default();
        // Repaint (or run `logic` while hidden) whenever the streamer state changes.
        {
            let (app, ctx) = (Arc::clone(&app), ctx.clone());
            rt.spawn(async move {
                let mut d = app.state.dynamic.subscribe();
                let mut s = app.state.streamer.subscribe();
                let mut shutdown = app.shutdown.subscribe();
                loop {
                    tokio::select! {
                        r = d.changed() => if r.is_err() { return },
                        r = s.changed() => if r.is_err() { return },
                        () = app.pairing_notify.notified() => {}
                        _ = shutdown.changed() => {}
                    }
                    ctx.request_repaint();
                }
            });
        }
        let system: Arc<OnceLock<SystemInfo>> = Arc::default();
        {
            let (system, ctx) = (Arc::clone(&system), ctx.clone());
            std::thread::spawn(move || {
                let _ = system.set(SystemInfo::gather());
                ctx.request_repaint();
            });
        }
        let settings = app.settings.lock().expect("poisoned").clone();
        Self {
            tab: Tab::from_index(settings.selected_tab),
            editing_accounts: settings.access_tokens().is_empty(),
            app,
            rt,
            dirty_since: None,
            bindings,
            hotkeys,
            hotkey_errors,
            add_account: None,
            dialog: None,
            tray,
            tray_rx,
            _tray_tx: tray_tx,
            shown_status: None,
            visible: !start_hidden,
            quitting: false,
            system,
            video_folders: None,
            mic_targets: None,
            reconnect_after_commit: false,
            rates: None,
        }
    }

    /// Streaming rates for ABOUT, refreshed every [`RATE_INTERVAL`] while the tab is shown.
    ///
    /// # Returns
    /// The rates, or `None` until two readings exist or when no desktop frame was sent in the
    /// last interval.
    fn stream_rates(&mut self) -> Option<StreamRates> {
        use std::sync::atomic::Ordering;
        let now = Instant::now();
        let stats = &self.app.stats;
        let (frames, bytes) = (stats.frames.load(Ordering::Relaxed), stats.bytes.load(Ordering::Relaxed));
        let encode_ms = f64::from(stats.encode_us.load(Ordering::Relaxed)) / 1000.0;
        let due = self.rates.is_none_or(|r| now - r.at >= RATE_INTERVAL);
        if due {
            let (fps, mbps) = match self.rates {
                Some(r) => {
                    let secs = (now - r.at).as_secs_f64();
                    ((frames - r.frames) as f64 / secs, (bytes - r.bytes) as f64 * 8.0 / secs / 1e6)
                }
                None => (0.0, 0.0),
            };
            self.rates = Some(StreamRates { at: now, frames, bytes, fps, mbps, encode_ms });
        }
        self.rates.filter(|r| r.fps > 0.0)
    }

    /// Shows a notice with an OK button.
    ///
    /// # Arguments
    /// * `text` - message.
    fn info(&mut self, text: impl Into<String>) {
        self.dialog = Some(Dialog { text: text.into(), confirm: None, dont_ask: None });
    }

    /// Asks a Yes/No question.
    ///
    /// # Arguments
    /// * `text` - question.
    /// * `confirm` - action performed on "Yes".
    fn ask(&mut self, text: impl Into<String>, confirm: Confirm) {
        self.dialog = Some(Dialog { text: text.into(), confirm: Some(confirm), dont_ask: None });
    }

    /// Opens the prompt for the next queued pairing request, bringing the window up from the
    /// notification area (C#: the pairing `MessageBox`, owned by the main window).
    ///
    /// # Arguments
    /// * `ctx` - egui context.
    fn next_pairing_request(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() || self.add_account.is_some() {
            return;
        }
        loop {
            let Some(r) = self.app.pairing_requests.lock().expect("poisoned").pop_front() else { return };
            // The account may have been added, or asking turned off, since it was queued.
            let s = self.app.settings.lock().expect("poisoned").clone();
            if !s.show_pairing_requests || !crate::pairing::can_add_account(&s, r.platform, &r.account_id) {
                continue;
            }
            let text = format!("Allow {} user '{}' to access this computer?", crate::pairing::platform_label(r.platform), r.account_id);
            self.dialog = Some(Dialog { text, confirm: Some(Confirm::AllowAccount(r.platform, r.account_id)), dont_ask: Some(false) });
            self.show(ctx);
            return;
        }
    }

    /// Hides the window in the notification area, or minimises it when there is no tray.
    ///
    /// # Arguments
    /// * `ctx` - egui context.
    fn hide(&mut self, ctx: &egui::Context) {
        if self.tray.is_some() {
            self.visible = false;
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        } else {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
    }

    /// Restores and focuses the window (C#: `ShowWindow`).
    ///
    /// # Arguments
    /// * `ctx` - egui context.
    fn show(&mut self, ctx: &egui::Context) {
        self.visible = true;
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    /// Quits the application.
    ///
    /// # Arguments
    /// * `ctx` - egui context.
    fn quit(&mut self, ctx: &egui::Context) {
        self.quitting = true;
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    /// Disconnects the headset (tray "Disconnect", C#: `OnDisconnectClick`).
    fn disconnect(&self) {
        let app = Arc::clone(&self.app);
        self.rt.spawn(async move { app.channels.messaging.reset().await });
    }

    /// Applies an edited copy of the settings and performs the side effects of changed options.
    ///
    /// # Arguments
    /// * `before` - settings at the start of the frame.
    /// * `after` - settings after the widgets ran.
    fn commit(&mut self, before: &StreamerSettings, after: StreamerSettings) {
        if before.start_with_windows != after.start_with_windows {
            if let Err(e) = vd_platform::system::set_autostart(after.start_with_windows, &[]) {
                warn!(error = %e, "cannot change the log-in autostart entry");
                self.info(format!("Unable to change the startup setting:\n{e}"));
            }
        }
        if before.encrypt_local_traffic != after.encrypt_local_traffic {
            let d = self.app.state.dynamic.borrow().clone();
            if d.is_connected && d.encrypted != after.encrypt_local_traffic {
                self.info("You will need to disconnect and re-connect to your computer for this change to take effect.");
            }
        }
        self.app.update_settings(after);
        self.dirty_since = Some(Instant::now());
    }

    /// Writes pending settings once [`SAVE_DELAY`] has passed since the last change.
    ///
    /// # Arguments
    /// * `ctx` - egui context (to schedule the next check).
    fn save_if_due(&mut self, ctx: &egui::Context) {
        if let Some(t) = self.dirty_since {
            if t.elapsed() >= SAVE_DELAY {
                self.app.save_settings();
                self.dirty_since = None;
            } else {
                ctx.request_repaint_after(SAVE_DELAY.saturating_sub(t.elapsed()));
            }
        }
    }

    /// Persists the shortcuts and re-registers the hotkeys.
    fn apply_bindings(&mut self) {
        if let Err(e) = self.bindings.save() {
            warn!(error = %e, "cannot save the keyboard shortcuts");
        }
        if let Some(h) = self.hotkeys.as_mut() {
            self.hotkey_errors = h.apply(&self.bindings);
        }
    }

    /// Draws the header: logo, connection indicator, minimise and close buttons.
    ///
    /// # Arguments
    /// * `ui` - header area.
    fn title_bar(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        // Dragging the header moves the window (C#: `HeaderMouseLeftButtonDown`).  The drag area
        // stops short of the status dot and buttons so it never competes with their clicks (the
        // OS move loop would swallow the mouse-up).
        let drag_rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - 110.0, rect.bottom()));
        if ui.interact(drag_rect, ui.scope_id().with("drag"), Sense::drag()).drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        ui.horizontal_centered(|ui| {
            ui.add_space(16.0);
            ui.label(RichText::new("VIRTUAL DESKTOP").size(20.0).strong().color(theme::TEXT));
            ui.label(RichText::new("STREAMER").size(12.0).color(theme::TEXT_TAB));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(4.0);
                if chrome_button(ui, ChromeGlyph::Close).on_hover_text("Close to the notification area").clicked() {
                    // C#: OnCloseButtonClick → HideWindow(minimizeToTray: true).
                    let ctx = ui.ctx().clone();
                    self.hide(&ctx);
                }
                if chrome_button(ui, ChromeGlyph::Minimize).on_hover_text("Minimize").clicked() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
                }
                ui.add_space(12.0);
                let status = self.app.state.dynamic.borrow().status;
                let (dot, resp) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 5.0, theme::status_color(status));
                resp.on_hover_text(tray::tooltip(status));
            });
        });
    }

    /// Draws the tab strip.
    ///
    /// # Arguments
    /// * `ui` - tab strip area.
    /// * `settings` - settings (the selected tab is persisted).
    fn tab_strip(&mut self, ui: &mut egui::Ui, settings: &mut StreamerSettings) {
        ui.spacing_mut().item_spacing.y = 0.0;
        for tab in Tab::ALL {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 48.0), Sense::click());
            let selected = self.tab == tab;
            let painter = ui.painter();
            if resp.hovered() && !selected {
                painter.rect_filled(rect, CornerRadius::ZERO, theme::CONTROL);
            }
            if selected {
                painter.rect_filled(rect, CornerRadius::ZERO, theme::BG);
                painter.rect_filled(egui::Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())), CornerRadius::ZERO, theme::ACCENT);
            }
            let color = if selected || resp.hovered() { theme::TEXT } else { theme::TEXT_TAB };
            painter.text(rect.left_center() + Vec2::new(24.0, 0.0), egui::Align2::LEFT_CENTER, tab.title(), egui::FontId::proportional(16.0), color);
            if resp.clicked() {
                self.tab = tab;
                settings.selected_tab = tab.index();
                if tab == Tab::Media {
                    self.video_folders = None;
                }
                if tab == Tab::Options {
                    self.mic_targets = None;
                }
            }
        }
    }

    /// Draws the footer: version on the left, video download on the right.
    ///
    /// # Arguments
    /// * `ui` - footer area.
    fn footer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.add_space(16.0);
            ui.label(RichText::new("Version:").color(theme::TEXT_TAB));
            ui.label(format!("{} (port of 1.34.18)", env!("CARGO_PKG_VERSION")));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(16.0);
                let (state, progress) = {
                    let s = self.app.state.streamer.borrow();
                    (s.download_state, s.download_progress)
                };
                match state {
                    DownloadState::None | DownloadState::CanPaste => {
                        if ui.button("Paste URL").on_hover_text("Download a YouTube video from the URL in the clipboard (Ctrl+V)").clicked() {
                            self.paste_url();
                        }
                    }
                    DownloadState::RetrievingInfo | DownloadState::Downloading => {
                        if ui.button("Cancel").clicked() {
                            youtube::cancel(&self.app);
                        }
                        if state == DownloadState::Downloading {
                            ui.add(egui::ProgressBar::new(progress).desired_width(140.0).show_percentage());
                        }
                        // C#: DownloadStateToStringConverter.
                        ui.label(if state == DownloadState::Downloading { "Downloading..." } else { "Retrieving video information..." });
                    }
                }
            });
        });
    }

    /// Starts a download from the clipboard URL (C#: `OnPasteUrlClick`).
    fn paste_url(&mut self) {
        let text = arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default();
        if !youtube::is_youtube_url(&text) {
            self.info("Copy a YouTube video URL to the clipboard first.");
            return;
        }
        let _guard = self.rt.enter();
        if let Err(e) = youtube::start(&self.app, &text) {
            self.info(e);
        }
    }

    /// Draws the open message box and performs a confirmed action.
    ///
    /// # Arguments
    /// * `ctx` - egui context.
    /// * `settings` - settings edited this frame.
    fn dialog(&mut self, ctx: &egui::Context, settings: &mut StreamerSettings) {
        let Some(dialog) = self.dialog.clone() else { return };
        let mut answer: Option<bool> = None;
        let mut dont_ask = dialog.dont_ask;
        egui::Modal::new(egui::Id::unique("message_box")).frame(dialog_frame()).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(&dialog.text);
            if let Some(d) = dont_ask.as_mut() {
                ui.add_space(8.0);
                ui.checkbox(d, "Don't ask again");
            }
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if dialog.confirm.is_some() {
                    if ui.add_sized([80.0, 26.0], egui::Button::new("No")).clicked() {
                        answer = Some(false);
                    }
                    if ui.add_sized([80.0, 26.0], egui::Button::new("Yes")).clicked() {
                        answer = Some(true);
                    }
                } else if ui.add_sized([80.0, 26.0], egui::Button::new("OK")).clicked() {
                    answer = Some(true);
                }
            });
        });
        let Some(yes) = answer else {
            if let Some(d) = self.dialog.as_mut() {
                d.dont_ask = dont_ask;
            }
            return;
        };
        self.dialog = None;
        if let Some(d) = dont_ask {
            // C#: ShowPairingRequests = !checkbox, whatever the answer.
            settings.show_pairing_requests = !d;
        }
        if !yes {
            return;
        }
        match dialog.confirm {
            Some(Confirm::DisableAutoBitrate) => settings.auto_adjust_bitrate = false,
            Some(Confirm::EnableBoostPriority) => settings.boost_priority = true,
            Some(Confirm::ChangeAccounts) => {
                self.editing_accounts = true;
                self.disconnect();
            }
            Some(Confirm::AllowAccount(platform, name)) => {
                if crate::pairing::can_add_account(settings, platform, &name) {
                    settings.add_account(platform, &name);
                    self.editing_accounts = false;
                    // C#: save, then register again unless a headset is connected.
                    self.reconnect_after_commit = !self.app.state.dynamic.borrow().is_connected;
                }
            }
            None => {}
        }
    }

    /// Handles tray events and streamer shutdown; runs even while the window is hidden.
    ///
    /// # Arguments
    /// * `ctx` - egui context.
    fn handle_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.tray_rx.try_recv() {
            match ev {
                TrayEvent::Show => self.show(ctx),
                TrayEvent::Disconnect => self.disconnect(),
                TrayEvent::Exit => self.quit(ctx),
            }
        }
        let status = self.app.state.dynamic.borrow().status;
        if self.shown_status != Some(status) {
            self.shown_status = Some(status);
            if let Some(t) = self.tray.as_mut() {
                t.set_status(status);
            }
        }
        self.next_pairing_request(ctx);
        // A headset `Exit` message stops the streamer; close the window with it.
        if self.app.is_shutting_down() && !self.quitting {
            self.quit(ctx);
        }
        // Closing from the task bar / Alt+F4 hides to the tray like the close button.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.hide(ctx);
        }
        self.save_if_due(ctx);
    }
}

impl eframe::App for SettingsWindow {
    /// Event handling shared by visible and hidden states.
    ///
    /// # Arguments
    /// * `ctx` - egui context.
    /// * `_frame` - eframe frame.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_events(ctx);
    }

    /// Draws the window.
    ///
    /// # Arguments
    /// * `ui` - root UI.
    /// * `_frame` - eframe frame.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let before = self.app.settings.lock().expect("poisoned").clone();
        let mut settings = before.clone();

        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::V)) && !ctx.egui_wants_keyboard_input() {
            self.paste_url();
        }

        egui::Panel::top("title").exact_size(52.0).frame(Frame::NONE.fill(theme::PANEL)).show(ui, |ui| self.title_bar(ui));
        egui::Panel::bottom("footer").exact_size(40.0).frame(Frame::NONE.fill(theme::PANEL)).show(ui, |ui| self.footer(ui));
        egui::Panel::left("tabs").exact_size(160.0).resizable(false).frame(Frame::NONE.fill(theme::PANEL)).show(ui, |ui| {
            ui.add_space(8.0);
            self.tab_strip(ui, &mut settings);
        });
        egui::CentralPanel::default().frame(Frame::NONE.fill(theme::BG).inner_margin(Margin { left: 24, right: 16, top: 18, bottom: 8 })).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
                Tab::Accounts => self.accounts_tab(ui, &mut settings),
                Tab::Bindings => self.bindings_tab(ui),
                Tab::Options => self.options_tab(ui, &mut settings),
                Tab::Advanced => self.advanced_tab(ui, &mut settings),
                Tab::Media => self.media_tab(ui, &mut settings),
                Tab::About => self.about_tab(ui, &settings),
            });
        });
        self.add_account_dialog(&ctx, &mut settings);
        self.dialog(&ctx, &mut settings);

        if settings != before {
            self.commit(&before, settings);
        }
        if std::mem::take(&mut self.reconnect_after_commit) {
            self.app.save_settings();
            self.dirty_since = None;
            self.app.restart_connection();
        }
    }

    /// Saves pending settings when the window closes.
    ///
    /// # Arguments
    /// * `_gl` - GL context (unused).
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.dirty_since.take().is_some() {
            self.app.save_settings();
        }
    }

    /// Background colour behind the panels.
    ///
    /// # Arguments
    /// * `_visuals` - current visuals.
    ///
    /// # Returns
    /// The window background colour.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::BG.to_normalized_gamma_f32()
    }
}

/// Glyph of a header button.
#[derive(Clone, Copy)]
enum ChromeGlyph {
    /// Horizontal bar.
    Minimize,
    /// Cross.
    Close,
}

/// Draws a 32×32 header button (C#: `Minimize32.png` / `Close32.png`).
///
/// # Arguments
/// * `ui` - header UI.
/// * `glyph` - glyph to draw.
///
/// # Returns
/// The button response.
fn chrome_button(ui: &mut egui::Ui, glyph: ChromeGlyph) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
    let painter = ui.painter();
    if resp.hovered() {
        let fill = if matches!(glyph, ChromeGlyph::Close) { theme::ERROR } else { theme::CONTROL_HOVER };
        painter.rect_filled(rect, CornerRadius::ZERO, fill);
    }
    let stroke = Stroke::new(1.5, if resp.hovered() { Color32::WHITE } else { theme::TEXT_DIM });
    let c = rect.center();
    match glyph {
        ChromeGlyph::Minimize => {
            painter.line_segment([c + Vec2::new(-6.0, 0.0), c + Vec2::new(6.0, 0.0)], stroke);
        }
        ChromeGlyph::Close => {
            painter.line_segment([c + Vec2::new(-6.0, -6.0), c + Vec2::new(6.0, 6.0)], stroke);
            painter.line_segment([c + Vec2::new(-6.0, 6.0), c + Vec2::new(6.0, -6.0)], stroke);
        }
    }
    resp
}

/// Frame of modal dialogs.
///
/// # Returns
/// A bordered panel-coloured frame.
fn dialog_frame() -> Frame {
    Frame::NONE.fill(theme::PANEL).stroke(Stroke::new(1.0, theme::BORDER)).inner_margin(Margin::same(18))
}

/// Makes this process the single running instance.
///
/// # Returns
/// The listener that later launches connect to, or `None` when another instance already runs
/// (it has then been asked to show its window).
pub fn claim_instance() -> Option<std::net::TcpListener> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], INSTANCE_PORT));
    match std::net::TcpListener::bind(addr) {
        Ok(l) => Some(l),
        Err(e) => {
            // Connecting is the "show" request; if nobody answers, run anyway.
            if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok() {
                None
            } else {
                warn!(error = %e, "cannot claim the single-instance port; continuing");
                std::net::TcpListener::bind(("127.0.0.1", 0)).ok()
            }
        }
    }
}

/// Opens the settings window and runs the streamer until the user exits.
///
/// # Arguments
/// * `rt` - runtime for the streamer tasks.
/// * `minimized` - `--minimized` was given.
/// * `listener` - single-instance listener from [`claim_instance`].
///
/// # Errors
/// When no window can be created (no display, no OpenGL).
pub fn run(rt: &Runtime, minimized: bool, listener: std::net::TcpListener) -> Result<()> {
    vd_platform::system::detach_own_console();
    let _guard = rt.enter();
    let settings = StreamerSettings::load();
    let start_hidden = minimized || settings.start_minimized_in_tray;
    let app = service::start(settings, false);
    let (hotkey_tx, hotkey_rx) = tokio::sync::mpsc::unbounded_channel();
    rt.spawn(hotkeys::run(Arc::clone(&app), hotkey_rx));

    let viewport = egui::ViewportBuilder::default()
        .with_title("Virtual Desktop Streamer")
        .with_app_id("virtual-desktop-streamer")
        .with_inner_size(WINDOW_SIZE)
        .with_min_inner_size(WINDOW_SIZE)
        .with_decorations(false)
        .with_resizable(true)
        .with_visible(!start_hidden)
        .with_icon(theme::window_icon());
    let options = eframe::NativeOptions { viewport, centered: true, ..Default::default() };
    let window_app = Arc::clone(&app);
    let handle = rt.handle().clone();
    let (event_tx, event_rx) = channel();
    let ctx_slot: Arc<OnceLock<egui::Context>> = Arc::default();
    {
        // Later launches connect here to bring this window to the front.
        let (tx, slot) = (event_tx.clone(), Arc::clone(&ctx_slot));
        std::thread::spawn(move || {
            for _ in listener.incoming().flatten() {
                let _ = tx.send(TrayEvent::Show);
                if let Some(ctx) = slot.get() {
                    ctx.request_repaint();
                }
            }
        });
    }
    let result = eframe::run_native(
        "Virtual Desktop Streamer",
        options,
        Box::new(move |cc| Ok(Box::new(SettingsWindow::new(cc, window_app, handle, hotkey_tx, (event_tx, event_rx), &ctx_slot, start_hidden)))),
    );
    rt.block_on(service::stop(&app));
    info!("settings window closed");
    result.map_err(|e| anyhow!("cannot open the settings window: {e}"))
}
