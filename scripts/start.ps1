# Starts the round-screen pipeline: publisher on the PC, client on the device.
#
# The device reaches the publisher through `adb reverse`, so the USB cable has to
# stay connected; there is no wireless path yet.
#
#   .\start.ps1                 # standard model, use the existing build
#   .\start.ps1 -Build          # rebuild the publisher first
#   .\start.ps1 -Model keyboard
#   .\start.ps1 -Stop           # stop the client and the publisher
#
# Nothing is drawn on the desktop: the publisher has no window at all.
[CmdletBinding()]
param(
    [string]$Model = 'standard',
    [switch]$Build,
    [switch]$Stop,
    [string]$Address = '127.0.0.1:7391',
    [string]$Adb = 'C:\Users\wbj\AppData\Local\Temp\opencode\platform-tools\adb.exe',
    [string]$Serial = '11B062483A'
)

$ErrorActionPreference = 'Stop'

$repo = Split-Path $PSScriptRoot -Parent
$package = 'com.bongocat.catscreen'
$publisher = Join-Path $repo 'target\debug\catscreen.exe'
$apk = Join-Path $repo 'android\build\CatScreen.apk'
$minswg = 'C:\Users\wbj\Tools\mingw-portable\mingw64\bin'
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

if (-not (Test-Path $Adb)) { throw "adb not found: $Adb" }

# adb reports ordinary progress on stderr, which `Stop` would turn into a
# terminating error. Callers check $LASTEXITCODE where it matters.
function Invoke-Adb {
    param([string[]]$Arguments)
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $Adb @Arguments 2>&1 } finally { $ErrorActionPreference = $previous }
}

function Stop-Publisher {
    Get-Process catscreen -ErrorAction SilentlyContinue | ForEach-Object {
        $_.Kill()
        Write-Host "stopped publisher (pid $($_.Id))"
    }
}

function Set-DeviceTunnel {
    # `adb reverse --list` is not usable on this device (Android 5.1 answers with
    # a protocol fault), so the registration is simply re-issued: it is
    # idempotent and the device reconnects by itself.
    Invoke-Adb @('reverse', "tcp:$($Address.Split(':')[-1])", "tcp:$($Address.Split(':')[-1])") | Out-Null
}

if ($Stop) {
    Invoke-Adb @('shell', 'am', 'force-stop', $package) | Out-Null
    Stop-Publisher
    return
}

if ($Build) {
    $env:Path = "$minswg;$cargoBin;$env:Path"
    $env:CC = Join-Path $minswg 'gcc.exe'
    Push-Location $repo
    try {
        cargo build -p catscreen
        if ($LASTEXITCODE -ne 0) { throw 'publisher build failed' }
    } finally { Pop-Location }
}

if (-not (Test-Path $publisher)) { throw "publisher not built: $publisher (rerun with -Build)" }

# A stale adb server is the usual reason the device silently never connects.
Invoke-Adb @('start-server') | Out-Null
$devices = Invoke-Adb @('devices')
if (-not ($devices -match [regex]::Escape($Serial) + '\s+device')) {
    throw "the device is not attached:`n$($devices -join "`n")"
}
Set-DeviceTunnel

if (Test-Path $apk) {
    $installed = Invoke-Adb @('install', '-r', $apk) |
        Where-Object { $_ -match '\S' } | Select-Object -Last 1
    Write-Host "install: $installed"
}

Stop-Publisher
Start-Process -FilePath $publisher -ArgumentList @('--model', $Model, '--address', $Address) `
    -WorkingDirectory $repo -WindowStyle Minimized
Start-Sleep -Milliseconds 1500

Invoke-Adb @('shell', 'am', 'force-stop', $package) | Out-Null
Invoke-Adb @('shell', 'am', 'start', '-n', "$package/.MainActivity") | Out-Null

Write-Host "publisher: $publisher (model $Model)"
Write-Host "device:    $package on $Address via adb reverse"
Write-Host "log:       & `"$Adb`" logcat -d -s CatRenderer"
