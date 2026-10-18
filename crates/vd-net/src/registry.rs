//! Client for the `ComputerRegistry` web service.  C#: `VirtualDesktop.Interfaces.IComputerRegistry`
//! via WCF `BasicHttpBinding(BasicHttpSecurityMode.Transport)` (SOAP 1.1 over HTTPS, 6 s send
//! timeout).
//!
//! The service contract and data contracts carry no explicit namespaces, so WCF defaults apply:
//! operations live in `http://tempuri.org/` with `SOAPAction:
//! "http://tempuri.org/IComputerRegistry/<Operation>"`, data contracts in
//! `http://schemas.datacontract.org/2004/07/VirtualDesktop.Interfaces`, and
//! `DataContractSerializer` writes members in **alphabetical** order.  These conventions are
//! inferred from the binding configuration and should be confirmed against live traffic.

use std::net::IpAddr;
use std::time::Duration;

use tracing::debug;
use vd_protocol::types::{Os, Platform};

use crate::adapters::NetworkAdapterInfo;
use crate::error::{NetError, Result};
use crate::stream::SessionKey;

const NS_SERVICE: &str = "http://tempuri.org/";
const NS_IFACES: &str = "http://schemas.datacontract.org/2004/07/VirtualDesktop.Interfaces";
const NS_SYSTEM: &str = "http://schemas.datacontract.org/2004/07/System";
const NS_ARRAYS: &str = "http://schemas.microsoft.com/2003/10/Serialization/Arrays";
const NS_XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";

/// An account the computer is registered for.  C#: `VirtualDesktop.Interfaces.PlatformAccessToken`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformAccessToken {
    /// Store platform.
    pub platform: Platform,
    /// User name / account id typed into the streamer.
    pub account_id: String,
    /// Numeric user id (0 when unknown).
    pub user_id: u64,
    /// String user id (Pico, Apple, Steam).
    pub user_id_string: Option<String>,
    /// Proof token (OAuth-backed platforms).
    pub user_proof: Option<String>,
}

impl PlatformAccessToken {
    /// Creates a token from a user name only, as the "Add account" dialog does.
    ///
    /// # Arguments
    /// * `platform` - store platform.
    /// * `account_id` - user name.
    ///
    /// # Returns
    /// The token.
    pub fn new(platform: Platform, account_id: impl Into<String>) -> Self {
        Self { platform, account_id: account_id.into(), user_id: 0, user_id_string: None, user_proof: None }
    }
}

/// Session credentials issued by the registry.  C#: `VirtualDesktop.Interfaces.RegistrationToken`.
#[derive(Clone, PartialEq, Eq)]
pub struct RegistrationToken {
    /// Opaque id sent first on every relay connection.
    pub connection_id: Vec<u8>,
    /// AES key material.
    pub key: SessionKey,
    /// `false` when the streamer version is rejected ("Streamer needs to be updated").
    pub is_valid: bool,
}

impl std::fmt::Debug for RegistrationToken {
    /// Formats the token without revealing secrets.
    ///
    /// # Arguments
    /// * `f` - formatter.
    ///
    /// # Returns
    /// Formatter result.
    ///
    /// # Errors
    /// Only formatter errors.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistrationToken").field("connection_id_len", &self.connection_id.len()).field("is_valid", &self.is_valid).finish()
    }
}

/// Parameters of `RegisterComputer5`.
#[derive(Debug, Clone)]
pub struct Registration {
    /// Accounts allowed to connect.
    pub accounts: Vec<PlatformAccessToken>,
    /// Stable computer id (Windows `ProductId`; `/etc/machine-id` on Linux).
    pub id: String,
    /// Computer name.
    pub name: String,
    /// Description (Windows `srvcomment`).
    pub description: String,
    /// Reported operating system.
    pub os: Os,
    /// Reported streamer version `(major, minor, build, revision)`.
    pub version: (i32, i32, i32, i32),
    /// Private adapters.
    pub adapters: Vec<NetworkAdapterInfo>,
    /// `StreamerSettings.AllowRemoteConnections`.
    pub allow_remote_connections: bool,
    /// `DynamicSettings.IsLocalTrafficEncrypted`.
    pub encrypt_local_traffic: bool,
    /// Always `true` in the original.
    pub encrypt_remote_traffic: bool,
}

