# Downloads the pinned LGPL "shared" FFmpeg build used by the in-process encoder (feature
# `ffmpeg-lib`): headers + import libraries for building, DLLs for shipping.
#
# Result: third_party\ffmpeg\{include,lib,bin}. Point the build at it with
#   $env:FFMPEG_DIR = "<repo>\third_party\ffmpeg"
# (build-installer.ps1 does this automatically).
#
# To upgrade FFmpeg, change $Release/$Asset/$Sha256 below and bump `ffmpeg-sys-next` in
# crates/vd-media/Cargo.toml to the matching major version (9.x <-> FFmpeg 9).
$ErrorActionPreference = "Stop"
$Release = "autobuild-2026-10-31-12-59"
$Asset = "ffmpeg-n9.0.2-39-g7adfbe40dc-win64-lgpl-shared-9.0.zip"
$Sha256 = "e907b01fa4783913151237e37df2a10fa97d99a1598a021bf59d4ae62ffed4c0"

$root = Resolve-Path "$PSScriptRoot\..\.."
$dest = Join-Path $root "third_party\ffmpeg"
$stamp = Join-Path $dest ".version"
if ((Test-Path $stamp) -and ((Get-Content $stamp -Raw).Trim() -eq $Asset)) {
    Write-Host "FFmpeg already present: $Asset"
    exit 0
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) "vdsr-ffmpeg-$([guid]::NewGuid())"
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
    $zip = Join-Path $tmp $Asset
    $url = "https://github.com/BtbN/FFmpeg-Builds/releases/download/$Release/$Asset"
    Write-Host "Downloading $url"
    Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($hash -ne $Sha256) { throw "SHA-256 mismatch for ${Asset}: $hash" }
    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    $extracted = Get-ChildItem $tmp -Directory | Where-Object { Test-Path (Join-Path $_.FullName "include\libavcodec") } | Select-Object -First 1
    if (-not $extracted) { throw "unexpected archive layout" }
    Remove-Item -Recurse -Force $dest -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $dest | Out-Null
    foreach ($d in "include", "lib", "bin") { Copy-Item -Recurse (Join-Path $extracted.FullName $d) $dest }
    Copy-Item (Join-Path $extracted.FullName "LICENSE.txt") $dest -ErrorAction SilentlyContinue
    Set-Content $stamp $Asset
    Write-Host "FFmpeg ready in $dest"
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
