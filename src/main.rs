//! `desktop_streamer` — a cross-platform Rust port of the Virtual Desktop Streamer.
//!
//! The original is a WPF application (`VirtualDesktop.Streamer.exe`); this port is a headless
//! daemon with a small command-line interface.  See `docs/REVERSE_ENGINEERING.md` for how each
//! module maps onto the original program and which parts are not ported yet (VR runtime
//! drivers, Windows virtual devices).

mod app;
mod audio_task;
mod connection;
mod desktop;
mod dispatch;
mod games;
mod input_task;
mod settings;
mod state;
mod videos;

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tracing::{info, warn};
use vd_protocol::types::Platform;

use crate::app::App;
use crate::settings::StreamerSettings;

/// Command-line interface.
#[derive(Debug, Parser)]
#[command(name = "desktop_streamer", version, about = "Rust port of the Virtual Desktop Streamer (Windows / Linux, x86-64 / ARM64)")]
struct Cli {
    /// Log filter (e.g. `info`, `debug`, `vd_net=trace`).
    #[arg(long, default_value = "info", global = true)]
    log: String,
    /// Command (defaults to `run`).
    #[command(subcommand)]
    command: Option<Command>,
}

/// Sub-commands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Run the streamer.
    Run {
        /// Skip the vrdesktop.net rendezvous and listen on TCP 38810–38840 directly (development
        /// mode for headset emulators; no encryption).
        #[arg(long)]
        direct: bool,
    },
    /// Print FFmpeg, encoder, monitor, network and routing diagnostics.
    Probe,
    /// Manage the accounts allowed to connect (`StreamerSettings.Accounts`).
    Account {
        /// Account action.
        #[command(subcommand)]
        action: AccountAction,
    },
    /// Show the settings file location and contents.
    Config,
}

/// Account actions.
#[derive(Debug, Subcommand)]
enum AccountAction {
    /// Add an account (`meta`, `pico`, `viveport`, `google`, `apple`, `steam`, `pfd`).
    Add {
        /// Store platform.
        platform: String,
        /// User name on that platform.
        user: String,
    },
    /// Remove an account.
    Remove {
        /// Store platform.
        platform: String,
        /// User name.
        user: String,
    },
    /// List accounts.
    List,
}

/// Initialises `tracing` with the given filter.
///
/// # Arguments
/// * `filter` - `EnvFilter` directive string.
fn init_logging(filter: &str) {
    let env = tracing_subscriber::EnvFilter::try_new(filter).unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(env).with_target(false).init();
}

/// Parses a platform name.
///
/// # Arguments
/// * `s` - platform name or alias.
///
/// # Returns
/// The platform.
///
/// # Errors
/// When the name is unknown.
fn parse_platform(s: &str) -> Result<Platform> {
    Platform::parse(s).with_context(|| format!("unknown platform `{s}` (use meta, pico, viveport, google, apple, steam or pfd)"))
}

/// Runs the streamer until Ctrl+C or a headset `Exit` message.
///
/// # Arguments
/// * `direct` - direct (relay-less) mode.
///
/// # Errors
/// Fatal start-up errors.
async fn run(direct: bool) -> Result<()> {
    let settings = StreamerSettings::load();
    if !direct && settings.access_tokens().is_empty() {
        bail!("no account configured: run `desktop_streamer account add meta <your Meta username>` first (or use `run --direct`)");
    }
    let media = app::detect_media(&settings).await;
    let (app, rx) = App::new(settings, media, direct);
    {
        let fb = (1920, 1080);
        let monitors = desktop::list_monitors(fb);
        info!(count = monitors.len(), "monitors detected");
        app.state.dynamic.send_modify(|d| d.monitors = monitors);
    }

    tokio::spawn(dispatch::run(Arc::clone(&app), rx.inbound.messages));
    tokio::spawn(dispatch::sync_streamer_settings(Arc::clone(&app)));
    tokio::spawn(desktop::run(Arc::clone(&app)));
    tokio::spawn(audio_task::run(Arc::clone(&app), rx.inbound.mic));
    tokio::spawn(input_task::run(Arc::clone(&app), rx.inbound.states));
    tokio::spawn(videos::run(Arc::clone(&app), rx.video));
    tokio::spawn(session_side_effects(Arc::clone(&app)));
    // Gaze samples drive the dynamic foveation map of the VR encoder, which is not ported; drain them.
    tokio::spawn(async move {
        let mut gaze = rx.inbound.gaze;
        while gaze.recv().await.is_some() {}
    });
    let conn = tokio::spawn(connection::run(Arc::clone(&app), rx.peers));

    let mut shutdown = app.shutdown.subscribe();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("Ctrl+C received"),
        _ = shutdown.changed() => {}
    }
    app.request_shutdown();
    app.channels.messaging.reset().await;
    conn.abort();
    app.save_settings();
    info!("streamer stopped");
    Ok(())
}

