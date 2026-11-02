//! Game library.  C#: `VirtualDesktop.Streamer.GameManager` (Steam, Oculus and Viveport libraries,
//! VR-capable titles only) and the `RequestGames`/`LaunchGame` handlers of `OculusStreamer`.
//!
//! This port lists installed Steam titles (the only store that exists on Linux) by reading
//! `libraryfolders.vdf` and `appmanifest_*.acf`, excluding the same tool app ids as the original
//! (Steamworks Common Redistributables 228980, SteamVR 250820, Windows Mixed Reality 719950) plus
//! Proton / Steam Linux Runtime.  The original additionally filters on the
//! `openvrsupport`/`openxrsupport` app-info flags, which requires parsing Steam's binary
//! `appinfo.vdf`; that filter is not ported.

use std::path::{Path, PathBuf};

use tracing::{info, warn};
use vd_protocol::messages::{GameEntry, GameId, encode_games};
use vd_protocol::types::GamePlatform;

use crate::app::App;

const EXCLUDED: [u64; 3] = [228_980, 250_820, 719_950];

/// Extracts every `"key" "value"` pair of a text VDF / ACF file (nesting is ignored).
///
/// # Arguments
/// * `text` - file contents.
///
/// # Returns
/// `(key, value)` pairs in file order.
pub fn vdf_pairs(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split('"').collect();
        // `\t"key"\t\t"value"` splits into ["\t", "key", "\t\t", "value", ""].
        if parts.len() >= 5 {
            out.push((parts[1].to_owned(), parts[3].replace("\\\\", "\\")));
        }
    }
    out
}

/// Candidate Steam installation directories.
///
/// # Returns
/// Existing directories.
pub fn steam_roots() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(home) = dirs::home_dir() {
        v.push(home.join(".steam/steam"));
        v.push(home.join(".local/share/Steam"));
        v.push(home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
    }
    if cfg!(windows) {
        v.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));
    }
    v.into_iter().filter(|p| p.join("steamapps").is_dir()).collect()
}

/// Lists installed Steam games.
///
/// # Returns
/// Library entries sorted by name.
pub fn steam_games() -> Vec<GameEntry> {
    let mut libraries: Vec<PathBuf> = Vec::new();
    for root in steam_roots() {
        libraries.push(root.clone());
        if let Ok(text) = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
            libraries.extend(vdf_pairs(&text).into_iter().filter(|(k, _)| k == "path").map(|(_, v)| PathBuf::from(v)));
        }
    }
    libraries.sort();
    libraries.dedup();
    let mut games = Vec::new();
    for lib in libraries {
        let Ok(rd) = std::fs::read_dir(lib.join("steamapps")) else { continue };
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().into_owned();
            if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
                continue;
            }
            if let Some(g) = parse_manifest(&e.path())
                && !games.iter().any(|x: &GameEntry| x.id == g.id) {
                    games.push(g);
                }
        }
    }
    games.sort_by_key(|a| a.name.to_lowercase());
    games
}

fn parse_manifest(path: &Path) -> Option<GameEntry> {
    let text = std::fs::read_to_string(path).ok()?;
    let pairs = vdf_pairs(&text);
    let appid: u64 = pairs.iter().find(|(k, _)| k == "appid")?.1.parse().ok()?;
    let name = pairs.iter().find(|(k, _)| k == "name")?.1.clone();
    if EXCLUDED.contains(&appid) || name.starts_with("Proton") || name.starts_with("Steam Linux Runtime") || name.starts_with("Steamworks") {
        return None;
    }
    Some(GameEntry { id: GameId { platform: GamePlatform::Steam, id: appid }, name })
}

/// Sends the `Games` message.
///
/// # Arguments
/// * `app` - application context.
pub async fn send_games(app: &App) {
    let games = tokio::task::spawn_blocking(steam_games).await.unwrap_or_default();
    info!(count = games.len(), "sending game library");
    match encode_games(&games) {
        Ok(msg) => app.send(&msg).await,
        Err(e) => warn!(error = %e, "cannot encode games"),
    }
}

/// Launches a game.  C#: `steam://rungameid/<id>` for Steam titles.
///
/// # Arguments
/// * `_app` - application context.
/// * `id` - game.
pub async fn launch(_app: &App, id: GameId) {
    if id.platform != GamePlatform::Steam {
        warn!(?id, "only Steam games can be launched by this port");
        return;
    }
    let url = format!("steam://rungameid/{}", id.id);
    let res = if cfg!(windows) {
        // The opener needs no standard handles.
        crate::async_command("cmd")
            .args(["/C", "start", "", &url])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    } else {
        tokio::process::Command::new("xdg-open").arg(&url).spawn()
    };
    match res {
        Ok(_) => info!(url, "game launched"),
        Err(e) => warn!(error = %e, "Unable to launch game."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_acf() {
        let acf = "\"AppState\"\n{\n\t\"appid\"\t\t\"620\"\n\t\"name\"\t\t\"Portal 2\"\n}\n";
        let p = vdf_pairs(acf);
        assert_eq!(p, vec![("appid".into(), "620".into()), ("name".into(), "Portal 2".into())]);
    }
}
