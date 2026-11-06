# Virtual Desktop Streamer 1.34.18 — reverse-engineering notes

Re-checked against 1.34.25 in November 2026; see §6e for what changed.

Source of truth for the Rust port. Every claim below says how it was established. **Observed**
means read directly from decompiled code, IL or Ghidra pseudocode. **Inferred** means deduced
from usage and still needs confirming against live traffic.

## 1. What is installed

| Component | Kind | Role | Port status |
|---|---|---|---|
| `VirtualDesktop.Streamer.exe` | .NET Fx 4.7.1, SmartAssembly-obfuscated, ILMerged (SteamKit2, SharpDX, FFmpeg.AutoGen, NLog, protobuf-net, CSCore, Newtonsoft, Xenko…) | UI, networking, encoding, VR orchestration | **Ported** (headless) except the VR path and the WPF UI |
| `libVirtualDesktopNet.dll` | C++ | Wi-Fi tuning (`OptimizeWLAN`, optimizer thread) | **Ported exactly** (`vd-platform::wlan`) |
| `libVirtualDesktopAMF.dll` | C++ | AMD AMF encoder wrapper | Replaced by FFmpeg `*_amf` / `*_vaapi` |
| `nvEncodeAPI64.dll` (driver) | — | NVENC, called directly from C# (`_008B._0003`) | Replaced by FFmpeg `*_nvenc` |
| Media Foundation MFTs | — | Intel/other encoders (`_001B_0002._0002`) | Replaced by FFmpeg `*_qsv` / `*_mf` / `*_vaapi` |
| `avcodec-61.dll` … (FFmpeg 7) | C | File playback, thumbnails, encoder probing | Replaced by the `ffmpeg`/`ffprobe` CLI, with optional libav through `ffmpeg-next` |
| `VirtualDesktop.Shell.Native.dll` | C++ | GPU driver version, shell helpers | Not needed |
| `VirtualDesktop.Injector{32,64}.dll`, `VirtualDesktop.LibOVRRT{32,64}_1.dll`, `openxr-oculus-compatibility.dll` | C++ (MinHook) | Oculus-runtime emulation and hooking for VR games | **Not ported** (Windows-only by nature) |
| `OpenVRDriver/…/driver_VirtualDesktop.dll` | C++ | SteamVR driver | **Not ported**; IPC contract documented in §6 |
| `OpenXR/virtualdesktop-openxr.dll` (VDXR) | C++ | OpenXR runtime | **Not ported** (VDXR is open source: mbucchia/VirtualDesktop-OpenXR) |
| `AudioDriver/vdvad.sys`, `GamepadDriver/vdvge.sys`, `MonitorDriver/vdvmd.dll` | Kernel / IddCx drivers | Virtual audio device, virtual gamepad, virtual monitors | Windows: the installed `vdvad.sys` is reused for the microphone (`IKsControl`). Linux: PulseAudio/PipeWire modules and `/dev/uinput`. Virtual monitors use the Parsec Virtual Display Driver instead of `vdvmd.dll` (§6d). |
| `Updater.exe`, `VirtualDesktop.Setup.exe`, `VirtualDesktop.Service.msi` | Installer / updater | — | Not ported |
| `C:\Program Files\Virtual Desktop\VirtualDesktop.Service.exe` / `Server.exe` | .NET (outside the analysed folder) | SYSTEM service: WCF `IInputInjection`, `IMonitorDuplication`, `IAudioService`, `IVirtualMonitor`, `IServiceManager` | Functions performed in-process (`vd-platform`) |

## 2. Method

1. REA `inspect_managed_artifact`: identity and classification. The metadata exceeded REA's 64 MB member budget, so the rest of the managed analysis used `ilspycmd` 8.2.
2. Full C# project decompile (11,537 files), then a script splitting the obfuscated merged namespaces into one file per type (552 types).
3. A de-noising pass removed SmartAssembly's exception-reporting wrappers and opaque predicates.
4. `ilspycmd -il` resolved the identically named overloads (`_0001()`) to concrete types, which yields the exact field types of each message (§4).
5. A struct-layout resolver turned sequential and explicit C# layouts into byte-accurate `#[repr(C)]` Rust structs, checked with compile-time `offset_of!` assertions.
6. Ghidra (via REA) decompiled the native DLLs (`libVirtualDesktopNet.dll`).

