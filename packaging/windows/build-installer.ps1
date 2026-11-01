# Builds the release executable and the Windows installer (dist\VirtualDesktopStreamerRust-<version>-setup-x64.exe)
# plus a portable zip (dist\VirtualDesktopStreamerRust-<version>-windows-x64.zip).
#
# The program is built with the in-process encoder (`--features ffmpeg-lib`) against the pinned
# LGPL FFmpeg in third_party\ffmpeg, whose DLLs and ffmpeg/ffprobe are shipped alongside.
#
# Requires Inno Setup 6 (winget install JRSoftware.InnoSetup), Visual Studio C++ build tools and
# an LLVM with libclang (see ffmpeg-env.ps1).
$ErrorActionPreference = "Stop"
$root = Resolve-Path "$PSScriptRoot\..\.."
Set-Location $root
. "$PSScriptRoot\ffmpeg-env.ps1"

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
Write-Host "Building desktop_streamer $version"
cargo build --release --features ffmpeg-lib
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 not found (winget install JRSoftware.InnoSetup)" }

New-Item -ItemType Directory -Force dist | Out-Null
& $iscc "/DAppVersion=$version" packaging\windows\installer.iss
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }

# Portable zip: the executable, FFmpeg and documentation.
$stage = Join-Path $env:TEMP "vdsr-zip-$version"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage, "$stage\licenses" | Out-Null
Copy-Item target\release\desktop_streamer.exe, README.md $stage
Copy-Item third_party\ffmpeg\bin\*.dll, third_party\ffmpeg\bin\ffmpeg.exe, third_party\ffmpeg\bin\ffprobe.exe $stage
Copy-Item third_party\ffmpeg\LICENSE.txt "$stage\licenses\FFmpeg-LICENSE.txt"
Copy-Item packaging\windows\FFMPEG-NOTICE.txt "$stage\licenses"
$zip = "dist\VirtualDesktopStreamerRust-$version-windows-x64.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path "$stage\*" -DestinationPath $zip
Remove-Item -Recurse -Force $stage

Get-ChildItem dist | ForEach-Object { "{0}  {1:N1} MB" -f $_.Name, ($_.Length / 1MB) }
