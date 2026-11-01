# Builds the three release artefacts: the application, the apk and the checksums.
#
#   .\build-release.ps1                 # application + apk + archive
#   .\build-release.ps1 -SkipApk        # desktop only
#   .\build-release.ps1 -SkipPack       # build, do not archive
#
# The desktop build is a **Production** build. That one Cargo feature is what
# turns on login-at-startup (ADR-0051), the remote plugin catalog and the
# production configuration root; a plain `cargo build --release` produces a
# Development binary that can only answer "unavailable in development builds"
# to the startup switch. `pack-release.py` verifies the feature before packing,
# so a wrong build is caught here rather than at the user's first login.
[CmdletBinding()]
param(
    [switch]$SkipApk,
    [switch]$SkipPack,
    [string]$AndroidSdk = 'D:\yuequedeng\bambu-monitor-android\.tools\android-sdk',
    [string]$AndroidJdk = 'D:\yuequedeng\bambu-monitor-android\.tools\jdk-17',
    [string]$Mingw = 'C:\Users\wbj\Tools\mingw-portable\mingw64\bin'
)

$ErrorActionPreference = 'Stop'

$repo = Split-Path $PSScriptRoot -Parent
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

# `python` on PATH is often the Microsoft Store alias, which is a zero-byte
# reparse point that prints "Python was not found" instead of running anything.
# Packaging is the last step, so hitting it here wastes a full rebuild: resolve a
# real interpreter up front and let the alias be the thing that gets skipped.
function Resolve-Python {
    if ($env:CATSCREEN_PYTHON) { return $env:CATSCREEN_PYTHON }
    $found = Get-Command python -All -ErrorAction SilentlyContinue |
        Where-Object { $_.Source -and $_.Source -notlike '*\WindowsApps\*' } |
        Select-Object -First 1
    if ($found) { return $found.Source }
    $launcher = Get-Command py -ErrorAction SilentlyContinue
    if ($launcher) { return $launcher.Source }
    # This session has no python of its own on PATH, only the Store alias, but
    # the sandbox ships one: take the newest that exists rather than failing the
    # last step of a build that already ran for six minutes.
    $bundled = Get-ChildItem (Join-Path $env:USERPROFILE '.workbuddy\binaries\python\versions') `
        -Directory -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending |
        ForEach-Object { Join-Path $_.FullName 'python.exe' } |
        Where-Object { Test-Path $_ } |
        Select-Object -First 1
    if ($bundled) { return $bundled }
    throw 'no python interpreter found; set CATSCREEN_PYTHON to one'
}

# The Windows build splices MinGW's `gcc` in as the linker's C compiler; without
# it the `cc` crate picks up whatever msvc toolchain happens to be on PATH.
if (Test-Path $Mingw) {
    $env:Path = "$Mingw;$env:Path"
    $env:CC = Join-Path $Mingw 'gcc.exe'
}
$env:Path = "$cargoBin;$env:Path"

Write-Host '[1/3] apk'
if (-not $SkipApk) {
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'build-apk.ps1') `
        -SdkRoot $AndroidSdk -JavaHome $AndroidJdk
    if ($LASTEXITCODE -ne 0) { throw 'apk build failed' }
} else {
    Write-Host '      skipped'
}

Write-Host '[2/3] desktop (production)'
# `desktop/` is its own workspace root (the repository root excludes it), so the
# build has to run from there — `-p` from the root fails with "cannot specify
# features for packages outside of workspace".
Push-Location (Join-Path $repo 'desktop')
try {
    cargo build --release -p bongocat-app --features production
    if ($LASTEXITCODE -ne 0) { throw 'desktop build failed' }
} finally { Pop-Location }

Write-Host '[3/3] archive'
if ($SkipPack) {
    Write-Host '      skipped'
} else {
    $python = Resolve-Python
    Write-Host "      python: $python"
    & $python (Join-Path $PSScriptRoot 'pack-release.py')
    if ($LASTEXITCODE -ne 0) { throw 'packaging failed' }
}