## 3. Transport

* Four TCP channels per session. Each can be wrapped in an AES record layer:

  | Channel | LAN port | Relay port |
  |---|---|---|
  | messaging | 38810 | 38811 + rotation |
  | data | 38820 | 38821 + rotation |
  | video | 38830 | 38831 + rotation |
  | audio | 38840 | 38841 + rotation |

  `rotation = (rotation + 1) % 6` on every registration (observed).
* UDP 38860: two empty broadcasts after resume, and the source port for Wake-on-LAN packets sent to ports 7 and 9 (observed).
* **Rendezvous** (observed):
  1. `RegisterComputer5` on `https://{america|europe}.vrdesktop.net/ComputerRegistry.svc/IComputerRegistry`, using WCF `BasicHttpBinding` over TLS. The region is picked from the UTC offset: −12 h to −2 h means America, anything else Europe.
  2. The service returns a `RegistrationToken` (`ConnectionID`, AES key and IV).
  3. Connect to the relay (20.225.41.170 or 40.89.161.236) and send the raw `ConnectionID`.
  4. Switch to the AES layer.
  5. The relay forwards `PeerInfo`. The PC then listens on its LAN ports, or hole-punches from the relay socket's local port when remote connections are allowed.
* **SOAP envelope details are inferred:** WCF defaults `tempuri.org`, DataContract alphabetical member order, `System.Version` as `_Build/_Major/_Minor/_Revision`.
* **AES record layer** (observed, `_0096_0002._0001`):
  * Each `Write` produces `AES-CBC(varint7(len) ‖ data ‖ random pad to 16)`.
  * `PaddingMode.None`.
  * The CBC chain continues across records in each direction.

## 4. Control messages (port 38810)

* Frame: `u8 type, i32 BE length, payload`.
* Payload encoding:
  * integers are big-endian;
  * strings are an `i16 BE` byte count followed by UTF-8;
  * a `Guid` is its 16 raw bytes;
  * an `IPAddress` is a `u8` length (4 or 16) followed by the address. IPv4 octets travel **reversed**, because the reader passes a big-endian `uint` to `IPAddress(long)`.
* All `MessageType` values and the decoded payloads are in `crates/vd-protocol/src/message.rs` and `messages.rs`. Field types were observed in IL.

Settings synchronisation (observed):

* **Reload:** `*SettingsReload` carries compact JSON with `DefaultValueHandling.IgnoreAndPopulate`.
* **Property change:** `*SettingsPropertyChanged` carries `{"Name": value}` with defaults included.
* **Enums:** sent as integers.
* **Ownership:** `SharedStreamerSettings` is PC-owned; `SharedUserSettings` and `SharedMobileSettings` are headset-owned.

## 5. Media channels

| Channel | Direction | Format (observed) |
|---|---|---|
| data 38820 | PC→HS | First `i32 BE n` + n random bytes (bandwidth probe; 5,000,000 when `AutoAdjustBitrate`). Then file transfers: tag 0 `i64 BE` + raw bytes; tag 2/3 `i32 BE` + bytes; tag 4 marker. |
| data 38820 | HS→PC | `i32 LE len, u64 LE XXH3-64, LZ4 block` → `HeadsetState`. 4936 bytes by our layout; the decoder warns if the decompressed size differs. |
| video 38830 | PC→HS | tag 0 `FrameHeader`(160) + bitstream; tag 1 `{i64 ts, i32 monitor, i32 size}` + bitstream; tag 2 `StreamDescriptor`(88); tag 3 `i32 LE` + UTF-8 text |
| video 38830 | HS→PC | `i32 LE len` + 2×`Vector2` gaze |
| audio 38840 | both | `i32 LE len` + s16le PCM. PC→HS: 48 kHz stereo, 1152-byte (6 ms) packets, gain 0.9. HS→PC mic: 48 kHz mono, 20 ms reads, gain `lerp(0.1, 2, MicVolume)`. |

Bitstreams are Annex-B. When `UseAnnexB == false`, start codes are rewritten as 4-byte BE lengths.

### Encoder behaviour (NVENC class, observed)

