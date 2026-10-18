//! Connection establishment shared by all four channels.
//!
//! C#: `_0096_0002._0002` (base class of every channel).  A channel reaches the *Connected*
//! state in one of three ways:
//!
//! 1. **LAN** — after the rendezvous server forwards a `PeerInfo` for a local headset, the
//!    streamer listens on the channel's LAN port (38810/20/30/40) and accepts the headset's
//!    connection (20 s timeout).
//! 2. **Relay** — the messaging channel first connects *out* to the regional relay
//!    (`america|europe.vrdesktop.net`), sends the raw `ConnectionID` bytes from its
//!    `RegistrationToken`, switches to the AES record layer and then exchanges control messages
//!    through the relay.
//! 3. **Remote / hole punching** — for internet sessions each channel connects to the relay,
//!    receives a `PeerInfo` with the headset's public endpoint, closes the relay socket and then
//!    *simultaneously* listens on and connects from the same local port (TCP simultaneous open),
//!    keeping whichever connection is established first.
//!
//! Socket options reproduced from the original: `TCP_NODELAY`, `SO_REUSEADDR` (and
//! `SO_REUSEPORT` on Unix, needed for hole punching), dual-stack listeners, and retrying after
//! 10 s when the port was already in use.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::task::JoinSet;
use tracing::{debug, warn};
use vd_protocol::message::{HEADER_LEN, NetMessage};
use vd_protocol::messages::PeerInfo;
use vd_protocol::MessageType;

use crate::error::{NetError, Result};
use crate::stream::{SecureReader, SecureWriter, SessionKey, split};

/// Time to wait for the headset to connect.  C#: `Task._0001(20000)` in `_0096_0002._0002._000F`.
pub const ACCEPT_TIMEOUT: Duration = Duration::from_secs(20);
/// Delay before re-binding after "address already in use".  C#: `Task.Delay(10000)` in `_0010`.
pub const ADDR_IN_USE_RETRY: Duration = Duration::from_secs(10);
/// Delay before starting outbound hole-punch attempts (IPv4).
pub const PUNCH_DELAY_V4: Duration = Duration::from_millis(50);
/// Delay before starting outbound hole-punch attempts (IPv6).
pub const PUNCH_DELAY_V6: Duration = Duration::from_millis(400);

/// Connection state of a channel.  C#: `_001C_0002._0003` (members obfuscated; meaning
/// inferred from the transitions and from `VirtualDesktop.Interfaces.Icon`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PeerState {
    /// No connection.
    #[default]
    Disconnected,
    /// Connecting to the relay server.
    Connecting,
    /// Connected to the relay server, waiting for a peer.
    ConnectedToServer,
    /// Listening for the headset.
    Listening,
    /// Connected to the headset.
    Connected,
}

/// An established, possibly encrypted connection.
pub struct Connection {
    /// Reading half.
    pub reader: SecureReader,
    /// Writing half.
    pub writer: SecureWriter,
    /// Remote endpoint.
    pub remote: SocketAddr,
    /// Local endpoint.
    pub local: SocketAddr,
}

impl Connection {
    /// Wraps a connected socket.
    ///
    /// # Arguments
    /// * `stream` - connected stream.
    /// * `key` - AES key when the channel must be encrypted.
    ///
    /// # Returns
    /// The connection.
    ///
    /// # Errors
    /// [`NetError::Io`] if the socket addresses cannot be read; [`NetError::Protocol`] for bad keys.
    pub fn new(stream: TcpStream, key: Option<&SessionKey>) -> Result<Self> {
        stream.set_nodelay(true)?;
        let remote = stream.peer_addr()?;
        let local = stream.local_addr()?;
        let (reader, writer) = split(stream, key)?;
        Ok(Self { reader, writer, remote, local })
    }
}

/// Unspecified address of the same family as `ip`.
///
/// # Arguments
/// * `ip` - reference address.
///
/// # Returns
/// `0.0.0.0` or `::`.
pub fn unspecified_like(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    }
}

