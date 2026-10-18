//! LAN wake-up broadcast and Wake-on-LAN.
//!
//! * C#: `ConnectionManager._0007` — after resume, two empty UDP datagrams are broadcast to
//!   `255.255.255.255:38860` so that headsets on the LAN re-query the PC.
//! * C#: `VirtualDesktop.Net.WOLHelper` — handles the headset's `WakeUpComputer` request by
//!   sending a 102-byte magic packet (6 × `0xFF` + 16 × MAC) from UDP port 38860 to the broadcast
//!   address on port 7, falling back to port 9.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use tokio::net::UdpSocket;
use tracing::debug;
use vd_protocol::channels::BROADCAST_PORT;

use crate::error::Result;

/// Parses a MAC address written as 12 hex digits with optional `:`/`-` separators
/// (C#: `PhysicalAddress.Parse`).
///
/// # Arguments
/// * `s` - MAC string.
///
/// # Returns
/// The 6 bytes, or `None` if malformed.
pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 12 {
        return None;
    }
    let mut out = [0u8; 6];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Builds a Wake-on-LAN magic packet.
///
/// # Arguments
/// * `mac` - target MAC.
///
/// # Returns
/// The 102-byte packet.
pub fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut p = [0xFFu8; 102];
    for i in 1..=16 {
        p[i * 6..i * 6 + 6].copy_from_slice(&mac);
    }
    p
}

/// Sends magic packets for every MAC in `macs`.  C#: `WOLHelper._0001(IEnumerable<string>)`.
///
/// # Arguments
/// * `macs` - MAC address strings (invalid ones are skipped, as in the original).
///
/// # Returns
/// `true` if the socket could be opened and packets were sent (the original returns `false`
/// only when an exception escapes).
///
/// # Errors
/// [`crate::NetError::Io`] if the UDP socket cannot be created.
pub async fn wake_on_lan(macs: &[String]) -> Result<bool> {
    let socket = match UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, BROADCAST_PORT)).await {
        Ok(s) => s,
        Err(_) => UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).await?,
    };
    socket.set_broadcast(true)?;
    for mac in macs {
        let Some(bytes) = parse_mac(mac) else {
            debug!(mac, "skipping malformed MAC");
            continue;
        };
        let pkt = magic_packet(bytes);
        if socket.send_to(&pkt, SocketAddr::from((Ipv4Addr::BROADCAST, 7))).await.is_err() {
            let _ = socket.send_to(&pkt, SocketAddr::from((Ipv4Addr::BROADCAST, 9))).await;
        }
    }
    Ok(true)
}

/// Broadcasts the empty "PC is awake" datagrams.  C#: `ConnectionManager._0007`.
///
/// # Errors
/// [`crate::NetError::Io`] if the socket cannot be created; send failures are ignored.
pub async fn broadcast_wake_announcement() -> Result<()> {
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).await?;
    socket.set_broadcast(true)?;
    let target = SocketAddr::from((Ipv4Addr::BROADCAST, BROADCAST_PORT));
    for _ in 0..2 {
        let _ = socket.send_to(&[], target).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_and_packet() {
        let mac = parse_mac("AA-BB-CC-dd:ee:01").unwrap();
        assert_eq!(mac, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x01]);
        let p = magic_packet(mac);
        assert_eq!(&p[..6], &[0xFF; 6]);
        assert_eq!(&p[96..], &mac);
    }
}
