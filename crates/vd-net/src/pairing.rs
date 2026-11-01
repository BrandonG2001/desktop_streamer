//! Discovery requests of headsets on the LAN, used for the "Ask for computer access" prompt.
//!
//! C#: `_0084_0002._0002`, started by `_0095_0002._0002._0001(...)` while the computer registers
//! with vrdesktop.net.  A headset that searches for computers sends a UDP datagram to port 38850
//! (IPv4 broadcast, or the IPv6 all-nodes group `ff02::1`).  The datagram is one or more AES
//! records ([`vd_protocol::crypto`]) under a key and IV built into the streamer.  Its plaintext is:
//!
//! ```text
//! [0..243)  the headset's 1024-bit RSA public key as .NET <RSAKeyValue> XML (ASCII)
//! then either
//!   0x00, platform u8, account id (UTF-8)                        unsigned; Meta (OculusQuest) only
//!   0x01, signature[256], 2 bytes, platform u8, account id (UTF-8)
//! ```
//!
//! The signature covers `platform || account id` and is made with Virtual Desktop's 2048-bit
//! signing key ([`SIGNING_MODULUS`]); requests whose signature does not verify are dropped.
//!
//! The original answers requests from accounts on its list with an encrypted `Computer`
//! description, and raises a pairing event for any other account, once per headset key.  The
//! port only reports the requests ([`listen`]); headsets with a known account keep finding the
//! computer through the vrdesktop.net registry.

use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};

use rsa::traits::SignatureScheme;
use rsa::{BigUint, Pkcs1v15Sign, Pss, RsaPublicKey};
use sha2::Digest;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, info, trace};
use vd_protocol::crypto::{BLOCK, RecordDecryptor};
use vd_protocol::types::Platform;

use crate::error::{NetError, Result};
use crate::registry::base64_decode;

/// UDP port of discovery requests.
pub const DISCOVERY_PORT: u16 = 38850;

/// AES-256 key of discovery requests (C#: static field `m__0002` of `_0084_0002._0002`).
const DISCOVERY_KEY: [u8; 32] = [32, 71, 236, 94, 194, 34, 85, 255, 165, 172, 187, 150, 6, 104, 106, 57, 57, 62, 244, 114, 75, 174, 237, 9, 48, 36, 239, 82, 57, 98, 205, 80];

/// AES IV of discovery requests (C#: static field `_0003` of `_0084_0002._0002`).
const DISCOVERY_IV: [u8; 16] = [82, 200, 129, 118, 144, 104, 249, 4, 62, 20, 120, 110, 20, 180, 63, 31];

/// Modulus of Virtual Desktop's request-signing key (exponent 65537), base64 as in the original.
const SIGNING_MODULUS: &str = "me0As0RFsds/LfBcxyaFpK4xsTHbUxoql7VxqRJlfNoce4WhwaycH19VovDUelXeelcFc7dfGCm8D8AG30TKa3V8ntni3v4PkTnw6KSXka76svoUtgCgNz9u2P3g17KuiWGN7hzJE2c84Pv25VgMhPhZfI/qv8JmYhU/pQ1XMc6t8Qa/CWLUqf9wIh/6Ey3wHFmwSpg9GhyPYDEUI+Ppn36Ia+2SjI4t764mY4OehVutBAsEOpFL9rlyGQggsrfOErp1xN6U5VM1Bibzqftjnjs5hy6bi/G5GS8lvGs25+gK8YSnhau/tpiFvJeZNE6vtaKO/yofqYD/e6L9MJPXpQ==";

/// Length of the headset's RSA key XML at the start of a request.
const KEY_XML_LEN: usize = 243;

/// Length of the request signature.
const SIGNATURE_LEN: usize = 256;

/// A headset searching for computers.  C#: `_001D._0002` (pairing event arguments).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryRequest {
    /// Store platform of the headset's account.
    pub platform: Platform,
    /// Account id (user name) the headset is signed in with.
    pub account_id: String,
    /// The headset's RSA public key XML; identifies the headset (the original prompts once per key).
    pub headset_key: String,
    /// The request carried a valid Virtual Desktop signature.
    pub signed: bool,
}

/// Converts a platform byte of a request.
///
/// # Arguments
/// * `b` - `Platform` value.
///
/// # Returns
/// The platform, or `None` for an unknown value.
fn platform_from_byte(b: u8) -> Option<Platform> {
    Some(match b {
        0 => Platform::Oculus,
        1 => Platform::OculusQuest,
        2 => Platform::Vive,
        3 => Platform::Pico,
        5 => Platform::Google,
        6 => Platform::PlayForDream,
        7 => Platform::Apple,
        8 => Platform::Steam,
        _ => return None,
    })
}