/// Creates a TCP socket for `local`'s address family with the original's options.
///
/// # Arguments
/// * `local` - address to bind.
/// * `reuse` - enable `SO_REUSEADDR` (and `SO_REUSEPORT` on Unix) so that a listener and an
///   outbound connection can share the port during hole punching.
///
/// # Returns
/// The bound socket.
///
/// # Errors
/// [`NetError::AddrInUse`], [`NetError::AccessDenied`] or [`NetError::Io`] if binding fails.
pub fn bound_socket(local: SocketAddr, reuse: bool) -> Result<TcpSocket> {
    let socket = if local.is_ipv4() { TcpSocket::new_v4()? } else { TcpSocket::new_v6()? };
    if reuse {
        socket.set_reuseaddr(true)?;
        #[cfg(unix)]
        socket.set_reuseport(true)?;
    }
    // The original also sets `LingerOption(true, 0)` (abortive close).  tokio deprecates
    // SO_LINGER because a non-zero linger blocks on drop; the default graceful close is used.
    socket.bind(local).map_err(NetError::from_bind)?;
    Ok(socket)
}

/// Connects to `remote`, optionally from a fixed local endpoint.
///
/// # Arguments
/// * `local` - local endpoint to bind (with port reuse), or `None` for an ephemeral port.
/// * `remote` - destination.
///
/// # Returns
/// The connected stream.
///
/// # Errors
/// Bind or connect errors.
pub async fn connect_from(local: Option<SocketAddr>, remote: SocketAddr) -> Result<TcpStream> {
    let local = local.unwrap_or_else(|| SocketAddr::new(unspecified_like(remote.ip()), 0));
    let socket = bound_socket(local, true)?;
    let stream = socket.connect(remote).await?;
    stream.set_nodelay(true)?;
    Ok(stream)
}

/// Listens on `bind` and, concurrently, connects out to every `punch_targets` address from the
/// same local endpoint; returns the first established connection.
///
/// C#: `_0096_0002._0002._000F` (listener + `ConcurrentBag<TcpClient>` of outbound attempts +
/// `TaskCompletionSource<TcpClient>` resolved by whichever completes first).
///
/// # Arguments
/// * `bind` - local endpoint (port 0 is not allowed when `punch_targets` is non-empty).
/// * `punch_targets` - remote endpoints to dial for NAT traversal (empty on the LAN).
/// * `timeout` - overall deadline (normally [`ACCEPT_TIMEOUT`]).
///
/// # Returns
/// The connected stream.
///
/// # Errors
/// [`NetError::Timeout`] when nothing connects in time, or bind errors.
pub async fn rendezvous(bind: SocketAddr, punch_targets: &[SocketAddr], timeout: Duration) -> Result<TcpStream> {
    // The original always sets ReuseAddress on listeners: the port was just used by the relay leg.
    let listener: TcpListener = {
        let socket = bound_socket(bind, true)?;
        socket.listen(16)?
    };
    let local = listener.local_addr()?;
    let mut set: JoinSet<Result<TcpStream>> = JoinSet::new();
    set.spawn(async move {
        let (stream, from) = listener.accept().await?;
        debug!(%from, "accepted channel connection");
        Ok(stream)
    });
    for &target in punch_targets {
        if target.ip().is_unspecified() {
            continue;
        }
        let delay = if target.is_ipv6() { PUNCH_DELAY_V6 } else { PUNCH_DELAY_V4 };
        set.spawn(async move {
            tokio::time::sleep(delay).await;
            loop {
                match connect_from(Some(local), target).await {
                    Ok(s) => return Ok(s),
                    Err(e) => {
                        debug!(%target, error = %e, "hole punch attempt failed, retrying");
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                }
            }
        });
    }
    let result = tokio::time::timeout(timeout, async {
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(Ok(stream)) => return Ok(stream),
                Ok(Err(e)) => warn!(error = %e, "connection attempt failed"),
                Err(e) => warn!(error = %e, "connection task panicked"),
            }
        }
        Err(NetError::Closed)
    })
    .await;
    set.abort_all();
    match result {
        Ok(r) => r,
        Err(_) => Err(NetError::Timeout("waiting for the headset to connect")),
    }
}

/// Reads one control frame.
///
/// # Arguments
/// * `reader` - channel reader.
///
/// # Returns
/// The message.
///
/// # Errors
/// [`NetError::Closed`] on EOF (the original throws `TaskCanceledException` when the first read
/// returns 0 bytes), other I/O / protocol errors.
pub async fn read_message(reader: &mut SecureReader) -> Result<NetMessage> {
    let header: [u8; HEADER_LEN] = reader.read_array().await?;
    let (raw_type, len) = NetMessage::parse_header(&header)?;
    let payload = reader.read_vec(len).await?;
    Ok(NetMessage { raw_type, payload })
}