/// Starts/stops session-scoped helpers: Wi-Fi optimisation (`StartWLANOptimizerThread`) and
/// priority boost (`IServiceManager.UseRealtimePriority`).
///
/// # Arguments
/// * `app` - application context.
async fn session_side_effects(app: Arc<App>) {
    let mut rx = app.state.dynamic.subscribe();
    let mut optimizer: Option<vd_platform::wlan::WlanOptimizer> = None;
    loop {
        let connected = rx.borrow().is_connected;
        if connected && optimizer.is_none() {
            optimizer = Some(vd_platform::wlan::WlanOptimizer::start());
            if app.settings.lock().expect("poisoned").boost_priority {
                if let Err(e) = vd_platform::process::set_high_priority(true) {
                    warn!(error = %e, "cannot raise process priority");
                }
            }
        } else if !connected {
            if let Some(o) = optimizer.take() {
                tokio::task::spawn_blocking(move || o.stop());
                let _ = vd_platform::process::set_high_priority(false);
            }
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// Prints diagnostics.
///
/// # Errors
/// Never fails; missing components are reported in the output.
async fn probe() -> Result<()> {
    let settings = StreamerSettings::load();
    match vd_media::version::Ffmpeg::discover().await {
        Ok(ff) => {
            println!("FFmpeg {}.{} at {}", ff.version.major, ff.version.minor, ff.ffmpeg.display());
            println!("  capture inputs: {:?}", ff.inputs);
        }
        Err(e) => println!("FFmpeg: not available ({e})"),
    }
    match app::detect_media(&settings).await {
        Some(m) => println!("Encoder: {:?} {:?}", m.backend, m.caps),
        None => println!("Encoder: none"),
    }
    match vd_platform::display::monitors() {
        Ok(ms) => {
            for m in ms {
                println!("Monitor {} '{}': {}x{} @{} Hz at ({}, {}){}", m.index, m.id, m.width, m.height, m.refresh_hz, m.x, m.y, if m.primary { " primary" } else { "" });
            }
        }
        Err(e) => println!("Monitors: {e}"),
    }
    for a in vd_net::adapters::private_adapters(true) {
        println!("Adapter {}: {} mac={} wireless={} gigabit={}", a.name, a.ip, a.mac, a.is_wireless, a.is_gigabit);
    }
    println!("Routing: {:?}", vd_net::routing::detect_routing_status().await);
    println!("WLAN optimisation (dry run): {:?}", vd_platform::wlan::optimize_wlan(false));
    Ok(())
}

/// Program entry point.
///
/// # Errors
/// Start-up failures are printed by `anyhow`.
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(&cli.log);
    match cli.command.unwrap_or(Command::Run { direct: false }) {
        Command::Run { direct } => run(direct).await,
        Command::Probe => probe().await,
        Command::Account { action } => {
            let mut s = StreamerSettings::load();
            match action {
                AccountAction::Add { platform, user } => {
                    s.add_account(parse_platform(&platform)?, &user);
                    s.save()?;
                    println!("added {platform} account {user}");
                }
                AccountAction::Remove { platform, user } => {
                    let removed = s.remove_account(parse_platform(&platform)?, &user);
                    s.save()?;
                    println!("{}", if removed { "removed" } else { "not found" });
                }
                AccountAction::List => {
                    for (p, users) in &s.accounts {
                        for u in users {
                            println!("{p}: {u}");
                        }
                    }
                }
            }
            Ok(())
        }
        Command::Config => {
            println!("{}", StreamerSettings::path().display());
            println!("{}", serde_json::to_string_pretty(&StreamerSettings::load())?);
            Ok(())
        }
    }
}
