//! # vd-platform
//!
//! Operating-system services needed by the streamer, with Linux and Windows implementations.
//!
//! On Windows the original split these between the user-mode streamer and a SYSTEM service
//! (`VirtualDesktop.Service.exe` / `VirtualDesktop.Server.exe`) reached over WCF named pipes
//! (`IInputInjection`, `IAudioService`, `IMonitorDuplication`, `IVirtualMonitor`,
//! `IServiceManager`), plus three kernel/user-mode drivers (virtual audio `vdvad.sys`, virtual
//! gamepad `vdvge.sys`, IddCx virtual monitor `vdvmd.dll`).  On Linux the same functionality is
//! reached directly from the streamer process:
//!
//! | service (C#)        | module          | Linux implementation                        | Windows implementation        |
//! |---------------------|-----------------|---------------------------------------------|-------------------------------|
//! | `IInputInjection`   | [`input`]       | `/dev/uinput` keyboard, mouse, pointer, pad | `SendInput`                   |
//! | `LoopbackCapture`, `Microphone`, `IAudioService` + `vdvad.sys` | [`audio`] | PulseAudio/PipeWire null sink + remap source | WASAPI loopback + `IKsControl` on the installed `vdvad.sys` |
//! | `IMonitorDuplication.GetMonitors` | [`display`] | `xrandr --listmonitors`            | `EnumDisplayMonitors`         |
//! | `libVirtualDesktopNet.dll` | [`wlan`] | `iw` power-save off                         | `wlanapi` (exact port)        |
//! | `IServiceManager.UseRealtimePriority` | [`process`] | `setpriority`                    | `SetPriorityClass`            |
//! | named shared memory / events (VR drivers) | [`shm`] | POSIX `shm_open` + `sem_open`  | `CreateFileMapping` / events  |

#![warn(missing_docs)]

pub mod audio;
pub mod display;
pub mod error;
pub mod input;
pub mod keymap;
pub mod process;
pub mod shm;
pub mod wlan;

pub use error::{PlatformError, Result};
