# Changelog

## 1.5.0

* **Update checker.** At startup and from "Check for updates" in ABOUT, the streamer looks for a newer release on GitHub. An installed Windows copy asks, then downloads the setup, checks it, installs it silently and starts again. The portable zip and Linux open the release page instead.

## 1.4.0

* **Touch input on Windows.** With "Use touch input" on (the default), a laser click is a real Windows touch contact, as in the original. Clicking with both controllers makes two-finger gestures such as pinch-zoom. Right and middle clicks stay mouse clicks. Before, the option did nothing and every click was a mouse click.
* **"Ask for computer access" works.** When a headset signed in with an account that is not in ACCOUNTS searches the local network, the settings window comes up and asks "Allow Meta user '…' to access this computer?". Yes adds the account and registers again. "Don't ask again" turns the option off.
* **Live figures in ABOUT:** frame rate, bitrate, encode time and the encoder in use. Latency now shows the value the headset reports. It always showed "--" before.
* Quieter log: FFmpeg's console output is logged only when an encoder exits on its own, not when it is stopped for a restart. A misleading "audio streaming unavailable" warning was removed.
* Fixed two garbled paths in the README's build instructions.

## 1.3.0

* **In-process video encoder on Windows.** Desktop capture and encoding now run inside the streamer through the FFmpeg libraries, with the same `ddagrab` capture and NVENC/AMF/QSV settings as before, instead of one `ffmpeg.exe` per monitor:
  * Key-frame requests (recentring, packet loss) no longer restart the encoders. The next frame is a key frame, like the original's ForceKeyFrame.
  * Bitrate changes are applied to the running encoder (NVENC, QSV) without a hitch, so the stream follows the measured bandwidth again. AMF still restarts.
  * Encoder latency is measured instead of estimated (about 4 ms on NVENC).
  * If the in-process encoder cannot start, that monitor falls back to the `ffmpeg` program automatically. Set `VD_ENCODER=cli` to force the program.
* The Windows installer and zip now include an LGPL FFmpeg 9 build (DLLs, `ffmpeg.exe`, `ffprobe.exe` and licence notice). FFmpeg no longer needs to be installed separately, and the winget FFmpeg option was removed.

## 1.2.1

* Fixed: recentring the headset (holding the Meta button) reset the environment around the virtual screen to the black void.
  * The headset asks for a fresh key frame after recentring. To produce one, the port restarts its FFmpeg encoders, and each restart re-sent the stream descriptor, which the headset treats as a new stream.
  * Like the original, the descriptor is now sent only when the stream's parameters change or at the start of a session.

## 1.2.0

* **Multiple monitors.** Desktop streaming now shows all monitors at once by default, up to the headset's limit (3 on Quest 3) and with a hardware encoder, like the original. Before, only one monitor was shown.
  * The headset's add/remove-monitor buttons work, and the choice is remembered.
  * Adding stops at the physical monitors, because virtual monitors are not ported.
  * Settings saved by 1.0/1.1 are migrated from 1 monitor to "all".
  * Fixed: secondary monitors showed only green. Their frames are now always sent as Annex B, like the original. Before, they were converted to length-prefixed NAL units when the headset asked for that format for the main monitor.
  * Fixed: frozen screens and failed connections with several monitors.
    * Frames of all monitors are now sent in lockstep with the main monitor's timestamp, like the original's `DesktopStreamer.Update()`. The headset pairs them by timestamp, and independent timestamps stalled its video.
    * FFmpeg no longer emits duplicate timestamps (`setpts`).
    * Restarts wait for the old encoder processes to exit, which fixes "Device or resource busy" from screen capture.
    * If one monitor's encoder stops, all of them are restarted.
  * Fixed: the headset's remove-monitor button never appeared. `CanRemoveMonitor` was set as the session started and could miss the settings snapshot sent on connect, so it was never sent. Changes made during connection are now always sent.
  * New OPTIONS → Monitors setting (All / 1 / 2 / 3) to choose the number of monitors from the PC.
  * Fewer stream restarts: the bitrate ramp-up while the headset measures bandwidth no longer restarts every encoder each time. Big drops still apply immediately, and increases apply at most every 30 s.

## 1.1.0

* **Driver-free PC audio on Windows.** "VR headset only" no longer needs the Virtual Desktop audio driver:
  * Every application except the streamer is recorded with process loopback, whichever device it plays on.
  * Every playback device is muted with its volume at zero until the headset disconnects. Bluetooth headphones that ignore a plain mute are silenced too.
  * Mute and volume are restored afterwards, including after a crash.
  * With the driver installed and "Use virtual audio driver" on, the original virtual-speaker routing is still used.
  * On Windows 10 builds without process loopback, the default playback device is recorded instead.
* **Microphone passthrough without the Virtual Desktop driver** via a virtual audio cable (VB-Audio Virtual Cable, Hi-Fi Cable or VoiceMeeter):
  * The headset microphone is played into the cable's input, and "Auto-select microphone" makes the cable's output the default recording device.
  * The cable is never muted by "VR headset only".
  * A new OPTIONS → Microphone picker chooses between Automatic, the Virtual Desktop driver and any playback device, and ABOUT shows where the microphone goes.

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
