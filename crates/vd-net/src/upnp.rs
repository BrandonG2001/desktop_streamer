//! Router port forwarding.  C#: `VirtualDesktop.Net.UPnPManager` (Open.NAT).
//!
//! When `StreamerSettings.AllowRemoteConnections` is enabled the original maps TCP ports
//! 38810/38820/38830/38840 to the PC's LAN address with an infinite lease and the product name as
//! description, removing stale mappings of those ports that point at another host.  Mappings are
//! deleted again when remote connections are disabled.

use std::net::{IpAddr, SocketAddr};

use igd_next::aio::tokio::search_gateway;
use igd_next::{PortMappingProtocol, SearchOptions};
use tracing::{debug, info};
use vd_protocol::channels::Channel;

use crate::error::{NetError, Result};

/// Mapping description (`_0007._0002._0001()` = product name).
pub const DESCRIPTION: &str = "Virtual Desktop Streamer";

/// Maps every LAN channel port to `local_ip`.
///
/// # Arguments
/// * `local_ip` - the PC's LAN address on the interface facing the router.
///
/// # Returns
/// The router's external IP, if it reports one.
///
/// # Errors
/// [`NetError::Upnp`] if no gateway is found or a mapping is refused.
pub async fn map_ports(local_ip: IpAddr) -> Result<Option<IpAddr>> {
    let gateway = search_gateway(SearchOptions::default()).await.map_err(|e| NetError::Upnp(e.to_string()))?;
    for ch in Channel::ALL {
        let port = ch.lan_port();
        // Lease 0 = permanent, exactly like `new Mapping(Protocol.Tcp, ip, port, port, 0, name)`.
        gateway
            .add_port(PortMappingProtocol::TCP, port, SocketAddr::new(local_ip, port), 0, DESCRIPTION)
            .await
            .map_err(|e| NetError::Upnp(format!("port {port}: {e}")))?;
        debug!(port, %local_ip, "UPnP mapping added");
    }
    let external = gateway.get_external_ip().await.ok();
    info!(?external, "UPnP port forwarding enabled");
    Ok(external)
}

/// Removes the mappings added by [`map_ports`].
///
/// # Errors
/// [`NetError::Upnp`] if no gateway is found (individual missing mappings are ignored).
pub async fn unmap_ports() -> Result<()> {
    let gateway = search_gateway(SearchOptions::default()).await.map_err(|e| NetError::Upnp(e.to_string()))?;
    for ch in Channel::ALL {
        let _ = gateway.remove_port(PortMappingProtocol::TCP, ch.lan_port()).await;
    }
    Ok(())
}
