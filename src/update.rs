//! Update checker.  C#: `VirtualDesktop.Streamer.UpdateHelper` and the ABOUT tab's
//! "Check for updates" button (`OnCheckForUpdatesClick`).
//!
//! The original runs its installer's `Updater.exe /justcheck` (Advanced Installer, reading
//! `download.vrdesktop.net`) at startup and from the button, and on success starts
//! `Updater.exe /silentall` and exits.  The port reads the latest release of its GitHub
//! repository instead.  An installed Windows copy downloads the release's setup program and runs
//! it silently; the setup replaces the files and starts the streamer again.  Portable and Linux
//! copies only point to the release page.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::info;

/// Release page shown when the update cannot be installed automatically.
pub const RELEASES_URL: &str = "https://github.com/BrandonG2001/desktop_streamer/releases/latest";
/// GitHub API endpoint of the latest (non-prerelease) release.
const LATEST_API: &str = "https://api.github.com/repos/BrandonG2001/desktop_streamer/releases/latest";
/// Suffix of the Windows installer asset (`VirtualDesktopStreamerRust-<version>-setup-x64.exe`).
const INSTALLER_SUFFIX: &str = "-setup-x64.exe";
/// Timeout of the version query (C#: `HttpClient.Timeout` of 10 s).
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
/// Command-line options of the silent update.  `/RESTARTAPP=1` makes the setup start the
/// streamer again when it is done (see `packaging/windows/installer.iss`).
const SETUP_ARGS: [&str; 5] = ["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS", "/RESTARTAPP=1"];

/// A newer release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Version without the `v` prefix, e.g. `1.5.0`.
    pub version: String,
    /// Release page.
    pub page: String,
    /// Windows installer, when the release has one.
    pub installer: Option<Asset>,
}

/// A downloadable file of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// File name.
    pub name: String,
    /// Download URL.
    pub url: String,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256 as lower-case hex, when GitHub reports one.
    pub sha256: Option<String>,
}

/// Part of the GitHub release JSON that is used.
#[derive(Debug, Deserialize)]
struct ApiRelease {
    /// Tag, e.g. `v1.5.0`.
    tag_name: String,
    /// Release page.
    html_url: String,
    /// Attached files.
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

/// One entry of `assets` in the GitHub release JSON.
#[derive(Debug, Deserialize)]
struct ApiAsset {
    /// File name.
    name: String,
    /// Download URL.
    browser_download_url: String,
    /// Size in bytes.
    size: u64,
    /// `sha256:<hex>`, reported for uploads since mid-2025.
    #[serde(default)]
    digest: Option<String>,
}

/// Parses a version such as `v1.4.0` or `1.4`.
///
/// # Arguments
/// * `s` - version or tag.
///
/// # Returns
/// Major, minor and patch (missing parts are 0), or `None` if a part is not a number.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.trim().trim_start_matches(['v', 'V']).split('.');
    let mut next = || parts.next().map_or(Some(0), |p| p.parse().ok());
    let v = (next()?, next()?, next()?);
    parts.next().is_none().then_some(v)
}

/// Whether `latest` is newer than `current` (C#: `Version > CurrentVersion`).
///
/// # Arguments
/// * `latest` - version of the latest release.
/// * `current` - running version.
///
/// # Returns
/// `true` if both parse and `latest` is greater.
fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse_version(latest), parse_version(current)), (Some(l), Some(c)) if l > c)
}

/// Converts the release JSON into a [`Release`] if it is newer than `current`.
///
/// # Arguments
/// * `json` - body of the GitHub API response.
/// * `current` - running version.
///
/// # Returns
/// The release, or `None` when `current` is up to date.
///
/// # Errors
/// When the JSON is not a release.
fn newer_release(json: &[u8], current: &str) -> Result<Option<Release>> {
    let r: ApiRelease = serde_json::from_slice(json).context("unexpected reply from GitHub")?;
    if !is_newer(&r.tag_name, current) {
        return Ok(None);
    }
    let installer = r.assets.into_iter().find(|a| a.name.ends_with(INSTALLER_SUFFIX)).map(|a| Asset {
        sha256: a.digest.and_then(|d| d.strip_prefix("sha256:").map(str::to_ascii_lowercase)),
        name: a.name,
        url: a.browser_download_url,
        size: a.size,
    });
    Ok(Some(Release { version: r.tag_name.trim_start_matches(['v', 'V']).to_owned(), page: r.html_url, installer }))
}

/// HTTP client for GitHub (the API rejects requests without a `User-Agent`).
///
/// # Returns
/// The client.
///
/// # Errors
/// When the TLS backend cannot be initialised.
fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder().user_agent(concat!("desktop_streamer/", env!("CARGO_PKG_VERSION"))).build()?)
}

