# Sets up the environment for building with `--features ffmpeg-lib` on Windows.  Dot-source it:
#
#   . .\packaging\windows\ffmpeg-env.ps1
#   cargo build --release --features ffmpeg-lib
#
# * FFMPEG_DIR  -> third_party\ffmpeg (fetched by fetch-ffmpeg.ps1 if missing)
# * LIBCLANG_PATH / PATH -> an LLVM with libclang.dll (bindgen generates the FFmpeg bindings)
# * BINDGEN_EXTRA_CLANG_ARGS -> parse headers for the MSVC target with the MSVC + Windows SDK
#   include folders (needed when libclang comes from a MinGW toolchain)
# * PATH += third_party\ffmpeg\bin so the built program finds the FFmpeg DLLs when run.
$root = Resolve-Path "$PSScriptRoot\..\.."
$ffmpeg = Join-Path $root "third_party\ffmpeg"
if (-not (Test-Path (Join-Path $ffmpeg "include\libavcodec"))) { & "$PSScriptRoot\fetch-ffmpeg.ps1" }
$env:FFMPEG_DIR = $ffmpeg

if (-not $env:LIBCLANG_PATH) {
    $clang = @("$env:ProgramFiles\LLVM\bin", "C:\mingw64\bin") + @((Get-Command clang -ErrorAction SilentlyContinue | ForEach-Object { Split-Path $_.Source }))
    $dir = $clang | Where-Object { $_ -and (Test-Path (Join-Path $_ "libclang.dll")) } | Select-Object -First 1
    if (-not $dir) { throw "libclang.dll not found; install LLVM (winget install LLVM.LLVM) or set LIBCLANG_PATH" }
    $env:LIBCLANG_PATH = $dir
}
if ($env:PATH -notlike "*$($env:LIBCLANG_PATH)*") { $env:PATH = "$($env:LIBCLANG_PATH);$env:PATH" }

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -property installationPath
$msvc = Get-ChildItem "$vs\VC\Tools\MSVC" -Directory | Sort-Object Name -Descending | Select-Object -First 1
$sdk = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\Include" -Directory | Where-Object { Test-Path "$($_.FullName)\um\d3d12video.h" } | Sort-Object Name -Descending | Select-Object -First 1
if (-not $msvc -or -not $sdk) { throw "MSVC or Windows SDK headers not found" }
$inc = @("$($msvc.FullName)\include", "$($sdk.FullName)\ucrt", "$($sdk.FullName)\um", "$($sdk.FullName)\shared") | ForEach-Object { "-isystem `"$($_ -replace '\\','/')`"" }
$env:BINDGEN_EXTRA_CLANG_ARGS = "--target=x86_64-pc-windows-msvc -fms-compatibility -fms-extensions " + ($inc -join " ")

$bin = Join-Path $ffmpeg "bin"
if ($env:PATH -notlike "*$bin*") { $env:PATH = "$bin;$env:PATH" }
Write-Host "FFmpeg build environment ready (FFMPEG_DIR=$env:FFMPEG_DIR, libclang in $env:LIBCLANG_PATH)"
