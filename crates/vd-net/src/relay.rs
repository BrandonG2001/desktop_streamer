//! Rendezvous / relay server selection.  C#: `_001C._0001` and `StreamerSettings.IncrementServerRotation`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use vd_protocol::channels::Channel;
use vd_protocol::types::ServerRegion;

/// Relay used for American regions (`_001C._0001.m__0001`).
pub const AMERICA_RELAY: IpAddr = IpAddr::V4(Ipv4Addr::new(20, 225, 41, 170));
/// Relay used for Europe (`_001C._0001._0002`).
pub const EUROPE_RELAY: IpAddr = IpAddr::V4(Ipv4Addr::new(40, 89, 161, 236));
/// Base relay port of the messaging channel (`ldc.i4 38811` + rotation).
pub const MESSAGING_RELAY_BASE: u16 = 38811;
/// Number of relay port rotations (`(ServerRotation + 1) % 6`).
pub const ROTATIONS: u16 = 6;

/// Picks the region from the local UTC offset, as the static constructor of `_001C._0001` does.
///
/// # Arguments
/// * `utc_offset_hours` - local offset from UTC (e.g. −5.0 for EST).
///
/// # Returns
/// `AmericaCentral` for offsets in `[-12, -2]`, `Europe` otherwise.
pub fn region_for_utc_offset(utc_offset_hours: f64) -> ServerRegion {
    if (-12.0..=-2.0).contains(&utc_offset_hours) { ServerRegion::AmericaCentral } else { ServerRegion::Europe }
}

/// Relay server address of a region.
///
/// # Arguments
/// * `region` - region.
///
/// # Returns
/// The relay IP.
pub fn relay_ip(region: ServerRegion) -> IpAddr {
    match region {
        ServerRegion::Europe => EUROPE_RELAY,
        _ => AMERICA_RELAY,
    }
}

/// `ComputerRegistry` SOAP endpoint of a region.
///
/// # Arguments
/// * `region` - region.
///
/// # Returns
/// The HTTPS URL.
pub fn registry_url(region: ServerRegion) -> &'static str {
    match region {
        ServerRegion::Europe => "https://europe.vrdesktop.net/ComputerRegistry.svc/IComputerRegistry",
        _ => "https://america.vrdesktop.net/ComputerRegistry.svc/IComputerRegistry",
    }
}

/// Next value of `StreamerSettings.ServerRotation`.
///
/// # Arguments
/// * `current` - current rotation.
///
/// # Returns
/// `(current + 1) % 6`.
pub fn next_rotation(current: u16) -> u16 {
    (current + 1) % ROTATIONS
}

/// Relay endpoint of a channel.
///
/// # Arguments
/// * `region` - relay region.
/// * `channel` - channel.
/// * `rotation` - `ServerRotation`.
///
/// # Returns
/// `relay_ip : port`, where the messaging port is `38811 + rotation` and the media ports are
/// `lan port + 1 + rotation`.
pub fn relay_endpoint(region: ServerRegion, channel: Channel, rotation: u16) -> SocketAddr {
    let port = channel.relay_port(rotation).unwrap_or(MESSAGING_RELAY_BASE + rotation);
    SocketAddr::new(relay_ip(region), port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions() {
        assert_eq!(region_for_utc_offset(-5.0), ServerRegion::AmericaCentral);
        assert_eq!(region_for_utc_offset(1.0), ServerRegion::Europe);
        assert_eq!(relay_endpoint(ServerRegion::Europe, Channel::Messaging, 2).port(), 38813);
        assert_eq!(next_rotation(5), 0);
    }
}