/// Escapes text for XML element content.
///
/// # Arguments
/// * `s` - raw text.
///
/// # Returns
/// The escaped text.
pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// Reverses [`xml_escape`] (plus numeric character references).
///
/// # Arguments
/// * `s` - escaped text.
///
/// # Returns
/// The raw text.
pub fn xml_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(end) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let ent = &tail[1..end];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=end]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64-encodes bytes (`xs:base64Binary`, how `byte[]` data members are serialised).
///
/// # Arguments
/// * `data` - bytes.
///
/// # Returns
/// Standard padded base64.
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decodes standard base64 (whitespace ignored).
///
/// # Arguments
/// * `s` - base64 text.
///
/// # Returns
/// The bytes, or `None` for invalid input.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            break;
        }
        let v = B64.iter().position(|&x| x == c)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Finds the text content of the first element whose *local* name is `local` (any prefix).
///
/// # Arguments
/// * `xml` - document.
/// * `local` - local element name.
///
/// # Returns
/// `Some(content)` (unescaped; empty for `<x/>` and `i:nil` elements) or `None` if absent.
pub fn element_text(xml: &str, local: &str) -> Option<String> {
    let mut search = 0;
    while let Some(rel) = xml[search..].find('<') {
        let start = search + rel;
        let tag_end = start + xml[start..].find('>')?;
        let tag = &xml[start + 1..tag_end];
        search = tag_end + 1;
        if tag.starts_with('/') || tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        let name = tag.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        let lname = name.rsplit(':').next().unwrap_or(name);
        if lname != local {
            continue;
        }
        if tag.ends_with('/') {
            return Some(String::new());
        }
        let close = format!("</{name}>");
        let end = xml[search..].find(&close)? + search;
        return Some(xml_unescape(&xml[search..end]));
    }
    None
}

fn nil_or(tag: &str, value: Option<&str>) -> String {
    match value {
        Some(v) => format!("<a:{tag}>{}</a:{tag}>", xml_escape(v)),
        None => format!("<a:{tag} i:nil=\"true\"/>"),
    }
}

fn token_xml(t: &PlatformAccessToken) -> String {
    // DataContractSerializer order: AccountID, Platform, UserID, UserIDString, UserProof.
    format!(
        "<a:PlatformAccessToken><a:AccountID>{}</a:AccountID><a:Platform>{}</a:Platform><a:UserID>{}</a:UserID>{}{}</a:PlatformAccessToken>",
        xml_escape(&t.account_id),
        t.platform.member_name(),
        t.user_id,
        nil_or("UserIDString", t.user_id_string.as_deref()),
        nil_or("UserProof", t.user_proof.as_deref()),
    )
}

fn adapter_xml(a: &NetworkAdapterInfo) -> String {
    // Alphabetical: IPAddresString, IsGigabit, IsWireless, MACAddresString.
    format!(
        "<a:NetworkAdapter><a:IPAddresString>{}</a:IPAddresString><a:IsGigabit>{}</a:IsGigabit><a:IsWireless>{}</a:IsWireless><a:MACAddresString>{}</a:MACAddresString></a:NetworkAdapter>",
        a.ip, a.is_gigabit, a.is_wireless, xml_escape(&a.mac)
    )
}

fn envelope(body: &str) -> String {
    format!(
        "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body>{body}</s:Body></s:Envelope>"
    )
}