* **Rate control and GOP:** infinite GOP, IDR forced on demand, P-frames only, CBR, one-frame VBV.
* **Quality options:** optional spatial AQ, 4:4:4, multipass. 10-bit for HEVC10 and AV1-10.
* **Pipeline:** a 4-surface ring, with output collected by a separate "Frame Encoding Thread".
* **Codec fallback:** `vd-media::codec::resolve_codec`.
* **Foveated QP map:** `vd-protocol::foveation`.

### Per-headset tables (observed; control flow de-flattened by hand)

`vd-protocol::hmd` holds:

* optimal and maximum desktop resolution;
* maximum monitor count (1, 2 or 3);
* desktop bitrate `lerp(2|4 Mbps, 40|120 Mbps, limit)`;
* VR bitrate ceilings per codec;
* VR render resolutions (partial).

### Audio (observed)

* **PC → headset:** WASAPI loopback, converted to 48 kHz stereo s16 with gain 0.9 and sent in 1152-byte packets. Which endpoint is recorded depends on the mode:
  * **"VR headset only" with "Use virtual audio driver"** (the default): `IAudioService.SetVirtualAudioDeviceRender(pid, setAsDefault: !VoiceMeeterMode)`. The service makes **Speakers (Virtual Desktop Audio)** visible and the default device for every role, and the streamer records it. The PC therefore outputs nothing, even on Bluetooth devices. `RestoreDefaultAudioDeviceRender` restores the previous default when the session ends.
    * While no session runs, the endpoint is hidden: its `DeviceState` carries the `0x10000000` flag set by `IPolicyConfig::SetEndpointVisibility(false)`. Hidden endpoints are not returned by `EnumAudioEndpoints`, so the port finds the endpoint under `HKLM\…\MMDevices\Audio\Render` by its interface name (`{b3f8fa53-0004-438e-9003-51a46e139bfc},6` = "Virtual Desktop Audio").
    * The port switches the default with `IPolicyConfig` (`CLSID_PolicyConfigClient`, vtable slots 13–14).
    * It records the previous defaults in a temp file so that a crash is undone at the next start. Verified on Windows: a played sound is captured from the virtual device, and the previous default comes back afterwards.
  * **VoiceMeeter mode:** the virtual device is recorded but not made the default.
  * **Headset only without the virtual driver:** the original mutes the default endpoint (`IsMuted = true`), and loopback still captures the mix. That leaves apps on other devices audible, and some Bluetooth headphones ignore the mute.
    * The port therefore uses a driver-free route here, and also when "Use virtual audio driver" is off.
    * It records every process except itself with `ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK)` and `PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE`, as 48 kHz s16 with event callback. This needs Windows 10 build 20348 or later; older builds fall back to default-endpoint loopback.
    * It mutes **every** active render endpoint and sets its volume to 0, saving the previous values to a temp file for crash recovery.
    * Verified on Windows: a played sound is captured at full level while all four endpoints read `mute=True volume=0.00`, and every value comes back exactly, also after the process is killed.
  * **"VR headset & computer":** the default endpoint is recorded unchanged.
