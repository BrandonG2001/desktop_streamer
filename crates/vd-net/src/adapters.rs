//! Network adapter enumeration.  C#: `_0019._0002` and `VirtualDesktop.Interfaces.NetworkAdapter`.
//!
//! The registry needs the PC's private (LAN) addresses so the headset can connect directly.
//! The original enumerates `NetworkInterface.GetAllNetworkInterfaces()`, skipping loopback,
//! interfaces that are not `Up`, and tunnel adapters (including Tailscale unless remote access
//! is allowed), and records for each IPv4 unicast address the MAC, whether the adapter is
//! wireless and whether it is at least 1 Gbit/s.

use std::net::IpAddr;

use vd_protocol::types::NetworkConnectionType;

/// One private adapter address.  C#: `NetworkAdapter` (`[DataContract]` struct; the misspelt
/// member names `IPAddresString` / `MACAddresString` are part of the wire contract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkAdapterInfo {
    /// Adapter (interface) name.
    pub name: String,
    /// IPv4 address in dotted notation.
    pub ip: IpAddr,
    /// MAC address as 12 upper-case hex digits (`PhysicalAddress.ToString()` format).
    pub mac: String,
    /// Wireless (802.11) adapter.
    pub is_wireless: bool,
    /// Link speed ≥ 1 Gbit/s (or unknown).
    pub is_gigabit: bool,
}

/// Heuristic tunnel/VPN detection by interface name (the original uses `NetworkInterfaceType.Tunnel`
/// and the "Tailscale" description).
///
/// # Arguments
/// * `name` - interface name.
///
/// # Returns
/// `true` for tunnel-like interfaces.
fn is_tunnel(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["tun", "tap", "wg", "tailscale", "zt", "utun", "ppp", "ipsec", "teredo", "isatap", "docker", "veth", "br-", "virbr"]
        .iter()
        .any(|p| n.starts_with(p) || n.contains("tailscale"))
}

#[cfg(target_os = "linux")]
fn wireless_and_gigabit(name: &str) -> (bool, bool) {
    let base = std::path::Path::new("/sys/class/net").join(name);
    let wireless = base.join("wireless").exists() || base.join("phy80211").exists();
    let speed = std::fs::read_to_string(base.join("speed")).ok().and_then(|s| s.trim().parse::<i64>().ok());
    (wireless, wireless || speed.is_none_or(|s| s >= 1000 || s < 0))
}

#[cfg(not(target_os = "linux"))]
fn wireless_and_gigabit(name: &str) -> (bool, bool) {
    let n = name.to_ascii_lowercase();
    let wireless = n.contains("wi-fi") || n.contains("wifi") || n.contains("wireless") || n.contains("wlan");
    (wireless, true)
}

/// Lists usable private IPv4 adapters.  C#: `_0019._0002._0001(bool, bool, bool)`.
///
/// # Arguments
/// * `include_tunnels` - keep tunnel/VPN interfaces (the original keeps Tailscale when remote
///   connections are allowed).
///
/// # Returns
/// Adapter addresses (empty if enumeration fails).
pub fn private_adapters(include_tunnels: bool) -> Vec<NetworkAdapterInfo> {
    let Ok(ifaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for iface in ifaces {
        if iface.is_loopback() || !iface.ip().is_ipv4() {
            continue;
        }
        if !include_tunnels && is_tunnel(&iface.name) {
            continue;
        }
        let mac = mac_address::mac_address_by_name(&iface.name)
            .ok()
            .flatten()
            .map(|m| m.bytes().iter().map(|b| format!("{b:02X}")).collect::<String>())
            .unwrap_or_default();
        let (is_wireless, is_gigabit) = wireless_and_gigabit(&iface.name);
        out.push(NetworkAdapterInfo { name: iface.name.clone(), ip: iface.ip(), mac, is_wireless, is_gigabit });
    }
    out
}

/// Classifies the link used for streaming.  C#: `_0019._0002._0001(IEnumerable<NetworkAdapter>)`.
///
/// # Arguments
/// * `adapters` - adapters from [`private_adapters`].
///
/// # Returns
/// `Wireless` if any adapter is wireless, `GigabitEthernet` if a wired adapter is gigabit,
/// `Ethernet` for slower wired links, `None` when the list is empty.
pub fn connection_type(adapters: &[NetworkAdapterInfo]) -> NetworkConnectionType {
    if adapters.is_empty() {
        NetworkConnectionType::None
    } else if adapters.iter().any(|a| a.is_wireless) {
        NetworkConnectionType::Wireless
    } else if adapters.iter().any(|a| a.is_gigabit) {
        NetworkConnectionType::GigabitEthernet
    } else {
        NetworkConnectionType::Ethernet
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_names() {
        assert!(is_tunnel("tailscale0"));
        assert!(is_tunnel("wg0"));
        assert!(!is_tunnel("eth0"));
    }
}