/// Builds the SOAP body of `RegisterComputer5`.
///
/// # Arguments
/// * `r` - registration parameters.
///
/// # Returns
/// The full SOAP envelope.
pub fn register_computer5_envelope(r: &Registration) -> String {
    let tokens: String = r.accounts.iter().map(token_xml).collect();
    let adapters: String = r.adapters.iter().filter(|a| matches!(a.ip, IpAddr::V4(_))).map(adapter_xml).collect();
    let (major, minor, build, revision) = r.version;
    envelope(&format!(
        "<RegisterComputer5 xmlns=\"{NS_SERVICE}\">\
<accessTokens xmlns:a=\"{NS_IFACES}\" xmlns:i=\"{NS_XSI}\">{tokens}</accessTokens>\
<id>{}</id><name>{}</name><description>{}</description><os>{}</os>\
<version xmlns:a=\"{NS_SYSTEM}\" xmlns:i=\"{NS_XSI}\"><a:_Build>{build}</a:_Build><a:_Major>{major}</a:_Major><a:_Minor>{minor}</a:_Minor><a:_Revision>{revision}</a:_Revision></version>\
<privateAdapters xmlns:a=\"{NS_IFACES}\" xmlns:i=\"{NS_XSI}\">{adapters}</privateAdapters>\
<allowRemoteConnections>{}</allowRemoteConnections><encryptLocalTraffic>{}</encryptLocalTraffic><encryptRemoteTraffic>{}</encryptRemoteTraffic>\
</RegisterComputer5>",
        xml_escape(&r.id),
        xml_escape(&r.name),
        xml_escape(&r.description),
        r.os.member_name(),
        r.allow_remote_connections,
        r.encrypt_local_traffic,
        r.encrypt_remote_traffic,
    ))
}

/// Parses a `RegisterComputer5Response`.
///
/// # Arguments
/// * `xml` - response document.
///
/// # Returns
/// `Some(token)` or `None` when the result is `nil` (the original treats `null` as "retry later").
///
/// # Errors
/// [`NetError::Soap`] for SOAP faults or undecodable fields.
pub fn parse_register_response(xml: &str) -> Result<Option<RegistrationToken>> {
    if let Some(fault) = element_text(xml, "faultstring") {
        return Err(NetError::Soap(fault));
    }
    let Some(result) = element_text(xml, "RegisterComputer5Result") else {
        return Ok(None);
    };
    if result.trim().is_empty() {
        return Ok(None);
    }
    let field = |name: &str| -> Result<Vec<u8>> {
        let text = element_text(&result, name).ok_or_else(|| NetError::Soap(format!("missing {name}")))?;
        base64_decode(&text).ok_or_else(|| NetError::Soap(format!("{name} is not base64")))
    };
    let is_valid = element_text(&result, "_isValid").is_some_and(|v| v.trim() == "true");
    Ok(Some(RegistrationToken {
        connection_id: field("ConnectionID")?,
        key: SessionKey { key: field("_key")?, iv: field("_iv")? },
        is_valid,
    }))
}

/// HTTP client for the registry.
#[derive(Clone)]
pub struct RegistryClient {
    http: reqwest::Client,
    url: String,
}

