//! The control-message channel.  C#: `VirtualDesktop.Net.NetMessagingClient`.
//!
//! Lifecycle driven by the connection manager:
//!
//! 1. [`MessagingClient::connect_relay`] — connect to the relay, send the connection id, enter
//!    *ConnectedToServer* and start receiving (the relay forwards `PeerInfo`, update requests…).
//! 2. On `PeerInfo`, [`MessagingClient::accept_peer`] — drop the relay socket, listen on 38810
//!    (LAN) or hole-punch from the relay's local port (remote), enter *Connected*.
//! 3. Every received frame is delivered on the `mpsc` receiver returned by
//!    [`MessagingClient::new`]; a read error resets the channel to *Disconnected*.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use vd_protocol::NetMessage;

use crate::error::Result;
use crate::link::LinkCore;
use crate::peer::{self, ACCEPT_TIMEOUT, PeerState};
use crate::stream::{SecureReader, SessionKey};

/// Control-message channel.
pub struct MessagingClient {
    core: Arc<LinkCore>,
    inbound: mpsc::UnboundedSender<NetMessage>,
    relay_local: std::sync::Mutex<Option<SocketAddr>>,
}

impl MessagingClient {
    /// Creates a disconnected client.
    ///
    /// # Returns
    /// The client and the receiver of every inbound message.
    pub fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<NetMessage>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Arc::new(Self { core: LinkCore::new("messaging"), inbound: tx, relay_local: std::sync::Mutex::new(None) }), rx)
    }

    /// Returns the channel state.
    ///
    /// # Returns
    /// The state.
    pub fn state(&self) -> PeerState {
        self.core.state()
    }

    /// Subscribes to state changes.
    ///
    /// # Returns
    /// A watch receiver.
    pub fn subscribe(&self) -> watch::Receiver<PeerState> {
        self.core.subscribe()
    }

    /// Remote endpoint of the current connection (the headset once connected).
    ///
    /// # Returns
    /// The endpoint, if any.
    pub fn remote(&self) -> Option<SocketAddr> {
        self.core.remote()
    }

    fn reader_loop(tx: mpsc::UnboundedSender<NetMessage>) -> impl FnOnce(SecureReader) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
        move |mut reader| {
            Box::pin(async move {
                loop {
                    let msg = peer::read_message(&mut reader).await?;
                    if tx.send(msg).is_err() {
                        return Ok(());
                    }
                }
            })
        }
    }

    /// Connects to the relay server.  C#: `_0096_0002._0002._0001(ServerRegion, byte[], Aes, int)`.
    ///
    /// The original binds the relay leg to the channel's own endpoint (`0.0.0.0:38810`), so the
    /// headset can later reach the PC on that well-known port.
    ///
    /// # Arguments
    /// * `relay` - relay endpoint.
    /// * `local` - local endpoint to bind (normally `0.0.0.0:38810`).
    /// * `connection_id` - `RegistrationToken.ConnectionID`.
    /// * `key` - session key.
    ///
    /// # Errors
    /// Connect errors (the caller retries; `AddrInUse` after [`peer::ADDR_IN_USE_RETRY`]).
    pub async fn connect_relay(&self, relay: SocketAddr, local: Option<SocketAddr>, connection_id: &[u8], key: Option<&SessionKey>) -> Result<()> {
        self.core.set_state(PeerState::Connecting);
        match peer::connect_relay(relay, local, connection_id, key).await {
            Ok(conn) => {
                *self.relay_local.lock().expect("poisoned") = Some(conn.local);
                self.core.attach(conn, PeerState::ConnectedToServer, Self::reader_loop(self.inbound.clone())).await;
                Ok(())
            }
            Err(e) => {
                self.core.set_state(PeerState::Disconnected);
                Err(e)
            }
        }
    }

    /// Accepts the headset after a `PeerInfo`.  C#: `_0096_0002._0002._0007` (LAN, listen on
    /// 38810) and `._0008` (remote, hole punch from the relay's local endpoint).
    ///
    /// # Arguments
    /// * `bind` - LAN listening endpoint; ignored when `punch_target` is set.
    /// * `punch_target` - headset public endpoint for remote sessions.
    /// * `key` - session key, applied when `encrypt` is set.
    /// * `encrypt` - wrap the peer connection in the record layer.
    ///
    /// # Errors
    /// [`crate::NetError::Timeout`] if the headset does not connect within 20 s.
    pub async fn accept_peer(&self, bind: SocketAddr, punch_target: Option<SocketAddr>, key: Option<&SessionKey>, encrypt: bool) -> Result<()> {
        // Listen on every interface, on the port the relay leg used (C#: `new IPEndPoint(Any, port)`).
        let local = match self.relay_local.lock().expect("poisoned").take() {
            Some(relay_local) => SocketAddr::new(peer::unspecified_like(relay_local.ip()), relay_local.port()),
            None => bind,
        };
        self.core.detach().await;
        self.core.set_state(PeerState::Listening);
        let targets: Vec<SocketAddr> = punch_target.into_iter().collect();
        match peer::rendezvous(local, &targets, ACCEPT_TIMEOUT).await {
            Ok(stream) => {
                let conn = peer::Connection::new(stream, if encrypt { key } else { None })?;
                self.core.attach(conn, PeerState::Connected, Self::reader_loop(self.inbound.clone())).await;
                Ok(())
            }
            Err(e) => {
                self.core.set_state(PeerState::Disconnected);
                Err(e)
            }
        }
    }

    /// Listens directly on `bind` without any relay (development / offline LAN mode; not part
    /// of the original flow, which always waits for the relay's `PeerInfo`).
    ///
    /// # Arguments
    /// * `bind` - listening endpoint.
    /// * `timeout` - accept deadline.
    ///
    /// # Errors
    /// Bind or timeout errors.
    pub async fn listen_direct(&self, bind: SocketAddr, timeout: std::time::Duration) -> Result<()> {
        self.core.detach().await;
        self.core.set_state(PeerState::Listening);
        match peer::rendezvous(bind, &[], timeout).await {
            Ok(stream) => {
                let conn = peer::Connection::new(stream, None)?;
                self.core.attach(conn, PeerState::Connected, Self::reader_loop(self.inbound.clone())).await;
                Ok(())
            }
            Err(e) => {
                self.core.set_state(PeerState::Disconnected);
                Err(e)
            }
        }
    }

    /// Sends a control message.  C#: `NetMessage._0001(_0096_0002._0002)` (send + return to pool).
    ///
    /// Sending while disconnected is a silent no-op in the original; here it returns
    /// [`crate::NetError::NotConnected`] so callers can decide.
    ///
    /// # Arguments
    /// * `msg` - message to send.
    ///
    /// # Errors
    /// Not connected, encoding or socket errors.
    pub async fn send(&self, msg: &NetMessage) -> Result<()> {
        self.core.send(&msg.encode()?).await
    }

    /// Disconnects.  C#: `Reset()`.
    pub async fn reset(&self) {
        self.core.reset().await;
    }
}