/// Decrypts every complete record of a datagram (C#: `_0010_0002._0001` reading a `MemoryStream`).
///
/// # Arguments
/// * `packet` - the datagram.
///
/// # Returns
/// The concatenated record payloads; a trailing incomplete record is ignored.
fn decrypt(packet: &[u8]) -> Vec<u8> {
    let Ok(mut dec) = RecordDecryptor::new(&DISCOVERY_KEY, &DISCOVERY_IV) else { return Vec::new() };
    let mut out = Vec::new();
    let mut rest = packet;
    while rest.len() >= BLOCK {
        let mut first = [0u8; BLOCK];
        first.copy_from_slice(&rest[..BLOCK]);
        if dec.decrypt_blocks(&mut first).is_err() {
            break;
        }
        let Ok(layout) = RecordDecryptor::layout(&first) else { break };
        if rest.len() < layout.total_len {
            break;
        }
        let mut body = rest[BLOCK..layout.total_len].to_vec();
        if dec.decrypt_blocks(&mut body).is_err() {
            break;
        }
        let mut record = first.to_vec();
        record.extend_from_slice(&body);
        out.extend_from_slice(&record[layout.data_offset..layout.data_offset + layout.data_len]);
        rest = &rest[layout.total_len..];
    }
    out
}

/// Checks a request signature against Virtual Desktop's signing key.
///
/// The hash and padding of the original's `RSA.VerifyData` call are bound at run time by the
/// obfuscator, so the usual combinations (SHA-256/SHA-1/SHA-512 with PKCS#1 v1.5 or PSS) are
/// all accepted.
///
/// # Arguments
/// * `data` - signed bytes (`platform || account id`).
/// * `signature` - 256-byte signature.
///
/// # Returns
/// `true` if one combination verifies.
fn verify_signature(data: &[u8], signature: &[u8]) -> bool {
    let Some(n) = base64_decode(SIGNING_MODULUS) else { return false };
    let Ok(key) = RsaPublicKey::new(BigUint::from_bytes_be(&n), BigUint::from(65537u32)) else { return false };
    let sha256 = sha2::Sha256::digest(data);
    let sha512 = sha2::Sha512::digest(data);
    let sha1 = sha1::Sha1::digest(data);
    Pkcs1v15Sign::new::<sha2::Sha256>().verify(&key, &sha256, signature).is_ok()
        || Pkcs1v15Sign::new::<sha1::Sha1>().verify(&key, &sha1, signature).is_ok()
        || Pkcs1v15Sign::new::<sha2::Sha512>().verify(&key, &sha512, signature).is_ok()
        || Pss::new::<sha2::Sha256>().verify(&key, &sha256, signature).is_ok()
}

/// Parses a discovery datagram.  C#: the receive loop of `_0084_0002._0002._0002(...)`.
///
/// # Arguments
/// * `packet` - the datagram.
///
/// # Returns
/// The request, or `None` for anything else: datagrams of 243 bytes or less (17-byte connect
/// requests, for instance), malformed requests, unsigned requests from other platforms than
/// Meta, and bad signatures.
pub fn parse_request(packet: &[u8]) -> Option<DiscoveryRequest> {
    if packet.len() <= KEY_XML_LEN {
        return None;
    }
    let plain = decrypt(packet);
    if plain.len() <= KEY_XML_LEN {
        return None;
    }
    let headset_key = String::from_utf8(plain[..KEY_XML_LEN].to_vec()).ok()?;
    let body = &plain[KEY_XML_LEN..];
    let (platform, account, signed) = match body.first()? {
        0 => {
            let platform = platform_from_byte(*body.get(1)?)?;
            if platform != Platform::OculusQuest {
                return None;
            }
            (platform, &body[2..], false)
        }
        1 => {
            let start = 1 + SIGNATURE_LEN + 2;
            if body.len() <= start {
                return None;
            }
            if !verify_signature(&body[start..], &body[1..1 + SIGNATURE_LEN]) {
                let account = String::from_utf8_lossy(&body[start + 1..]);
                info!(platform = body[start], %account, "ignored a LAN discovery request whose signature does not verify");
                return None;
            }
            (platform_from_byte(body[start])?, &body[start + 1..], true)
        }
        _ => return None,
    };
    let account_id = String::from_utf8(account.to_vec()).ok()?;
    if account_id.is_empty() {
        return None;
    }
    Some(DiscoveryRequest { platform, account_id, headset_key, signed })
}

