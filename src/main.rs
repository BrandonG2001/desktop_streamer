//! `desktop_streamer` — a cross-platform Rust port of the Virtual Desktop Streamer.
//!
//! The original is a WPF application (`VirtualDesktop.Streamer.exe`).  Started without a
//! sub-command this port opens the same settings window ([`gui`]) with a notification-area icon;
//! `run` keeps the headless daemon for servers and scripts.  See `docs/REVERSE_ENGINEERING.md`
//! for how each module maps onto the original program and which parts are not ported yet (VR
//! runtime drivers, Windows virtual devices).

mod app;
mod audio_task;
mod bindings;
mod connection;
mod desktop;
mod dispatch;
mod games;
mod gui;
mod hotkeys;
mod input_task;
mod pairing;
mod service;
mod settings;
mod state;
mod videos;
mod youtube;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tracing::info;
use vd_protocol::types::Platform;

use crate::settings::StreamerSettings;

/// Command-line interface.
#[derive(Debug, Parser)]
#[command(name = "desktop_streamer", version, about = "Rust port of the Virtual Desktop Streamer (Windows / Linux, x86-64 / ARM64)")]
struct Cli {
    /// Log filter (e.g. `info`, `debug`, `vd_net=trace`).
    #[arg(long, default_value = "info", global = true)]
    log: String,
    /// Start hidden in the notification area (used by the log-in autostart entry).
    #[arg(long)]
    minimized: bool,
    /// Command (defaults to the settings window).
    #[command(subcommand)]
    command: Option<Command>,
}

/// Sub-commands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Run the streamer without a window (headless).
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
    /// Register or remove the log-in autostart entry and update "Start with Windows"
    /// (used by the installer, which runs it as the installing user).
    #[command(hide = true)]
    Autostart {
        /// `on` or `off`.
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
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

/// Log file written next to the settings (C#: NLog `Logs\Streamer.log`, opened by "Show logs").
///
/// # Returns
/// The path.
pub fn log_path() -> std::path::PathBuf {
    StreamerSettings::directory().join("Logs").join("Streamer.log")
}

/// Initialises `tracing` with the given filter, logging to the terminal and optionally to
/// [`log_path`].
///
/// # Arguments
/// * `filter` - `EnvFilter` directive string.
/// * `to_file` - also write the log file (truncated at start-up).
fn init_logging(filter: &str, to_file: bool) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let env = tracing_subscriber::EnvFilter::try_new(filter).unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let file = to_file
        .then(|| {
            let path = log_path();
            std::fs::create_dir_all(path.parent()?).ok()?;
            std::fs::File::create(&path).ok()
        })
        .flatten()
        .map(|f| tracing_subscriber::fmt::layer().with_ansi(false).with_target(false).with_writer(std::sync::Mutex::new(f)));
    tracing_subscriber::registry().with(env).with(tracing_subscriber::fmt::layer().with_target(false)).with(file).init();
}

/// Creates a [`std::process::Command`] that never opens a console window on Windows (the
/// settings window runs without a console).
///
/// # Arguments
/// * `program` - executable to run.
///
/// # Returns
/// The command builder.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(vd_platform::system::CREATE_NO_WINDOW);
    }
    c
}

/// Async variant of [`command`].
///
/// # Arguments
/// * `program` - executable to run.
///
/// # Returns
/// The command builder.
pub fn async_command(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut c = tokio::process::Command::new(program);
    #[cfg(windows)]
    c.creation_flags(vd_platform::system::CREATE_NO_WINDOW);
    c
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

/// Runs the streamer without a window until Ctrl+C or a headset `Exit` message.
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
    let app = service::start(settings, direct);
    let mut shutdown = app.shutdown.subscribe();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("Ctrl+C received"),
        _ = shutdown.changed() => {}
    }
    service::stop(&app).await;
    Ok(())
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

/// Handles the `account` sub-command.
///
/// # Arguments
/// * `action` - what to do.
///
/// # Errors
/// Unknown platform names or settings that cannot be saved.
fn account(action: AccountAction) -> Result<()> {
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

/// Program entry point.
///
/// # Errors
/// Start-up failures are printed by `anyhow`.
fn main() -> Result<()> {
    let cli = Cli::parse();
    let gui = cli.command.is_none();
    // A second launch only asks the running window to show itself (and must not truncate its log).
    let instance = if gui {
        match gui::claim_instance() {
            Some(l) => Some(l),
            None => return Ok(()),
        }
    } else {
        None
    };
    init_logging(&cli.log, gui || matches!(cli.command, Some(Command::Run { .. })));
    // Physical-pixel monitor rectangles, matching the headset's pointer coordinates.
    vd_platform::display::enable_dpi_awareness();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let Some(command) = cli.command else {
        // The window runs on the main thread (required by winit); the streamer on the runtime.
        return gui::run(&rt, cli.minimized, instance.expect("claimed above"));
    };
    rt.block_on(async move {
        match command {
            Command::Run { direct } => run(direct).await,
            Command::Probe => probe().await,
            Command::Account { action } => account(action),
            Command::Config => {
                println!("{}", StreamerSettings::path().display());
                println!("{}", serde_json::to_string_pretty(&StreamerSettings::load())?);
                Ok(())
            }
            Command::Autostart { state } => {
                let enabled = state == "on";
                vd_platform::system::set_autostart(enabled, &[])?;
                let mut s = StreamerSettings::load();
                s.start_with_windows = enabled;
                s.save()?;
                Ok(())
            }
        }
    })
}
