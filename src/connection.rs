//! Session establishment.  C#: `VirtualDesktop.Streamer.ConnectionManager`.
//!
//! Original flow (one iteration of [`run`]):
//!
//! 1. enumerate private adapters and register with `IComputerRegistry.RegisterComputer5`
//!    (accounts, computer id/name, OS, version, adapters, remote/encryption flags) to obtain a
//!    `RegistrationToken` (connection id + AES key/IV);
//! 2. increment `ServerRotation`, connect the messaging channel to the regional relay on
//!    `38811 + rotation` and send the connection id;
//! 3. when the relay forwards a headset's `PeerInfo`, accept the headset — hole punching through
//!    the relay's local port when remote connections are allowed, otherwise listening on 38810;
//! 4. establish the video (38830/38831+r), audio (38840/38841+r) and data (38820/38821+r)
//!    channels, send the bandwidth probe, mark the session connected and send
//!    `StreamerSettingsReload`;
//! 5. when any channel drops, reset all of them and start over.
//!
//! `--direct` mode skips steps 1–3 and listens on the LAN ports directly (no encryption); it is a
//! development aid for headset emulators — the official headset app always rendezvouses through
//! the relay.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{error, info, warn};
use vd_net::NetError;
use vd_net::adapters::{connection_type, private_adapters};
use vd_net::peer::{ChannelMode, PeerState};
use vd_net::registry::{Registration, RegistryClient};
use vd_net::relay;
use vd_net::stream::SessionKey;
use vd_protocol::channels::{BANDWIDTH_PROBE_BYTES, Channel};
use vd_protocol::messages::PeerInfo;
use vd_protocol::settings::reload_message;

use crate::app::App;
use crate::state::ConnectionStatus;

/// Delay before retrying after a failure.
const RETRY: Duration = Duration::from_secs(5);

fn lan_bind(ch: Channel) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::UNSPECIFIED, ch.lan_port()))
}

/// Local UTC offset in hours (Linux: `TZ`/`/etc/localtime` via `date +%z`; Windows: `tzutil`-free
/// fallback via the `TZ` variable).  Used only to choose the relay region.
///
/// # Returns
/// The offset, or 0 if unknown.
fn utc_offset_hours() -> f64 {
    let out = if cfg!(windows) {
        crate::command("powershell").args(["-NoProfile", "-Command", "[TimeZoneInfo]::Local.BaseUtcOffset.TotalHours"]).output()
    } else {
        std::process::Command::new("date").arg("+%z").output()
    };
    let Ok(out) = out else { return 0.0 };
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if let Ok(h) = s.parse::<f64>() {
        return h;
    }
    // "+0100" / "-0500"
    if s.len() == 5 {
        let sign = if s.starts_with('-') { -1.0 } else { 1.0 };
        let h: f64 = s[1..3].parse().unwrap_or(0.0);
        let m: f64 = s[3..5].parse().unwrap_or(0.0);
        return sign * (h + m / 60.0);
    }
    0.0
}

/// Session parameters agreed with the rendezvous service.
struct SessionParams {
    region: vd_protocol::types::ServerRegion,
    rotation: u16,
    connection_id: Vec<u8>,
    key: Option<SessionKey>,
}

/// Registers the computer.  C#: the `RegisterComputer5` lambda in `ConnectionManager`.
///
/// # Arguments
/// * `app` - application context.
///
/// # Returns
/// Session parameters.
///
/// # Errors
/// Registry errors, or [`NetError::Soap`] when the token is invalid (streamer too old) or no
/// account is configured.
async fn register(app: &App) -> Result<SessionParams, NetError> {
    let (registration, region) = {
        let mut s = app.settings.lock().expect("poisoned");
        let accounts = s.access_tokens();
        if accounts.is_empty() {
            return Err(NetError::Soap("no account configured — run `desktop_streamer account add <platform> <user>`".into()));
        }
        if s.computer_id.is_none() {
            s.computer_id = Some(vd_platform::process::machine_id().unwrap_or_else(|| format!("{:032x}", rand_u128())));
        }
        let adapters = private_adapters(s.allow_remote_connections);
        app.state.dynamic.send_modify(|d| d.connection_type = connection_type(&adapters));
        let region = relay::region_for_utc_offset(utc_offset_hours());
        (
            Registration {
                accounts,
                id: s.computer_id.clone().unwrap_or_default(),
                name: vd_platform::process::computer_name(),
                description: String::new(),
                os: s.port.reported_os,
                version: s.reported_version(),
                adapters,
                allow_remote_connections: s.allow_remote_connections,
                encrypt_local_traffic: s.encrypt_local_traffic,
                encrypt_remote_traffic: true,
            },
            region,
        )
    };
    let client = RegistryClient::new(relay::registry_url(region))?;
    let token = client.register_computer(&registration).await?.ok_or_else(|| NetError::Soap("registry returned no token".into()))?;
    if !token.is_valid {
        return Err(NetError::Soap("Streamer needs to be updated (registry rejected the reported version)".into()));
    }
    let rotation = {
        let mut s = app.settings.lock().expect("poisoned");
        s.server_rotation = relay::next_rotation(s.server_rotation);
        s.server_rotation
    };
    app.save_settings();
    Ok(SessionParams { region, rotation, connection_id: token.connection_id, key: Some(token.key) })
}

