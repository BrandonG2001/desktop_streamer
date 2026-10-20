# Changelog

## 1.0.0

This is the first release of the Rust port of Virtual Desktop Streamer 1.34.18. It is wire-compatible with the Virtual Desktop app on the headset.

### Features

* **Connections:**
  * Connects through vrdesktop.net registration and the relay, with hole punching and encrypted sessions.
  * Has a relay-less LAN mode for development (`run --direct`).
* **Desktop streaming:**
  * Encodes with FFmpeg: H.264, HEVC, HEVC 10-bit and AV1, using NVENC, AMF, QSV, VA-API, MediaFoundation or software encoders.
  * Supports multiple monitors, monitor switching and adaptive bitrate.
* **Video library playback:**
  * Plays videos passed through from the PC library and the configured video folders.
  * Downloads YouTube videos with "Paste URL" (needs yt-dlp).
* **Audio:**
  * Streams PC audio to the headset.
  * "VR headset only" makes the Virtual Desktop speakers the default device while a session runs, so the PC stays silent.
  * Passes the headset microphone through to the virtual microphone, with optional auto-select as the default recording device.
  * Supports VoiceMeeter mode.
* **Input:**
  * Keyboard, mouse mode (buttons, motion and wheel) and laser pointers.
  * Gamepad emulation on Linux.
  * DPI-aware pointer mapping on Windows.
* **Settings window:**
  * Has the original's ACCOUNTS, BINDINGS, OPTIONS, ADVANCED, MEDIA and ABOUT tabs.
  * Includes a notification-area icon and system-wide hotkeys.
  * Runs as a single instance and can start with Windows or start minimized.
* **Platforms:**
  * Windows 10/11 x64 ships with an Inno Setup installer and a portable zip.
  * Linux x86-64 ships as a tarball with `install.sh` (uinput, PulseAudio/PipeWire, X11 or kmsgrab capture).

### Known limitations

* VR game streaming (SteamVR/OpenXR/Oculus runtimes) and virtual monitors are not ported.
* On Windows, PC-audio routing and the microphone need the audio driver from the official Virtual Desktop Streamer.
* Windows gamepad emulation needs the original ViGEm/vdvge driver and is not provided.
* Only one streamer can run at a time, so quit the official Virtual Desktop Streamer first.