/// Opens the discovery socket: dual-stack `[::]:38850`, joined to `ff02::1` on every interface.
///
/// # Returns
/// The socket.
///
/// # Errors
/// [`NetError::AddrInUse`] / [`NetError::AccessDenied`] when the port cannot be bound (for
/// example while the official streamer runs), other socket errors as [`NetError::Io`].
fn bind() -> Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_only_v6(false)?;
    let addr = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, DISCOVERY_PORT, 0, 0));
    socket.bind(&addr.into()).map_err(NetError::from_bind)?;
    socket.set_nonblocking(true)?;
    let group = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1);
    let mut indexes: Vec<u32> = if_addrs::get_if_addrs().unwrap_or_default().into_iter().filter(|i| !i.is_loopback()).filter_map(|i| i.index).collect();
    indexes.sort_unstable();
    indexes.dedup();
    for index in indexes {
        if let Err(e) = socket.join_multicast_v6(&group, index) {
            debug!(index, error = %e, "cannot join ff02::1 on interface");
        }
    }
    Ok(UdpSocket::from_std(socket.into())?)
}

/// Receives discovery requests until `tx` is closed or the task is cancelled.
///
/// # Arguments
/// * `tx` - receives each valid request (duplicates included; the caller decides what is new).
///
/// # Errors
/// Binding errors from [`bind`]; receive errors are skipped.
pub async fn listen(tx: mpsc::UnboundedSender<DiscoveryRequest>) -> Result<()> {
    let socket = bind()?;
    debug!(port = DISCOVERY_PORT, "listening for headsets searching the LAN");
    let mut buf = vec![0u8; 2048];
    loop {
        let (n, from) = tokio::select! {
            r = socket.recv_from(&mut buf) => match r {
                Ok(r) => r,
                // Windows reports ICMP "port unreachable" from earlier sends as receive errors.
                Err(_) => continue,
            },
            () = tx.closed() => return Ok(()),
        };
        if let Some(request) = parse_request(&buf[..n]) {
            trace!(%from, platform = ?request.platform, account = %request.account_id, "headset searching for computers");
            if tx.send(request).is_err() {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vd_protocol::crypto::RecordEncryptor;

    /// A 243-byte stand-in for a headset's `<RSAKeyValue>` XML.
    fn key_xml() -> String {
        let modulus = "A".repeat(170) + "==";
        let xml = format!("<RSAKeyValue><Modulus>{modulus}</Modulus><Exponent>AQAB</Exponent></RSAKeyValue>");
        assert_eq!(xml.len(), KEY_XML_LEN);
        xml
    }

    /// Encrypts `parts` as consecutive records, like successive `Write` calls of the headset.
    fn seal(parts: &[&[u8]]) -> Vec<u8> {
        let mut enc = RecordEncryptor::new(&DISCOVERY_KEY, &DISCOVERY_IV).unwrap();
        parts.iter().flat_map(|p| enc.seal(p).unwrap()).collect()
    }

    #[test]
    fn unsigned_meta_request() {
        let xml = key_xml();
        let packet = seal(&[xml.as_bytes(), &[0, 1], b"someone"]);
        let r = parse_request(&packet).expect("request");
        assert_eq!((r.platform, r.account_id.as_str(), r.signed), (Platform::OculusQuest, "someone", false));
        assert_eq!(r.headset_key, xml);
    }

    #[test]
    fn unsigned_requests_are_meta_only() {
        let xml = key_xml();
        assert_eq!(parse_request(&seal(&[xml.as_bytes(), &[0, 3], b"someone"])), None);
    }

    #[test]
    fn bad_signature_is_dropped() {
        let xml = key_xml();
        let mut body = vec![1u8];
        body.extend_from_slice(&[0x55; SIGNATURE_LEN]);
        body.extend_from_slice(&[0, 0, 1]);
        body.extend_from_slice(b"someone");
        assert_eq!(parse_request(&seal(&[xml.as_bytes(), &body])), None);
    }

    #[test]
    fn short_datagrams_are_not_requests() {
        assert_eq!(parse_request(&[0u8; 17]), None);
        assert_eq!(parse_request(&[0u8; 400]), None);
    }

    /// Binds the real port 38850, so it is skipped by default (run with `--ignored` while no
    /// other streamer runs).
    #[tokio::test]
    #[ignore]
    async fn listener_receives_ipv4_ipv6_and_broadcast() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(listen(tx));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let xml = key_xml();
        let sender = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        sender.set_broadcast(true).unwrap();
        let sender6 = UdpSocket::bind("[::]:0").await.unwrap();
        for (i, target) in ["127.0.0.1", "[::1]", "255.255.255.255"].iter().enumerate() {
            let packet = seal(&[xml.as_bytes(), &[0, 1], format!("user{i}").as_bytes()]);
            let addr = format!("{target}:{DISCOVERY_PORT}");
            let s = if target.starts_with('[') { &sender6 } else { &sender };
            s.send_to(&packet, &addr).await.unwrap();
            let r = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await.expect(target).unwrap();
            assert_eq!(r.account_id, format!("user{i}"));
        }
        task.abort();
    }

    #[test]
    fn signing_key_is_2048_bit() {
        assert_eq!(base64_decode(SIGNING_MODULUS).map(|m| m.len()), Some(256));
    }
}