fn rand_u128() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    t ^ (u128::from(std::process::id()) << 64)
}

/// Waits until any of the four channels leaves the *Connected* state.
///
/// # Arguments
/// * `app` - application context.
async fn wait_for_disconnect(app: &App) {
    let c = &app.channels;
    let mut rx = [c.messaging.subscribe(), c.data.subscribe(), c.video.subscribe(), c.audio.subscribe()];
    let mut shutdown = app.shutdown.subscribe();
    loop {
        if rx.iter().any(|r| *r.borrow() != PeerState::Connected) || *shutdown.borrow() {
            return;
        }
        let [a, b, cc, d] = &mut rx;
        tokio::select! {
            _ = a.changed() => {}
            _ = b.changed() => {}
            _ = cc.changed() => {}
            _ = d.changed() => {}
            _ = shutdown.changed() => {}
        }
    }
}

/// Resets every channel and the headset-owned state.
///
/// # Arguments
/// * `app` - application context.
async fn teardown(app: &App) {
    let c = &app.channels;
    c.messaging.reset().await;
    c.data.reset().await;
    c.video.reset().await;
    c.audio.reset().await;
    app.state.reset_headset_state();
    app.state.dynamic.send_modify(|d| d.status = ConnectionStatus::Disconnected);
    info!("session ended");
}

/// Establishes the three media channels concurrently.
///
/// # Arguments
/// * `app` - application context.
/// * `modes` - `(video, audio, data)` modes.
/// * `key` - session key.
/// * `encrypt` - encrypt the peer connections.
///
/// # Errors
/// The first channel error.
async fn establish_media(app: &App, modes: [ChannelMode; 3], key: Option<&SessionKey>, encrypt: bool) -> Result<(), NetError> {
    let c = &app.channels;
    let [v, a, d] = &modes;
    let (rv, ra, rd) = tokio::join!(c.video.establish(v, key, encrypt), c.audio.establish(a, key, encrypt), c.data.establish(d, key, encrypt));
    rv?;
    ra?;
    rd?;
    Ok(())
}

/// Finishes a session once all channels are up: bandwidth probe, `IsConnected`, settings reload.
///
/// # Arguments
/// * `app` - application context.
/// * `encrypted` - whether the session is encrypted.
async fn on_connected(app: &App, encrypted: bool) {
    let probe = if app.settings.lock().expect("poisoned").auto_adjust_bitrate { BANDWIDTH_PROBE_BYTES } else { 0 };
    if let Err(e) = app.channels.data.send_bandwidth_probe(probe).await {
        warn!(error = %e, "bandwidth probe failed");
    }
    app.state.dynamic.send_modify(|d| {
        d.is_connected = true;
        d.encrypted = encrypted;
        d.status = ConnectionStatus::Connected;
    });
    let snapshot = app.state.streamer.borrow().clone();
    crate::dispatch::mark_streamer_settings_sent(snapshot.clone());
    let reload = reload_message(&snapshot);
    match reload {
        Ok(msg) => app.send(&msg).await,
        Err(e) => warn!(error = %e, "cannot encode settings reload"),
    }
    info!(remote = ?app.channels.messaging.remote(), encrypted, "headset connected");
    app.request_keyframe_all();
}

impl App {
    /// Requests key frames (new session or `ForceKeyFrame`).
    pub fn request_keyframe_all(&self) {
        self.state.request_keyframe();
    }
}

