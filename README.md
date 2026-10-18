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
| `desktop_streamer` (root) | The daemon: connection manager, desktop and video streaming, audio, input, games |

## Usage

```sh
cargo build --release
desktop_streamer account add meta <your-meta-username>
desktop_streamer probe        # FFmpeg, encoder, monitors, network
desktop_streamer run          # rendezvous through vrdesktop.net, like the original
desktop_streamer run --direct # relay-less LAN mode for development (see examples/headset_sim.rs)
```

Settings live in `StreamerSettings.json` in the user config directory (override with `VD_SETTINGS_DIR`), under `Port`:

* `CaptureSource`: `auto`, `x11`, `kmsgrab`, `ddagrab`, `gdigrab`, `test` or `custom`.
* `EncoderBackend`: `nvenc`, `amf`, `qsv`, `vaapi`, `v4l2m2m` or `software`.
* `RenderNode`: the DRM render node used by VA-API and QSV.

## Requirements

* **FFmpeg ≥ 5** on `PATH` (or `VD_FFMPEG_DIR`). Use `--features ffmpeg-lib` to link libav directly.
* **Linux:** write access to `/dev/uinput`, plus `pactl`/`parec`/`pacat` and `iw`.

## Tests

`cargo test --workspace` runs the unit tests.
`examples/headset_sim.rs` exercises a full session end to end against `run --direct`.