impl RegistryClient {
    /// Creates a client for `url` (see [`crate::relay::registry_url`]).
    ///
    /// # Arguments
    /// * `url` - SOAP endpoint.
    ///
    /// # Returns
    /// The client.
    ///
    /// # Errors
    /// [`NetError::Http`] if the TLS backend cannot be initialised.
    pub fn new(url: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(6)).build()?;
        Ok(Self { http, url: url.into() })
    }

    async fn call(&self, action: &str, envelope: String) -> Result<String> {
        debug!(action, "SOAP call");
        let resp = self
            .http
            .post(&self.url)
            .header("Content-Type", "text/xml; charset=utf-8")
            .header("SOAPAction", format!("\"{NS_SERVICE}IComputerRegistry/{action}\""))
            .body(envelope)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() && element_text(&text, "faultstring").is_none() {
            return Err(NetError::Soap(format!("HTTP {status}")));
        }
        Ok(text)
    }

    /// Registers the computer.  C#: `IComputerRegistry.RegisterComputer5`.
    ///
    /// # Arguments
    /// * `r` - registration parameters.
    ///
    /// # Returns
    /// The token, or `None` when the service returned `null`.
    ///
    /// # Errors
    /// HTTP or SOAP errors.
    pub async fn register_computer(&self, r: &Registration) -> Result<Option<RegistrationToken>> {
        let xml = self.call("RegisterComputer5", register_computer5_envelope(r)).await?;
        parse_register_response(&xml)
    }

    /// Unregisters the computer.  C#: `IComputerRegistry.UnregisterComputer3` (one-way).
    ///
    /// # Arguments
    /// * `connection_id` - id from the registration token.
    ///
    /// # Errors
    /// HTTP errors.
    pub async fn unregister_computer(&self, connection_id: &[u8]) -> Result<()> {
        let body = format!("<UnregisterComputer3 xmlns=\"{NS_SERVICE}\"><connectionID>{}</connectionID></UnregisterComputer3>", base64_encode(connection_id));
        self.call("UnregisterComputer3", envelope(&body)).await.map(|_| ())
    }

    /// Asks the service to wake other registered computers.  C#: `IComputerRegistry.WakeUpComputer`.
    ///
    /// # Arguments
    /// * `token` - account.
    /// * `macs` - MAC addresses.
    ///
    /// # Errors
    /// HTTP errors.
    pub async fn wake_up_computer(&self, token: &PlatformAccessToken, macs: &[String]) -> Result<()> {
        let macs_xml: String = macs.iter().map(|m| format!("<b:string>{}</b:string>", xml_escape(m))).collect();
        let tok = token_xml(token).replace("<a:PlatformAccessToken>", "").replace("</a:PlatformAccessToken>", "");
        let body = format!(
            "<WakeUpComputer xmlns=\"{NS_SERVICE}\"><accessToken xmlns:a=\"{NS_IFACES}\" xmlns:i=\"{NS_XSI}\">{tok}</accessToken><macAddresses xmlns:b=\"{NS_ARRAYS}\">{macs_xml}</macAddresses></WakeUpComputer>"
        );
        self.call("WakeUpComputer", envelope(&body)).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for v in [&b""[..], b"f", b"fo", b"foo", b"foobar"] {
            assert_eq!(base64_decode(&base64_encode(v)).unwrap(), v);
        }
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn parses_token() {
        let xml = "<s:Envelope><s:Body><RegisterComputer5Response><RegisterComputer5Result xmlns:a=\"x\"><a:ConnectionID>AQID</a:ConnectionID><a:_isValid>true</a:_isValid><a:_iv>AAAAAAAAAAAAAAAAAAAAAA==</a:_iv><a:_key>AAAAAAAAAAAAAAAAAAAAAA==</a:_key></RegisterComputer5Result></RegisterComputer5Response></s:Body></s:Envelope>";
        let t = parse_register_response(xml).unwrap().unwrap();
        assert_eq!(t.connection_id, [1, 2, 3]);
        assert!(t.is_valid);
        assert_eq!(t.key.iv.len(), 16);
    }

    #[test]
    fn envelope_contains_alphabetical_members() {
        let r = Registration {
            accounts: vec![PlatformAccessToken::new(Platform::OculusQuest, "me & you")],
            id: "id".into(),
            name: "pc".into(),
            description: String::new(),
            os: Os::Windows,
            version: (1, 34, 18, 0),
            adapters: vec![],
            allow_remote_connections: true,
            encrypt_local_traffic: false,
            encrypt_remote_traffic: true,
        };
        let x = register_computer5_envelope(&r);
        assert!(x.contains("<a:AccountID>me &amp; you</a:AccountID><a:Platform>OculusQuest</a:Platform>"));
        assert!(x.find("_Build").unwrap() < x.find("_Major").unwrap());
    }
}
