//! Notification-area icon.  C#: `MainWindow._notifyIcon` (`RecreateNotifyIcon`,
//! `UpdateNotifyIconAndText`, `UpdateContextMenu`).
//!
//! The icon colour and tooltip follow the connection status; the menu offers "Settings...",
//! "Disconnect" and "Exit" (the original's "Launch Game..." needs the VR runtime, which is not
//! ported).  Windows uses `tray-icon` (Shell_NotifyIcon); Linux uses the freedesktop
//! StatusNotifierItem D-Bus protocol through `ksni`, so no GTK is needed.  Events are sent to the
//! settings window, which is woken with [`egui::Context::request_repaint`] even while hidden.

use std::sync::mpsc::Sender;

use eframe::egui;

use crate::state::ConnectionStatus;

/// A request from the tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    /// Show the settings window (double click or "Settings...").
    Show,
    /// Disconnect the headset.
    Disconnect,
    /// Quit the streamer.
    Exit,
}

/// Tooltip for a connection status (C#: `UpdateNotifyIconAndText`).
///
/// # Arguments
/// * `status` - connection status.
///
/// # Returns
/// The tooltip text.
pub fn tooltip(status: ConnectionStatus) -> &'static str {
    match status {
        ConnectionStatus::Disconnected => "Virtual Desktop Streamer",
        ConnectionStatus::Connecting => "Virtual Desktop Streamer is connecting...",
        ConnectionStatus::Waiting => "Virtual Desktop Streamer is ready",
        ConnectionStatus::Connected => "Virtual Desktop Streamer is connected",
    }
}

/// Forwards a tray event to the window and wakes it.
///
/// # Arguments
/// * `tx` - window event queue.
/// * `ctx` - egui context to wake.
/// * `event` - the event.
fn send(tx: &Sender<TrayEvent>, ctx: &egui::Context, event: TrayEvent) {
    let _ = tx.send(event);
    ctx.request_repaint();
}

#[cfg(windows)]
mod imp {
    use std::sync::mpsc::Sender;

