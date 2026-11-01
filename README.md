# desktop_streamer

A Rust port of the Virtual Desktop Streamer 1.34.18, reverse-engineered from the shipped binaries. It targets Windows and Linux on x86-64 and ARM64.
See [docs/REVERSE_ENGINEERING.md](docs/REVERSE_ENGINEERING.md) for the protocol, the component mapping and what is not ported.

## Workspace

| Crate | Contents |
|---|---|
| `vd-protocol` | Wire formats: framing, AES record layer, settings sync, structs, per-headset tables (no I/O) |
| `vd-net` | tokio networking: channels, relay/rendezvous, registry SOAP client, UPnP, WOL, routing |
| `vd-media` | FFmpeg-based capture and encode, file playback, thumbnails (version-isolated) |
| `vd-platform` | Input injection, audio devices, monitors, Wi-Fi tuning, priority, shared memory |
| `desktop_streamer` (root) | The application: settings window and tray icon, connection manager, desktop and video streaming, audio, input, hotkeys, games |

## Install

Download the packages from the [latest release](https://github.com/BrandonG2001/desktop_streamer/releases/latest).

* **Windows 10/11 (x64):** run `VirtualDesktopStreamerRust-<version>-setup-x64.exe`. It installs to
  Program Files and adds a Start-menu entry. It can also add a firewall rule, start with Windows,
  and install FFmpeg (and optionally yt-dlp) with winget. A portable `.zip` is also available.
* **Linux (x86-64):** extract `desktop_streamer-<version>-linux-x86_64.tar.gz` and run
  `./install.sh --uinput`. This installs to `~/.local` with a menu entry, and grants `/dev/uinput`
  access for input injection.

PC audio needs no driver: in "VR headset only" mode every application is recorded with Windows
process loopback, and all PC speakers are muted until the headset disconnects. When the official
[Virtual Desktop Streamer](https://www.vrdesktop.net) driver is present, "Use virtual audio
driver" routes audio through its virtual speakers instead, like the original.

Windows cannot create a microphone without a driver, so microphone passthrough uses one of two
drivers:

* the Virtual Desktop driver, if it is installed; or
* a virtual audio cable such as [VB-Audio Virtual Cable](https://vb-audio.com/Cable/) (free).
  * The headset microphone is played into "CABLE Input", and apps record from "CABLE Output".
  * "Auto-select microphone" makes "CABLE Output" the default microphone while you are connected.

OPTIONS → Microphone selects the target ("Automatic" picks the driver, then a known cable).

Quit the official streamer before starting this one, because they share ports and hotkeys.

## Building packages

* **Windows:** `packaging\windows\build-installer.ps1` builds the release executable, the Inno Setup
  installer and the portable zip into `dist\`. It requires Inno Setup 6
  (`winget install JRSoftware.InnoSetup`).
* **CI:** publishing a GitHub release runs `.github/workflows/release.yml`. The workflow builds
  the Windows and Linux packages and attaches them to the release.
* **Icons:** `packaging/make_icon.py` regenerates `packaging/icons/` from the same drawing the
  window uses.

## Usage

```sh
cargo build --release
desktop_streamer              # settings window + tray icon, like the original (add accounts in ACCOUNTS)
desktop_streamer --minimized  # start hidden in the notification area
desktop_streamer account add meta <your-meta-username>
desktop_streamer probe        # FFmpeg, encoder, monitors, network
desktop_streamer run          # headless: rendezvous through vrdesktop.net, no window
desktop_streamer run --direct # relay-less LAN mode for development (see examples/headset_sim.rs)
```

The settings window mirrors the original's tabs: ACCOUNTS, BINDINGS (system-wide shortcuts),
OPTIONS, ADVANCED, MEDIA and ABOUT, with "Paste URL" video downloads (needs `yt-dlp`) in the
footer. Closing the window hides it in the notification area; use the tray menu's "Exit" to quit.
Launching the program again brings the running window back. Only one instance runs at a time,
so quit the original Virtual Desktop Streamer first: both use the same ports and hotkeys.
Logs are written to `Logs/Streamer.log` next to the settings ("Show logs" in ABOUT).

Settings live in `StreamerSettings.json` in the user config directory (override with `VD_SETTINGS_DIR`), under `Port`:

* `CaptureSource`: `auto`, `x11`, `kmsgrab`, `ddagrab`, `gdigrab`, `test` or `custom`.
* `EncoderBackend`: `nvenc`, `amf`, `qsv`, `vaapi`, `v4l2m2m` or `software`.
* `RenderNode`: the DRM render node used by VA-API and QSV.

## Requirements

* **FFmpeg ≥ 5** on `PATH` (or `VD_FFMPEG_DIR`, or next to the executable). The Windows installer and zip bundle an LGPL FFmpeg 9 build.
* **In-process encoder** (`--features ffmpeg-lib`, used by the Windows release):
  * It captures and encodes inside the program through the FFmpeg libraries, giving instant key frames, live bitrate changes and measured latency.
  * It falls back to running the `ffmpeg` program per monitor, which is the default build. Set `VD_ENCODER=cli` to force that mode.
  * Building it needs the FFmpeg headers and libraries. On Windows, `. .\packaging\windowsfmpeg-env.ps1` fetches the pinned build into `third_partyfmpeg` and sets `FFMPEG_DIR`, `LIBCLANG_PATH` and the bindgen arguments.
* **Linux:** write access to `/dev/uinput`, plus `pactl`/`parec`/`pacat` and `iw`. The window uses
  OpenGL through X11 or Wayland. The tray icon needs a StatusNotifierItem host (KDE, or GNOME with
  the AppIndicator extension); without one, closing the window quits. Global hotkeys need X11 or
  XWayland.

## Tests

`cargo test --workspace` runs the unit tests.
`examples/headset_sim.rs` exercises a full session end to end against `run --direct`.
