; Inno Setup script for the Windows installer of desktop_streamer.
;
; Build with packaging\windows\build-installer.ps1, which fetches the pinned FFmpeg
; (third_party\ffmpeg), builds the program with the in-process encoder and runs ISCC.
; The installer is written to dist\.

#define AppName "Virtual Desktop Streamer (Rust)"
#ifndef AppVersion
  #define AppVersion "1.2.1"
#endif
#define AppExe "desktop_streamer.exe"
#define AppPublisher "Brandon Graeber"
#define AppUrl "https://github.com/BrandonG2001/desktop_streamer"
#define FirewallRule "Virtual Desktop Streamer (Rust)"

[Setup]
AppId={{6B0D5C2E-3F1A-4C8E-9B7D-2A61E4F0C9D3}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; Firewall rules and Program Files need elevation.
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
SetupIconFile=..\icons\app.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
InfoBeforeFile=before-install.txt
OutputDir=..\..\dist
OutputBaseFilename=VirtualDesktopStreamerRust-{#AppVersion}-setup-x64
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Close a running copy (Restart Manager) before replacing the executable.
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "startup"; Description: "Start with Windows"; GroupDescription: "Startup:"
Name: "firewall"; Description: "Allow headset connections through Windows Firewall"; GroupDescription: "Network:"
Name: "ytdlp"; Description: "Install yt-dlp with winget (optional, for ""Paste URL"" video downloads)"; GroupDescription: "Dependencies:"; Flags: unchecked; Check: not YtDlpFound

[Files]
Source: "..\..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
; FFmpeg (LGPL): libraries of the in-process encoder, plus ffmpeg/ffprobe for video playback
; and the command-line fallback.
Source: "..\..\third_party\ffmpeg\bin\*.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\third_party\ffmpeg\bin\ffmpeg.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\third_party\ffmpeg\bin\ffprobe.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\third_party\ffmpeg\LICENSE.txt"; DestDir: "{app}\licenses"; DestName: "FFmpeg-LICENSE.txt"; Flags: ignoreversion
Source: "FFMPEG-NOTICE.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; DestName: "README.md"; Flags: ignoreversion
Source: "..\..\docs\REVERSE_ENGINEERING.md"; DestDir: "{app}\docs"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\{cm:UninstallProgram,{#AppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
; "Start with Windows": the program writes its own per-user Run entry and setting, as the
; installing user (an elevated installer's HKCU may belong to a different administrator).
Filename: "{app}\{#AppExe}"; Parameters: "autostart on"; Flags: runhidden runasoriginaluser waituntilterminated; Tasks: startup
Filename: "{app}\{#AppExe}"; Parameters: "autostart off"; Flags: runhidden runasoriginaluser waituntilterminated; Tasks: not startup
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FirewallRule}"""; Flags: runhidden; Tasks: firewall
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#FirewallRule}"" dir=in action=allow program=""{app}\{#AppExe}"" enable=yes profile=any"; Flags: runhidden; Tasks: firewall; StatusMsg: "Adding firewall rule..."
Filename: "winget.exe"; Parameters: "install --id yt-dlp.yt-dlp -e --silent --accept-package-agreements --accept-source-agreements"; Flags: runhidden waituntilterminated; Tasks: ytdlp; StatusMsg: "Installing yt-dlp..."
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/IM {#AppExe} /F"; Flags: runhidden; RunOnceId: "StopApp"
Filename: "{app}\{#AppExe}"; Parameters: "autostart off"; Flags: runhidden waituntilterminated; RunOnceId: "NoAutostart"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FirewallRule}"""; Flags: runhidden; RunOnceId: "DelFirewall"

[Code]
{ Reports whether an executable can be found on PATH. }
function OnPath(const Exe: String): Boolean;
begin
  Result := FileSearch(Exe, GetEnv('PATH')) <> '';
end;

{ yt-dlp on PATH or in the WinGet links folder. }
function YtDlpFound: Boolean;
begin
  Result := OnPath('yt-dlp.exe') or FileExists(ExpandConstant('{localappdata}\Microsoft\WinGet\Links\yt-dlp.exe'));
end;

{ The Virtual Desktop audio driver (vdvad.sys) provides the virtual microphone. }
function AudioDriverInstalled: Boolean;
begin
  Result := RegKeyExists(HKLM, 'SYSTEM\CurrentControlSet\Services\vdvad_WaveExtensible');
end;

procedure CurrentStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and (not AudioDriverInstalled) and (not WizardSilent) then
    MsgBox('The Virtual Desktop audio driver was not found.' + #13#10#13#10 +
           'Video, input and PC audio work without it (in "VR headset only" mode the PC speakers are ' +
           'muted while streaming). For microphone passthrough, install either the official Virtual ' +
           'Desktop Streamer or a virtual audio cable such as VB-Audio Virtual Cable ' +
           '(https://vb-audio.com/Cable/), then select "CABLE Output" as microphone in your apps.', mbInformation, MB_OK);
end;
