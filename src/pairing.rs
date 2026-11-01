//! "Ask for computer access": prompts when a headset with an unknown account searches the LAN.
//!
//! C#: `_0095_0002._0002` — the pairing event handler of the LAN discovery service and its
//! message box "Allow Meta user 'X' to access this computer?".  Requests are received by
//! [`vd_net::pairing`]; this module filters them and queues the new ones for the settings window.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use vd_net::NetError;
use vd_net::pairing::DiscoveryRequest;
use vd_protocol::types::Platform;

use crate::app::App;
use crate::settings::StreamerSettings;

/// Most accounts per platform (C#: `PlatformAccountCollection.LimitReached`).
pub const MAX_ACCOUNTS_PER_PLATFORM: usize = 5;

/// Whether `platform`/`account` may be added (C#: `AccountCollection.CanAddAccountID`): it is not
/// on the list yet and the platform has room.
///
/// # Arguments
/// * `s` - current settings.
/// * `platform` - account platform.
/// * `account` - account id.
///
/// # Returns
/// `true` if the account can be added.
pub fn can_add_account(s: &StreamerSettings, platform: Platform, account: &str) -> bool {
    let names = s.accounts.get(platform.member_name()).map(Vec::as_slice).unwrap_or_default();
    !names.iter().any(|n| n.eq_ignore_ascii_case(account)) && names.len() < MAX_ACCOUNTS_PER_PLATFORM
}

/// Name of a platform in the prompt (C#: `Platform <= OculusQuest ? "Meta" : display name`).
///
/// # Arguments
/// * `platform` - account platform.
///
/// # Returns
/// The name.
pub fn platform_label(platform: Platform) -> &'static str {
    match platform {
        Platform::Oculus | Platform::OculusQuest => "Meta",
        Platform::Vive => "Viveport",
        Platform::Pico => "Pico",
        Platform::Google => "Google",
        Platform::PlayForDream => "Play for Dream",
        Platform::Apple => "Apple",
        Platform::Steam => "Steam",
    }
}

/// Queues `request` for a prompt if it is new and asking is enabled.
///
/// # Arguments
/// * `app` - application context.
/// * `seen` - headset keys already handled (the original prompts once per key per run).
/// * `request` - received request.
fn handle(app: &App, seen: &mut HashSet<String>, request: DiscoveryRequest) {
    {
        let s = app.settings.lock().expect("poisoned");
        if !can_add_account(&s, request.platform, &request.account_id) || seen.contains(&request.headset_key) {
            return;
        }
        seen.insert(request.headset_key.clone());
        if !s.show_pairing_requests {
            return;
        }
    }
    info!(platform = platform_label(request.platform), account = %request.account_id, signed = request.signed, "a headset with an unknown account is searching for computers");
    app.pairing_requests.lock().expect("poisoned").push_back(request);
    app.pairing_notify.notify_one();
}

/// Set once the "port in use" warning has been logged.
static BIND_WARNED: AtomicBool = AtomicBool::new(false);

/// Receives discovery requests and queues the prompts.
///
/// # Arguments
/// * `app` - application context.
async fn run(app: Arc<App>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let listener = tokio::spawn(vd_net::pairing::listen(tx));
    while let Some(request) = rx.recv().await {
        let mut seen = app.paired_headsets.lock().expect("poisoned");
        handle(&app, &mut seen, request);
    }
    match listener.await {
        Ok(Err(e @ (NetError::AddrInUse(_) | NetError::AccessDenied(_)))) => {
            if !BIND_WARNED.swap(true, Ordering::Relaxed) {
                warn!(port = vd_net::pairing::DISCOVERY_PORT, error = %e, "cannot listen for headsets searching the LAN; pairing prompts are unavailable");
            }
        }
        Ok(Err(e)) => debug!(error = %e, "LAN discovery stopped"),
        _ => {}
    }
}

/// Listens for discovery requests while it is alive (C#: started with the registration and
/// stopped once a headset connects).
pub struct Discovery(JoinHandle<()>);

impl Discovery {
    /// Starts listening.
    ///
    /// # Arguments
    /// * `app` - application context.
    ///
    /// # Returns
    /// The guard; dropping it stops the listener.
    pub fn start(app: &Arc<App>) -> Self {
        Self(tokio::spawn(run(Arc::clone(app))))
    }
}

impl Drop for Discovery {
    /// Stops the listener.
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_can_be_added_until_the_limit() {
        let mut s = StreamerSettings::default();
        assert!(can_add_account(&s, Platform::OculusQuest, "a"));
        s.add_account(Platform::OculusQuest, "A");
        assert!(!can_add_account(&s, Platform::OculusQuest, "a"), "already listed (case-insensitive)");
        for n in ["b", "c", "d", "e"] {
            s.add_account(Platform::OculusQuest, n);
        }
        assert!(!can_add_account(&s, Platform::OculusQuest, "f"), "limit of five");
        assert!(can_add_account(&s, Platform::Pico, "f"));
    }

    #[test]
    fn prompt_names_meta() {
        assert_eq!(platform_label(Platform::Oculus), "Meta");
        assert_eq!(platform_label(Platform::OculusQuest), "Meta");
        assert_eq!(platform_label(Platform::Vive), "Viveport");
    }
}