/// One rendezvous session.
///
/// # Arguments
/// * `app` - application context.
/// * `peers` - `PeerInfo` messages forwarded by the dispatcher.
///
/// # Errors
/// Any registration or connection error (the caller retries).
async fn rendezvous_session(app: &Arc<App>, peers: &mut mpsc::UnboundedReceiver<PeerInfo>) -> Result<(), NetError> {
    // A PeerInfo left over from a previous relay leg refers to a port we no longer use.
    while peers.try_recv().is_ok() {}
    app.state.dynamic.send_modify(|d| d.status = ConnectionStatus::Connecting);
    // C#: the LAN discovery service runs from registration until a headset connects.
    let discovery = crate::pairing::Discovery::start(app);
    let params = register(app).await?;
    let relay_ep = relay::relay_endpoint(params.region, Channel::Messaging, params.rotation);
    info!(%relay_ep, region = ?params.region, "connecting to relay");
    // The relay leg is bound to 0.0.0.0:38810 so the headset can reach the PC on that port later.
    match app.channels.messaging.connect_relay(relay_ep, Some(lan_bind(Channel::Messaging)), &params.connection_id, params.key.as_ref()).await {
        Ok(()) => {}
        Err(NetError::AddrInUse(e)) => {
            tokio::time::sleep(vd_net::peer::ADDR_IN_USE_RETRY).await;
            return Err(NetError::AddrInUse(e));
        }
        Err(e) => return Err(e),
    }
    app.state.dynamic.send_modify(|d| d.status = ConnectionStatus::Waiting);
    let mut state = app.channels.messaging.subscribe();
    let peer = loop {
        tokio::select! {
            p = peers.recv() => match p {
                Some(p) => break p,
                None => return Err(NetError::Closed),
            },
            _ = state.changed() => {
                if *state.borrow() == PeerState::Disconnected {
                    // The relay sends PeerInfo and closes the leg immediately; the dispatcher may
                    // forward the PeerInfo just after we observe the close.
                    match tokio::time::timeout(Duration::from_secs(2), peers.recv()).await {
                        Ok(Some(p)) => break p,
                        _ => return Err(NetError::Closed),
                    }
                }
            }
        }
    };
    drop(discovery);
    let allow_remote = app.settings.lock().expect("poisoned").allow_remote_connections;
    let encrypt = peer.encrypt();
    let punch = allow_remote.then_some(peer.endpoint);
    info!(peer = %peer.endpoint, encrypt, remote = punch.is_some(), "headset requested a session");
    // C#: `_0096_0002._0002._0001(IPAddress.Any, endpoint, aes, true)` — the messaging channel is
    // always encrypted; PeerInfo's flag only governs the media channels.
    app.channels.messaging.accept_peer(lan_bind(Channel::Messaging), punch, params.key.as_ref(), true).await?;
    // C#: the PeerInfo handler always passes the connection id, so every channel rendezvouses
    // through its relay leg; AllowRemoteConnections only enables dialling out to the headset.
    let modes = [Channel::Video, Channel::Audio, Channel::Data].map(|ch| ChannelMode::Remote {
        relay: relay::relay_endpoint(params.region, ch, params.rotation),
        local: lan_bind(ch),
        connection_id: params.connection_id.clone(),
        punch: allow_remote,
    });
    establish_media(app, modes, params.key.as_ref(), encrypt).await?;
    on_connected(app, encrypt).await;
    wait_for_disconnect(app).await;
    Ok(())
}

/// One direct (relay-less) LAN session.
///
/// # Arguments
/// * `app` - application context.
///
/// # Errors
/// Connection errors.
async fn direct_session(app: &Arc<App>) -> Result<(), NetError> {
    info!("direct mode: waiting for a headset on TCP 38810–38840");
    app.state.dynamic.send_modify(|d| d.status = ConnectionStatus::Waiting);
    app.channels.messaging.listen_direct(lan_bind(Channel::Messaging), Duration::from_secs(3600)).await?;
    let modes = [Channel::Video, Channel::Audio, Channel::Data].map(|ch| ChannelMode::Lan { bind: lan_bind(ch) });
    establish_media(app, modes, None, false).await?;
    on_connected(app, false).await;
    wait_for_disconnect(app).await;
    Ok(())
}

/// Connection manager main loop.  C#: `ConnectionManager._0001()` / `_0002()` / `_0006`.
///
/// # Arguments
/// * `app` - application context.
/// * `peers` - `PeerInfo` receiver.
pub async fn run(app: Arc<App>, mut peers: mpsc::UnboundedReceiver<PeerInfo>) {
    if !app.settings.lock().expect("poisoned").allow_remote_connections {
        info!("remote connections disabled; UPnP mappings removed");
        let _ = vd_net::upnp::unmap_ports().await;
    }
    let status = vd_net::routing::detect_routing_status().await;
    app.state.dynamic.send_modify(|d| d.routing_status = status);
    info!(?status, "routing status");
    let mut shutdown = app.shutdown.subscribe();
    while !app.is_shutting_down() {
        if !app.direct_mode && app.settings.lock().expect("poisoned").access_tokens().is_empty() {
            // C#: IsEditingAccounts — nothing to register until an account is added.
            info!("no account configured; add one in the settings window");
            tokio::select! {
                _ = app.reconnect.notified() => {}
                _ = shutdown.changed() => {}
            }
            continue;
        }
        let session = async {
            if app.direct_mode { direct_session(&app).await } else { rendezvous_session(&app, &mut peers).await }
        };
        // The settings window asks for a fresh registration when accounts or options change.
        let res = tokio::select! {
            r = session => Some(r),
            _ = app.reconnect.notified() => None,
        };
        teardown(&app).await;
        if app.is_shutting_down() {
            break;
        }
        match res {
            None => {
                info!("connection settings changed; registering again");
                continue;
            }
            Some(Ok(())) => {}
            Some(Err(NetError::AccessDenied(e))) => error!(error = %e, "Anti-virus or VPN software is preventing connections to your PC"),
            Some(Err(NetError::Timeout(what))) => warn!(what, "timed out"),
            Some(Err(e)) => warn!(error = %e, "session failed"),
        }
        tokio::select! {
            _ = tokio::time::sleep(RETRY) => {}
            _ = app.reconnect.notified() => {}
        }
    }
}
