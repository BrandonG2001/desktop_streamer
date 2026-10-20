# Builds the release executable and the Windows installer (dist\VirtualDesktopStreamerRust-<version>-setup-x64.exe)
# plus a portable zip (dist\VirtualDesktopStreamerRust-<version>-windows-x64.zip).
#
# Requires Inno Setup 6 (winget install JRSoftware.InnoSetup).
$ErrorActionPreference = "Stop"
$root = Resolve-Path "$PSScriptRoot\..\.."
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
Write-Host "Building desktop_streamer $version"
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 not found (winget install JRSoftware.InnoSetup)" }

New-Item -ItemType Directory -Force dist | Out-Null
& $iscc "/DAppVersion=$version" packaging\windows\installer.iss
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }

# Portable zip: the executable and documentation.
$stage = Join-Path $env:TEMP "vdsr-zip-$version"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item target\release\desktop_streamer.exe, README.md $stage
$zip = "dist\VirtualDesktopStreamerRust-$version-windows-x64.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path "$stage\*" -DestinationPath $zip
Remove-Item -Recurse -Force $stage

Get-ChildItem dist | ForEach-Object { "{0}  {1:N1} MB" -f $_.Name, ($_.Length / 1MB) }