/// Writes one control frame.
///
/// # Arguments
/// * `writer` - channel writer.
/// * `msg` - message.
///
/// # Errors
/// I/O or encoding errors.
pub async fn write_message(writer: &mut SecureWriter, msg: &NetMessage) -> Result<()> {
    writer.write_packet(&msg.encode()?).await
}

/// Connects to the relay server and announces the session.  C#: `_0096_0002._0002._0010`.
///
/// # Arguments
/// * `relay` - relay endpoint.
/// * `local` - fixed local endpoint (used again later for hole punching), or `None`.
/// * `connection_id` - raw `RegistrationToken.ConnectionID`.
/// * `key` - AES key; the stream switches to the record layer right after the id is sent.
///
/// # Returns
/// The relay connection.
///
/// # Errors
/// Connect / write errors.  `AddrInUse` should be retried after [`ADDR_IN_USE_RETRY`].
pub async fn connect_relay(relay: SocketAddr, local: Option<SocketAddr>, connection_id: &[u8], key: Option<&SessionKey>) -> Result<Connection> {
    use tokio::io::AsyncWriteExt;
    let mut stream = connect_from(local, relay).await?;
    stream.write_all(connection_id).await?;
    Connection::new(stream, key)
}

/// Waits for the `PeerInfo` the relay sends when a headset joins.  C#: `_0096_0002._0002._0011`.
///
/// # Arguments
/// * `conn` - relay connection.
///
/// # Returns
/// The peer information.
///
/// # Errors
/// [`NetError::UnexpectedMessage`] if another message arrives first ("Expected PeerInfo message").
pub async fn read_peer_info(conn: &mut Connection) -> Result<PeerInfo> {
    let msg = read_message(&mut conn.reader).await?;
    if msg.message_type() != Some(MessageType::PeerInfo) {
        return Err(NetError::UnexpectedMessage(msg.to_string()));
    }
    Ok(PeerInfo::decode(&msg)?)
}

/// How a (non-messaging) channel should be established once the session parameters are known.
#[derive(Debug, Clone)]
pub enum ChannelMode {
    /// Listen on `bind` for the headset (LAN session).
    Lan {
        /// Listening endpoint (normally `[::]:<lan port>` or `0.0.0.0:<lan port>`).
        bind: SocketAddr,
    },
    /// Rendezvous through the relay, then accept the headset on the same local port.
    ///
    /// This is the path the official flow always takes (C#: `_0096_0002._0002._000E`): the relay
    /// leg is bound to the channel's well-known local port (`0.0.0.0:38820` …), the relay answers
    /// with a `PeerInfo` and closes, and the PC listens on that same port — so a LAN headset
    /// reaches it at `<pc private ip>:<lan port>` while a remote one reaches the NAT mapping
    /// created by the relay leg.
    Remote {
        /// Relay endpoint for this channel (`relay ip : lan port + 1 + rotation`).
        relay: SocketAddr,
        /// Local endpoint of the relay leg and of the subsequent listener (`0.0.0.0:<lan port>`).
        local: SocketAddr,
        /// `RegistrationToken.ConnectionID`.
        connection_id: Vec<u8>,
        /// Dial the headset's public endpoint (`AllowRemoteConnections`).
        punch: bool,
    },
}

/// Establishes a media channel.  C#: `_0096_0002._0002._0007` (LAN) and `._000E` (remote).
///
/// # Arguments
/// * `mode` - LAN or remote parameters.
/// * `key` - session key (always used for the relay leg).
/// * `encrypt` - whether the final peer connection uses the record layer.
///
/// # Returns
/// The connected channel.
///
/// # Errors
/// Timeouts, bind errors or relay protocol errors.
pub async fn establish_channel(mode: &ChannelMode, key: Option<&SessionKey>, encrypt: bool) -> Result<Connection> {
    let final_key = if encrypt { key } else { None };
    match mode {
        ChannelMode::Lan { bind } => {
            let stream = rendezvous(*bind, &[], ACCEPT_TIMEOUT).await?;
            Connection::new(stream, final_key)
        }
        ChannelMode::Remote { relay, local, connection_id, punch } => {
            let mut relay_conn = connect_relay(*relay, Some(*local), connection_id, key).await?;
            let peer = read_peer_info(&mut relay_conn).await?;
            drop(relay_conn);
            let targets: Vec<SocketAddr> = if *punch { vec![peer.endpoint] } else { Vec::new() };
            let stream = rendezvous(*local, &targets, ACCEPT_TIMEOUT).await?;
            Connection::new(stream, final_key)
        }
    }
}