* **Auto-select microphone:** `SetVirtualAudioDeviceCapture` makes **Microphone (Virtual Desktop Audio)** the default recording device while microphone passthrough runs, and the port does the same.
* **Headset → PC microphone without the driver (port only):** Windows has no user-mode way to create a capture endpoint, so the port plays the microphone into the playback side of a *virtual audio cable*.
  * It renders 48 kHz mono s16 with `AUTOCONVERTPCM`, using the same 80 ms backlog limit as the driver path.
  * Known cables are matched by device name: "CABLE Input"/"CABLE Output", Hi-Fi Cable, CABLE-A/B, and VoiceMeeter (Aux/VAIO3) Input/Output.
  * The cable's recording side becomes the default microphone when "Auto-select microphone" is on.
  * The cable is excluded from the driver-free speaker silencing.
  * `Port.MicrophoneDevice` selects `auto` (driver, then cable), `vdvad`, or a playback endpoint id.
  * Verified on Windows by pointing it at a real playback device: its peak meter reads 0.244 (= the probe's 8000/32767 tone) while every other device is silenced.
* **Headset → PC microphone:** the original binds the DirectShow `AM_KSCATEGORY_CAPTURE` filter named "VDVAD Wave" to `IKsControl` and calls `KsProperty` with Set `{15E0F7CE-5521-4214-A1D1-17DEDD428BE9}`, Id 0, Flags `KSPROPERTY_TYPE_SET`, data = 48 kHz mono s16 PCM (≤ 1920 bytes). The driver returns `ERROR_INVALID_FUNCTION` while nothing records from "Microphone (Virtual Desktop Audio)"; the original ignores the result. Verified on Windows by recording the endpoint while pushing a 440 Hz tone.

## 6. VR driver IPC (documented, not ported)

The drivers and the streamer share named objects. Struct layouts are generated in the scratch notes; sizes are observed.

* **`VirtualDesktop.Oculus.DriverData`:** `_001E._0002`, 464 B.
* **`VirtualDesktop.BodyState`:** `_008A._0002`, 9792 B.
* **`VirtualDesktop.Oculus.BoundaryData`:** no fixed layout recorded.
* **Events:** `…OperationStartEvent`, `…OperationEndEvent`, `…PoseUpdatedEvent`, `…PresentEvent`, `…RenderedEvent` (manual reset), `VirtualDesktop.BodyStateEvent{,2}`.
* **Input injection:** `VirtualDesktop.InputInjection`, 108 B.

`vd-platform::shm` already implements these names on Windows and on POSIX. A Linux SteamVR driver (`driver_virtualdesktop.so`) would be the next step towards Linux VR.

### Input injection semantics

From `VirtualDesktop.Server.exe` `_0008._0002`. Its injection loop runs every 1 ms over the shared `InputState`, which the streamer overwrites with every headset packet while the desktop is streamed.

* **Laser pointers** (`_0003_0002._0003`, two of them):
  * `b0 || b1`: the pointer is in range, and a touch contact follows `(x, y)`. The coordinates are physical virtual-desktop pixels, divided only by the DPI-virtualisation factor; no monitor origin is added.
  * `b2 || b3 || b4`: the contact is down. `b3` makes it a secondary press and `b4` a tertiary one; otherwise it is primary.
  * With `UseTouchInput` (and `InitializeTouchInjection(2, TOUCH_FEEDBACK_DEFAULT)` succeeding), a primary contact is a real touch contact, `PT_TOUCH` with id 0 or 1 and an 8×8 `rcContact`. Secondary and tertiary presses stay mouse right/middle buttons. The pointer flags are `_0002._0007`: INRANGE 2, INCONTACT 4, DOWN 0x10000, UPDATE 0x20000, UP 0x40000. The frame sequences are:
    * press: `INRANGE|UPDATE`, then `INRANGE|INCONTACT|DOWN`, then `INRANGE|INCONTACT|UPDATE`;
    * hold or move: `INRANGE|INCONTACT|UPDATE`, injected every millisecond (the port re-injects every 50 ms);
    * release: `INRANGE|UP`, then `INRANGE|UPDATE`, then `UPDATE`.
  * Every frame carries both active contacts. The cursor follows the owning pointer with `MOUSEEVENTF_MOVE|ABSOLUTE`, except while that pointer is a touch contact.
  * Without touch, the contacts are emulated with the mouse (left/right/middle). The port does the same on Linux.
  * Not ported: while a pointer is in range, the original also widens the double-click and drag rectangles to 32 px and the double-click time to 1200 ms (`SystemParametersInfo` 29/30/32/76/77). It resets them to 4 px and 500 ms afterwards, not to the user's values.
* **Mouse mode** (`_0003_0002._0002`, bools `b1`–`b6`, floats `x`/`y`, sbytes `w1`/`w2`):
  * `b1` turns mouse mode on. Presses, motion and the wheel are ignored without it, but releases are still delivered.
  * `b2`–`b6` are LEFT, MIDDLE, RIGHT, XBUTTON1 and XBUTTON2, edge-triggered.
  * `x`/`y` persist between packets and the server moves the cursor by `round(x), round(y)` every millisecond, which makes them a velocity in px/ms. When the trailing bool of the controller-button struct (`_0003_0002._0001`, offset 0x62 of `InputState`) is set, they are instead an absolute position, scaled by 65535 / primary-screen size.
  * `w1` is HWHEEL and `w2` is WHEEL, each value × 8, consumed once per packet. While a laser pointer is in range they become `InjectScrollInput` instead.
* **Gamepad:** the `XINPUT_GAMEPAD`-shaped block goes straight to ViGEm. Outside desktop streaming the streamer copies only the gamepad and controller blocks.

## 6b. Settings window (`MainWindow.xaml`)

The WPF layout came from the compiled BAML resources (`mainwindow.baml`, `addaccountwindow.baml`,
`themes/metrodark/*.baml`). They were decoded with a small BAML record reader, which resolves
type, attribute and string tables and prints element trees with bindings. Behaviour came from
the decompiled `MainWindow`, `BindingSettings`, `YouTubeHelper` and converter classes.

* **Window:** borderless, 700×516 (also the minimum size). The header has the logo and
  minimise/close buttons and doubles as the drag area. Tabs sit on the left, each 160×48 with
  16 pt text. Colours are `#101010` background, `#151515` panels, `#F0F0F0` text, `#808080`
  inactive tabs and `#00AADE` accent.
* **Tabs and bindings:**
  * **ACCOUNTS:** binds `StreamerSettings.Accounts`. Edit mode is `DynamicSettings.IsEditingAccounts`, which turns on automatically when no account is valid. "Add account ▾" lists Meta, Pico, Google, Play for Dream and Viveport (`PlatformProvider`). The limit is 5 accounts per platform, and duplicates are rejected. "Save" restarts the connection manager.
  * **BINDINGS:** `BindingSettings.json` maps `BindingAction` to `{KeyCode, ModifierKeys}`. The defaults are Win+Shift+A/D/C/Z/X/H/K/O/G. Shortcuts are registered with `RegisterHotKey`, and every action except SwitchMonitor and ToggleVRMode sends a message of the same name.
  * **OPTIONS** and **ADVANCED:** the fields, tooltips, slider ranges (`0.4–1.0`, `0.5–2.0`, `0.15–0.35`, `0.05–1.0`, shown as `0%`) and reset defaults are listed in `src/gui/tabs.rs`. The confirmation prompts guard turning off "Automatically adjust bitrate" and turning on "Boost game priority".
  * **MEDIA:** shows the screenshot and video roots. Extra video folders are symbolic links inside the video root, which the original creates via `IServiceManager`. The port uses a directory junction on Windows and a symlink on Linux.
  * **ABOUT:** shows labels formatted by `LatencyConverter`, `PCEthernetConverter`, `RoutingStatusConverter`, `BooleanToYesConverter` and `BooleanToEnabledConverter`.
* **Tray:** the menu is "Settings...", "Launch Game...", "Disconnect" and "Exit". The icon and
  tooltip follow the connection state. The close button hides the window to the tray, and the
  minimise button minimises it. The port generates its own icons and omits "Launch Game...",
  which needs the VR runtime.
* **Paste URL:** runs `yt-dlp -f bestvideo+bestaudio[ext=m4a]/mp4 -o <videos>\%(title)s.%(ext)s
  --compat-opt prefer-vp9-sort --restrict-filenames --no-check-certificate`. It parses
  `[download] N% of` lines into `SharedStreamerSettings.DownloadState` and `DownloadProgress`.
* **ABOUT latency:** `DynamicSettings.Latency = (int)(HeadsetState._0001 * 1000f)`, the first float of every headset state (observed in IL). The port also shows frame rate, bitrate and encode time.
* **Update checker** (`UpdateHelper`): at startup and from ABOUT's "Check for updates", the original runs `Updater.exe /justcheck` (Advanced Installer, `-critical` at startup). If that reports an update, it starts `Updater.exe /silentall -nofreqcheck -reducedgui` and exits. If `Updater.exe` is missing, it offers to open www.vrdesktop.net. The same helper fetches `download.vrdesktop.net/files/version.txt` and flags a hosts file that redirects `vrdesktop.net`.
  * The port reads the latest GitHub release of this repository instead. On an installed Windows copy (`unins000.exe` next to the executable), it asks, then downloads `…-setup-x64.exe` and checks its size and the SHA-256 that GitHub reports. It then runs the setup with `/SILENT /SUPPRESSMSGBOXES /NORESTART /CLOSEAPPLICATIONS /RESTARTAPP=1` and exits. `RESTARTAPP` makes the setup start the new version.
  * Portable and Linux copies offer the release page. The startup check asks instead of installing silently, and waits until no headset is connected.
* **Not reproduced:** "Check for interfering apps" (`AppsCheckerWindow`), the hosts-file piracy
  warning and localisation (`CultureResources`).

## 6c. LAN discovery and pairing prompts (`_0084_0002._0002`)

Observed in 1.34.25, the version installed when this was ported. The service runs from the start of registration until a headset connects.

* **Socket:** UDP `[::]:38850` in dual mode, joined to `ff02::1` on every interface. Headsets send to `255.255.255.255:38850` or `ff02::1`.
* **Encryption:** the AES record layer of §3, with a key and IV built into the streamer (`vd-net::pairing`).
* **Request** (more than 243 bytes). The plaintext is:
  1. 243 bytes of the headset's 1024-bit RSA public key as `<RSAKeyValue>` XML;
  2. then either:
     * `0x00`, platform, account (UTF-8). This is unsigned and accepted for `OculusQuest` only; or
     * `0x01`, a 256-byte signature, 2 bytes, platform, account. The signature covers `platform ‖ account` and is checked with `RSA.VerifyData` against Virtual Desktop's 2048-bit key. The hash and padding are bound at run time by the obfuscator, so the port accepts SHA-256, SHA-1 and SHA-512 with PKCS#1 v1.5, and SHA-256 with PSS.
* **Known account:** the reply is `RSA-OAEP(headset key, AES key ‖ IV)` (128 bytes), followed by the `DataContractSerializer` XML of `Computer`, encrypted under that AES key. `Computer` holds `ID`, `Name`, `Description`, `OS`, `StreamerVersion`, `PrivateAdapters`, the remote and encryption flags, a random `ConnectionID` and the session `Key`/`IV`. Unsigned requesters are remembered: OculusStreamer later disconnects them unless the HMD type allows it.
* **Connect request** (17 bytes): `ConnectionID ‖ platform`. The streamer then opens a LAN session to the sender with the session key, without the relay.
* **Unknown account:** the first request per headset key raises the prompt "Allow {Meta|platform} user '{account}' to access this computer?" when `ShowPairingRequests` is on. Its "don't ask again" box sets `ShowPairingRequests = false`. On Yes, the account is added if `CanAddAccountID` allows it (not listed, fewer than 5 per platform), the settings are saved, and registration restarts unless a headset is connected.
* **Port:** only the unknown-account path (the prompt) is implemented. Headsets with a known account still find the PC through the registry, so the PC does not reply to LAN discovery.

## 6d. Virtual monitors (`IVirtualMonitor`)

* **Original:** the service's `IVirtualMonitor` (`Add`, `Remove`, `AddIfNecessary(count)`) drives `vdvmd.dll`.
  * At session start, `StreamerManager` calls `AddIfNecessary(max(1, min(MonitorCount, MaxMonitors(hmd))))`, or 1 without a hardware encoder.
  * The headset's add-monitor button (`DesktopStreamer._0005`) calls `Add()` when `Monitors.Length <= MonitorCount`, then `MonitorCount++`. The remove button calls `Remove()`, then `MonitorCount--`.
  * `SharedUserSettings.ExtraDisplay` adds or removes one virtual monitor (not ported).
  * The stream descriptor describes at most 3 monitors, so a headset never shows more than 3.
* **Port:** the Parsec Virtual Display Driver (`winget install Parsec.ParsecVDD`, 0.45 tested). Its control interface comes from github.com/nomi-san/parsec-vdd (`core/parsec-vdd.h`):
  * the device interface `{00b41627-04c4-429e-a26e-0265cf50c8fa}`, opened for overlapped I/O;
  * `DeviceIoControl` with 32 input bytes: `0x22E004` adds a display and returns its index; `0x22A008` removes the display whose 16-bit big-endian index is in the input; `0x22A00C` is the keep-alive; `0x22E010` returns the version.
  * Without the keep-alive (sent every 100 ms), the driver removes its displays after about 3 s.
* **Observed timing:**
  * Windows attaches a display about 100 ms after the add.
  * The driver completes an add or remove request only once Windows has finished the previous display change, which takes about 3 s. A second add, or a remove right after a mode change, therefore takes 2.5–3 s. A remove while ddagrab captures the display is immediate.
* **Driver modes:** 24, 30, 60, 144 and 240 Hz for common resolutions up to 4096×2160 (no 72, 90 or 120 Hz). The port picks the lowest rate at or above the headset's desktop frame rate.
* **Capture:** ddagrab captures the virtual displays as outputs of the rendering GPU. The port now numbers monitors by their DXGI output name rather than by enumeration order.
* **Modes:**
  * **Physical** (default) works like the original: virtual monitors fill up to `MonitorCount`, after the physical ones.
  * **Virtual** streams only virtual monitors (`MonitorCount`, "All" = the headset maximum).
  * In both modes the virtual monitors exist only while a headset is connected.

## 6e. Changes in 1.34.25

The port was built from 1.34.18. In November 2026 the registry stopped accepting that version: `RegisterComputer` returned `_isValid = false`, which the port reports as "Streamer needs to be updated". With 1.34.25.0 reported it is accepted again. The 1.34.25 binary (SHA-256 `0229a322…0bfb8`) was compared with the port: `ilspycmd` 8.2 decompile; REA could not admit its metadata, as before.

* **Unchanged (observed):**
  * The fields of the three shared-settings classes. The only C# fields without a port counterpart are `[JsonIgnore] DesktopBitrate`, which the port computes, and an internal `_vrStart` timestamp.
  * The 88-byte stream descriptor (packet tag 2), its 3-monitor layout, and the message ids 1–100.
* **Message 23 is now `RestartStream`, 24 is `ForceKeyFrame`** (were `BeginStreaming`/`RestartStreaming`). 24 sets `DynamicSettings.ForceKeyFrame`, as before. 23 makes `StreamerManager` run, on the streamer thread, `DesktopStreamer._0004()`, which ends every monitor capture so the encoders are recreated and the descriptor is sent again (VR: `OculusStreamer._0002()`). The port restarts its pipelines and re-sends the descriptor.
* **Clipboard sharing:** new message ids 101–113 (`ClipboardChanged` … `ClipboardCopied`). Not ported; the port ignores them as before.
* **Headset types:**
  * XREAL Aura is now 1600 (was 1200), and 1011 is gone.
  * New: Pico Space Pro 1700, Meta VR Glasses 1710, Apple Vision Pro M2 1800 and M5 1801.
  * `MaxMonitors` is 4 for those four, otherwise unchanged (1/2/3).
* **4-monitor descriptor:** a 112-byte descriptor (the 88-byte layout plus a fourth size and rectangle), sent with packet tag 4 only when `hmdType >= 1700`. Every other headset still gets the first 88 bytes with tag 2. Not ported: the port describes at most 3 monitors.

## 7. Known uncertainties

* **Inferred field meanings:** `HapticFeedback.extra`, `VrFrameInfo` floats, and the `NetworkConnectionType` member mapping.
* **Keyboard struct size:** the explicit-layout `_0083_0002._0004` declares `Size = 24`, but its overlapping 4-byte slots make the marshalled size 26 (assumed). If the headset sends 4932 bytes instead of 4936, every offset after the keyboard block shifts by 2 to 4 bytes. The data channel logs a warning in that case.
* **SOAP:** namespaces and element order are inferred.
* **Encoder latency:** the CLI backend reports one frame interval, not a measured value.
* **Discovery signature:** the hash and padding of the request signature are inferred (see §6c).

## 8. Porting notes

* **Linux input:** grant `/dev/uinput` access, e.g. `KERNEL=="uinput", GROUP="input", MODE="0660"`, and add the user to `input`.
* **Linux audio:** needs `pactl`, `parec` and `pacat` (pipewire-pulse or PulseAudio).
* **Linux capture:**
  * X11: `x11grab`.
  * Wayland: `kmsgrab` (needs `cap_sys_admin` on the binary, or root) or `Port.CustomCaptureArgs`.
  * A native PipeWire portal capture is the recommended next addition.
* **ARM64:** everything is pure Rust except FFmpeg. Use `v4l2m2m` or `vaapi` encoders.
* **FFmpeg upgrades:** edit only `crates/vd-media/src/version.rs` (version-gated option spellings) and `codec.rs` (encoder options). For the `ffmpeg-lib` feature (the in-process encoder), also bump `ffmpeg-sys-next` to the matching major version and the pinned build in `packaging/windows/fetch-ffmpeg.ps1`; only `inproc.rs` touches the C API.