/// Asks GitHub for the latest release (C#: `UpdateHelper` → `Updater.exe /justcheck`).
///
/// # Returns
/// The release if it is newer than this program, `None` when up to date.
///
/// # Errors
/// When GitHub cannot be reached or replies with an error.
pub async fn check() -> Result<Option<Release>> {
    let resp = client()?
        .get(LATEST_API)
        .header("Accept", "application/vnd.github+json")
        .timeout(CHECK_TIMEOUT)
        .send()
        .await
        .context("cannot reach GitHub")?;
    if !resp.status().is_success() {
        bail!("GitHub replied {}", resp.status());
    }
    let body = resp.bytes().await.context("cannot read the reply from GitHub")?;
    let release = newer_release(&body, env!("CARGO_PKG_VERSION"))?;
    match &release {
        Some(r) => info!(version = %r.version, "update available"),
        None => info!("no update available"),
    }
    Ok(release)
}

/// Whether this copy was installed by the Windows setup, so an update can replace it (the setup
/// leaves its uninstaller next to the executable).
///
/// # Returns
/// `true` for an installed Windows copy; `false` for the portable zip and on Linux.
pub fn can_install() -> bool {
    cfg!(windows) && std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("unins000.exe").is_file())).unwrap_or(false)
}

/// Downloads the installer of `asset` into the temporary directory and checks it.
///
/// # Arguments
/// * `asset` - the installer.
/// * `progress` - receives the progress in per mille.
///
/// # Returns
/// Path of the downloaded installer.
///
/// # Errors
/// When the download fails, or the size or checksum does not match.
async fn download(asset: &Asset, progress: &AtomicU32) -> Result<PathBuf> {
    let mut resp = client()?.get(&asset.url).send().await.context("cannot download the update")?;
    if !resp.status().is_success() {
        bail!("download failed: {}", resp.status());
    }
    let mut data = Vec::with_capacity(usize::try_from(asset.size).unwrap_or(0));
    while let Some(chunk) = resp.chunk().await.context("download interrupted")? {
        data.extend_from_slice(&chunk);
        if let Some(p) = (data.len() as u64 * 1000).checked_div(asset.size) {
            progress.store(p.min(1000) as u32, Ordering::Relaxed);
        }
    }
    if data.len() as u64 != asset.size {
        bail!("download incomplete ({} of {} bytes)", data.len(), asset.size);
    }
    if let Some(expected) = &asset.sha256 {
        let actual: String = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
        if &actual != expected {
            bail!("the downloaded file is damaged (checksum mismatch)");
        }
    }
    // Only a plain file name: the asset name comes from the network.
    let name = Path::new(&asset.name).file_name().context("invalid installer name")?;
    let path = std::env::temp_dir().join(name);
    tokio::fs::write(&path, &data).await.with_context(|| format!("cannot write {}", path.display()))?;
    Ok(path)
}

/// Downloads and starts the silent update (C#: `Updater.exe /silentall -nofreqcheck
/// -reducedgui`).  Windows asks for administrator rights; the setup then installs and starts
/// the new version.  The caller quits once this returns `Ok`, so the files can be replaced.
///
/// # Arguments
/// * `release` - the release to install.
/// * `progress` - receives the download progress in per mille.
///
/// # Errors
/// When the release has no installer, the download fails or the setup cannot be started.
pub async fn install(release: &Release, progress: Arc<AtomicU32>) -> Result<()> {
    let asset = release.installer.as_ref().context("this release has no Windows installer")?;
    let path = download(asset, &progress).await?;
    info!(version = %release.version, path = %path.display(), "starting the update");
    std::process::Command::new(&path).args(SETUP_ARGS).spawn().with_context(|| format!("cannot start {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(parse_version("v1.4.0"), Some((1, 4, 0)));
        assert_eq!(parse_version("2.1"), Some((2, 1, 0)));
        assert_eq!(parse_version("1.4.0-beta"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert!(is_newer("v1.10.0", "1.9.3"));
        assert!(is_newer("1.4.1", "1.4.0"));
        assert!(!is_newer("v1.4.0", "1.4.0"));
        assert!(!is_newer("1.3.9", "1.4.0"));
        assert!(!is_newer("nightly", "1.4.0"));
    }

    #[test]
    fn release_json_is_parsed() {
        let json = br#"{
            "tag_name": "v1.5.0",
            "html_url": "https://github.com/BrandonG2001/desktop_streamer/releases/tag/v1.5.0",
            "assets": [
                {"name": "desktop_streamer-1.5.0-linux-x86_64.tar.gz", "browser_download_url": "https://x/l", "size": 10},
                {"name": "VirtualDesktopStreamerRust-1.5.0-setup-x64.exe", "browser_download_url": "https://x/s", "size": 20, "digest": "sha256:ABCD"}
            ]
        }"#;
        let r = newer_release(json, "1.4.0").unwrap().unwrap();
        assert_eq!(r.version, "1.5.0");
        let a = r.installer.unwrap();
        assert_eq!((a.url.as_str(), a.size, a.sha256.as_deref()), ("https://x/s", 20, Some("abcd")));
        assert_eq!(newer_release(json, "1.5.0").unwrap(), None);
        assert!(newer_release(b"{}", "1.4.0").is_err());
    }

    #[tokio::test]
    #[ignore = "needs the network and a public repository"]
    async fn github_is_reachable() {
        check().await.unwrap();
    }
}
