//! State and I/O plumbing shared by every channel object.
//!
//! The C# channels expose a `Status` property with a `StatusChanged` event, serialise writers
//! with an `AsyncLock`, and run one receive loop per connection that calls `Reset()` (back to
//! *Disconnected*) on any read error.  [`LinkCore`] reproduces that with a tokio `watch` channel,
//! a `Mutex<Option<SecureWriter>>`, and a reader task handle.

use std::future::Future;
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use tracing::debug;

use crate::error::{NetError, Result};
use crate::peer::{Connection, PeerState};
use crate::stream::{SecureReader, SecureWriter};

/// Shared state of one channel.
pub struct LinkCore {
    name: &'static str,
    state: watch::Sender<PeerState>,
    writer: Mutex<Option<SecureWriter>>,
    reader_task: StdMutex<Option<JoinHandle<()>>>,
    remote: StdMutex<Option<std::net::SocketAddr>>,
}

impl LinkCore {
    /// Creates a disconnected link.
    ///
    /// # Arguments
    /// * `name` - channel name for logging.
    ///
    /// # Returns
    /// The link, shared behind an `Arc`.
    pub fn new(name: &'static str) -> Arc<Self> {
        let (state, _) = watch::channel(PeerState::Disconnected);
        Arc::new(Self { name, state, writer: Mutex::new(None), reader_task: StdMutex::new(None), remote: StdMutex::new(None) })
    }

    /// Returns the channel name.
    ///
    /// # Returns
    /// Static name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the current state.
    ///
    /// # Returns
    /// The state.
    pub fn state(&self) -> PeerState {
        *self.state.borrow()
    }

    /// Subscribes to state changes (`StatusChanged`).
    ///
    /// # Returns
    /// A watch receiver.
    pub fn subscribe(&self) -> watch::Receiver<PeerState> {
        self.state.subscribe()
    }

    /// Sets the state, notifying subscribers only when it changes.
    ///
    /// # Arguments
    /// * `s` - new state.
    pub fn set_state(&self, s: PeerState) {
        self.state.send_if_modified(|cur| {
            if *cur != s {
                debug!(channel = self.name, from = ?*cur, to = ?s, "channel state");
                *cur = s;
                true
            } else {
                false
            }
        });
    }

    /// Remote endpoint of the active connection.
    ///
    /// # Returns
    /// The endpoint, if connected.
    pub fn remote(&self) -> Option<std::net::SocketAddr> {
        *self.remote.lock().expect("poisoned")
    }

    /// Reports whether the link is connected to the headset.
    ///
    /// # Returns
    /// `true` in [`PeerState::Connected`].
    pub fn is_connected(&self) -> bool {
        self.state() == PeerState::Connected
    }

    /// Installs a new connection: stores the writer, spawns `reader_loop` on the reader half and
    /// switches to `state`.
    ///
    /// When the reader loop returns (EOF or error) the link resets itself to *Disconnected*,
    /// mirroring the original's `catch { Reset(); }` in every receive loop.
    ///
    /// # Arguments
    /// * `conn` - the established connection.
    /// * `state` - state to enter (`Connected` or `ConnectedToServer`).
    /// * `reader_loop` - async function consuming the reader half.
    pub async fn attach<F, Fut>(self: &Arc<Self>, conn: Connection, state: PeerState, reader_loop: F)
    where
        F: FnOnce(SecureReader) -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.detach().await;
        *self.writer.lock().await = Some(conn.writer);
        *self.remote.lock().expect("poisoned") = Some(conn.remote);
        let me = Arc::clone(self);
        let handle = tokio::spawn(async move {
            let res = reader_loop(conn.reader).await;
            match res {
                Ok(()) | Err(NetError::Closed) => debug!(channel = me.name, "connection closed"),
                Err(e) => debug!(channel = me.name, error = %e, "receive loop ended"),
            }
            *me.writer.lock().await = None;
            *me.remote.lock().expect("poisoned") = None;
            me.set_state(PeerState::Disconnected);
        });
        *self.reader_task.lock().expect("poisoned") = Some(handle);
        self.set_state(state);
    }

    /// Drops the current connection without changing the state (used before re-connecting).
    pub async fn detach(&self) {
        if let Some(h) = self.reader_task.lock().expect("poisoned").take() {
            h.abort();
        }
        if let Some(mut w) = self.writer.lock().await.take() {
            let _ = w.shutdown().await;
        }
        *self.remote.lock().expect("poisoned") = None;
    }

    /// Closes the connection and returns to *Disconnected*.  C#: `Reset()` / `_0004()`.
    pub async fn reset(&self) {
        self.detach().await;
        self.set_state(PeerState::Disconnected);
    }

    /// Sends one packet if a connection is active.
    ///
    /// Write errors reset the link (the original calls `Reset()` from the write `catch`).
    ///
    /// # Arguments
    /// * `packet` - bytes (one record).
    ///
    /// # Errors
    /// [`NetError::NotConnected`] when no writer is installed; the write error otherwise.
    pub async fn send(&self, packet: &[u8]) -> Result<()> {
        let mut guard = self.writer.lock().await;
        let Some(w) = guard.as_mut() else {
            return Err(NetError::NotConnected);
        };
        if let Err(e) = w.write_packet(packet).await {
            *guard = None;
            drop(guard);
            self.set_state(PeerState::Disconnected);
            return Err(e);
        }
        Ok(())
    }
}
