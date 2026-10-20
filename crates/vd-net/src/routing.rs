//! NAT classification by traceroute.  C#: `VirtualDesktop.Net.TraceRoute`.
//!
//! The original pings `8.8.8.8` with TTL 1 and 2 (`System.Net.NetworkInformation.Ping` on Windows,
//! `/bin/ping -c 1 -W <s> -t <ttl> 8.8.8.8` under Mono) and classifies the router hops:
//! a hop in `100.64.0.0/10` means carrier-grade NAT; one private hop means NAT; two private hops
//! mean double NAT; a public first hop means the PC has a public address.
//!
//! This port shells out to the system `ping` on every platform so it needs no raw-socket
//! privileges.

use std::net::IpAddr;
use std::time::Duration;

use vd_protocol::types::RoutingStatus;

/// Probe target (`TraceRoute.m__0001`).
pub const TARGET: &str = "8.8.8.8";

/// Reports whether `ip` is in a private (RFC 1918 / ULA / link-local) range.
///
/// # Arguments
/// * `ip` - address.
///
/// # Returns
/// `true` for private addresses.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// Reports whether `ip` is in the carrier-grade NAT range `100.64.0.0/10`.
///
/// # Arguments
/// * `ip` - address.
///
/// # Returns
/// `true` for CGNAT addresses.
pub fn is_cgnat(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v4) if v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64)
}

/// Classifies the first router hops.
///
/// # Arguments
/// * `hops` - addresses of hop 1, 2, … (`None` for hops that did not answer).
///
/// # Returns
/// The routing status.
pub fn classify(hops: &[Option<IpAddr>]) -> RoutingStatus {
    let mut private = 0;
    for hop in hops.iter().take(2) {
        let Some(ip) = hop else { break };
        if is_cgnat(*ip) {
            return RoutingStatus::CGNAT;
        }
        if is_private(*ip) {
            private += 1;
        } else {
            break;
        }
    }
    match private {
        0 => RoutingStatus::PublicIP,
        1 => RoutingStatus::NAT,
        _ => RoutingStatus::DoubleNAT,
    }
}

/// Extracts the replying router address from `ping` output ("From x.x.x.x" on Linux/macOS,
/// "Reply from x.x.x.x" on Windows).
///
/// # Arguments
/// * `output` - ping stdout.
///
/// # Returns
/// The address, if present.
pub fn parse_ping_hop(output: &str) -> Option<IpAddr> {
    for line in output.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(idx) = lower.find("from ") {
            let rest = &line[idx + 5..];
            let token: String = rest.chars().take_while(|c| c.is_ascii_hexdigit() || *c == '.' || *c == ':').collect();
            let token = token.trim_end_matches(':');
            if let Ok(ip) = token.parse() {
                return Some(ip);
            }
        }
    }
    None
}

/// Pings [`TARGET`] with the given TTL and returns the router that reported the expiry.
///
/// # Arguments
/// * `ttl` - hop limit.
/// * `timeout` - per-probe timeout.
///
/// # Returns
/// The hop address, or `None` if no answer / ping unavailable.
pub async fn probe_hop(ttl: u8, timeout: Duration) -> Option<IpAddr> {
    let secs = timeout.as_secs().max(1).to_string();
    let millis = timeout.as_millis().max(1).to_string();
    let ttl = ttl.to_string();
    let mut cmd = if cfg!(windows) {
        let mut c = crate::command("ping");
        c.args(["-n", "1", "-w", &millis, "-i", &ttl, TARGET]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = crate::command("/sbin/ping");
        c.args(["-c", "1", "-t", &secs, "-m", &ttl, TARGET]);
        c
    } else {
        let mut c = crate::command("ping");
        c.args(["-c", "1", "-W", &secs, "-t", &ttl, TARGET]);
        c
    };
    cmd.kill_on_drop(true);
    let out = tokio::time::timeout(timeout + Duration::from_secs(2), cmd.output()).await.ok()?.ok()?;
    parse_ping_hop(&String::from_utf8_lossy(&out.stdout))
}

/// Runs the two-hop probe and classifies the result.  C#: `TraceRoute._0001()`.
///
/// # Returns
/// The routing status (`PublicIP` if nothing answers, matching the original's default).
pub async fn detect_routing_status() -> RoutingStatus {
    let timeout = Duration::from_secs(1);
    let h1 = probe_hop(1, timeout).await;
    let h2 = if h1.is_some() { probe_hop(2, timeout).await } else { None };
    classify(&[h1, h2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        let p = |s: &str| Some(s.parse().unwrap());
        assert_eq!(classify(&[p("192.168.1.1"), p("8.8.4.4")]), RoutingStatus::NAT);
        assert_eq!(classify(&[p("192.168.1.1"), p("10.0.0.1")]), RoutingStatus::DoubleNAT);
        assert_eq!(classify(&[p("192.168.1.1"), p("100.70.0.1")]), RoutingStatus::CGNAT);
        assert_eq!(classify(&[p("81.2.3.4")]), RoutingStatus::PublicIP);
    }

    #[test]
    fn parses_ping_output() {
        assert_eq!(parse_ping_hop("From 192.168.1.1 icmp_seq=1 Time to live exceeded"), "192.168.1.1".parse().ok());
        assert_eq!(parse_ping_hop("Reply from 10.0.0.1: TTL expired in transit."), "10.0.0.1".parse().ok());
    }
}