    use eframe::egui;
    use tracing::warn;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};

    use super::{TrayEvent, send, tooltip};
    use crate::gui::theme;
    use crate::state::ConnectionStatus;

    /// Windows notification-area icon.
    pub struct Tray {
        icon: TrayIcon,
        disconnect: MenuItem,
    }

    /// Builds the icon bitmap for a status.
    ///
    /// # Arguments
    /// * `status` - connection status.
    ///
    /// # Returns
    /// The icon, or `None` if the bitmap is rejected.
    fn icon(status: ConnectionStatus) -> Option<Icon> {
        Icon::from_rgba(theme::icon_rgba(32, theme::status_color(status)), 32, 32).ok()
    }

    impl Tray {
        /// Creates the icon.  Must run on the thread of the window event loop.
        ///
        /// # Arguments
        /// * `tx` - window event queue.
        /// * `ctx` - egui context to wake.
        ///
        /// # Returns
        /// The icon, or `None` if the shell refused it (logged).
        pub fn new(tx: Sender<TrayEvent>, ctx: egui::Context) -> Option<Self> {
            let menu = Menu::new();
            let settings = MenuItem::new("Settings...", true, None);
            let disconnect = MenuItem::new("Disconnect", false, None);
            let exit = MenuItem::new("Exit", true, None);
            if let Err(e) = menu.append_items(&[&settings, &PredefinedMenuItem::separator(), &disconnect, &PredefinedMenuItem::separator(), &exit]) {
                warn!(error = %e, "cannot build the tray menu");
            }
            let ids = (settings.id().clone(), disconnect.id().clone(), exit.id().clone());
            let (menu_tx, menu_ctx) = (tx.clone(), ctx.clone());
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                let event = if e.id == ids.0 {
                    TrayEvent::Show
                } else if e.id == ids.1 {
                    TrayEvent::Disconnect
                } else if e.id == ids.2 {
                    TrayEvent::Exit
                } else {
                    return;
                };
                send(&menu_tx, &menu_ctx, event);
            }));
            TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
                if let TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = e {
                    send(&tx, &ctx, TrayEvent::Show);
                }
            }));
            let mut builder = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip(tooltip(ConnectionStatus::Disconnected));
            if let Some(i) = icon(ConnectionStatus::Disconnected) {
                builder = builder.with_icon(i);
            }
            match builder.build() {
                Ok(icon) => Some(Self { icon, disconnect }),
                Err(e) => {
                    warn!(error = %e, "cannot create the notification-area icon");
                    None
                }
            }
        }

        /// Updates icon, tooltip and menu for a new status.
        ///
        /// # Arguments
        /// * `status` - connection status.
        pub fn set_status(&mut self, status: ConnectionStatus) {
            let _ = self.icon.set_icon(icon(status));
            let _ = self.icon.set_tooltip(Some(tooltip(status)));
            self.disconnect.set_enabled(status == ConnectionStatus::Connected);
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::sync::mpsc::Sender;

    use eframe::egui;
    use ksni::menu::{MenuItem, StandardItem};
    use ksni::{Handle, Icon, TrayMethods};
    use tracing::warn;

    use super::{TrayEvent, send, tooltip};
    use crate::gui::theme;
    use crate::state::ConnectionStatus;

    /// StatusNotifierItem model.
    struct Sni {
        tx: Sender<TrayEvent>,
        ctx: egui::Context,
        status: ConnectionStatus,
    }

    impl ksni::Tray for Sni {
        /// D-Bus id of the item.
        fn id(&self) -> String {
            "virtual-desktop-streamer".into()
        }

        /// Title shown by the host.
        fn title(&self) -> String {
            "Virtual Desktop Streamer".into()
        }

        /// Left click opens the settings window.
        fn activate(&mut self, _x: i32, _y: i32) {
            send(&self.tx, &self.ctx, TrayEvent::Show);
        }

        /// Icon in ARGB32 (network byte order).
        fn icon_pixmap(&self) -> Vec<Icon> {
            let mut data = theme::icon_rgba(32, theme::status_color(self.status));
            for px in data.chunks_exact_mut(4) {
                px.rotate_right(1); // RGBA → ARGB
            }
            vec![Icon { width: 32, height: 32, data }]
        }

        /// Tooltip text.
        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip { title: tooltip(self.status).into(), ..Default::default() }
        }

        /// Context menu.
        fn menu(&self) -> Vec<MenuItem<Self>> {
            vec![
                StandardItem { label: "Settings...".into(), activate: Box::new(|t: &mut Self| send(&t.tx, &t.ctx, TrayEvent::Show)), ..Default::default() }.into(),
                MenuItem::Separator,
                StandardItem {
                    label: "Disconnect".into(),
                    enabled: self.status == ConnectionStatus::Connected,
                    activate: Box::new(|t: &mut Self| send(&t.tx, &t.ctx, TrayEvent::Disconnect)),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem { label: "Exit".into(), activate: Box::new(|t: &mut Self| send(&t.tx, &t.ctx, TrayEvent::Exit)), ..Default::default() }.into(),
            ]
        }
    }

    /// Linux tray icon handle.
    pub struct Tray {
        handle: Handle<Sni>,
        rt: tokio::runtime::Handle,
    }

    impl Tray {
        /// Registers the item with the session's StatusNotifierWatcher.
        ///
        /// # Arguments
        /// * `tx` - window event queue.
        /// * `ctx` - egui context to wake.
        /// * `rt` - runtime driving the D-Bus connection.
        ///
        /// # Returns
        /// The icon, or `None` when no tray host is running (logged).
        pub fn new(tx: Sender<TrayEvent>, ctx: egui::Context, rt: &tokio::runtime::Handle) -> Option<Self> {
            let sni = Sni { tx, ctx, status: ConnectionStatus::Disconnected };
            match rt.block_on(sni.spawn()) {
                Ok(handle) => Some(Self { handle, rt: rt.clone() }),
                Err(e) => {
                    warn!(error = %e, "no notification-area host; closing the window will quit");
                    None
                }
            }
        }

        /// Updates icon, tooltip and menu for a new status.
        ///
        /// # Arguments
        /// * `status` - connection status.
        pub fn set_status(&mut self, status: ConnectionStatus) {
            let handle = self.handle.clone();
            self.rt.spawn(async move {
                handle.update(|t| t.status = status).await;
            });
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use std::sync::mpsc::Sender;

    use eframe::egui;

    use super::TrayEvent;
    use crate::state::ConnectionStatus;

    /// No tray icon on this platform.
    pub struct Tray;

    impl Tray {
        /// Always `None`.
        ///
        /// # Returns
        /// `None`.
        pub fn new(_tx: Sender<TrayEvent>, _ctx: egui::Context) -> Option<Self> {
            None
        }

        /// No-op.
        pub fn set_status(&mut self, _status: ConnectionStatus) {}
    }
}

pub use imp::Tray;
