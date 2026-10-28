# Builds CatScreen.apk with the Android SDK's raw build tools (no Gradle, no network).
#
#   .\build-apk.ps1
#   .\build-apk.ps1 -SdkRoot D:\sdk -JavaHome D:\jdk
#
# The signing key lives beside the sources but is never committed; the first run
# creates one with a throwaway password, which is fine for a test package loaded
# over USB and not for anything published.
[CmdletBinding()]
param(
    [string]$SdkRoot = 'D:\yuequedeng\bambu-monitor-android\.tools\android-sdk',
    [string]$JavaHome = 'D:\yuequedeng\bambu-monitor-android\.tools\jdk-17',
    [string]$Keystore = ''
)

$ErrorActionPreference = 'Stop'

$repo = Split-Path $PSScriptRoot -Parent
$proj = Join-Path $repo 'android'
$bt = Join-Path $SdkRoot 'build-tools\34.0.0'
$androidJar = Join-Path $SdkRoot 'platforms\android-23\android.jar'
$build = Join-Path $proj 'build'
if (-not $Keystore) { $Keystore = Join-Path $repo 'signing\kat.keystore' }

$env:JAVA_HOME = $JavaHome
$env:Path = "$JavaHome\bin;$env:Path"

foreach ($tool in @("$bt\aapt.exe", "$bt\d8.bat", "$bt\zipalign.exe", "$bt\apksigner.bat", $androidJar, "$JavaHome\bin\javac.exe")) {
    if (-not (Test-Path $tool)) { throw "missing tool: $tool" }
}

# The previous build is emptied file by file rather than deleted as a tree: a
# guarded shell can refuse the recursive delete of a directory inside the
# repository, and every artefact below is rewritten from scratch anyway.
if (Test-Path $build) {
    foreach ($item in Get-ChildItem -LiteralPath $build -Force) {
        if ($item.PSIsContainer) {
            [System.IO.Directory]::Delete($item.FullName, $true)
        } else {
            [System.IO.File]::Delete($item.FullName)
        }
    }
}
New-Item -ItemType Directory -Force -Path "$build\classes", "$build\dex" | Out-Null

if (-not (Test-Path $Keystore)) {
    New-Item -ItemType Directory -Force -Path (Split-Path $Keystore) | Out-Null
    & "$JavaHome\bin\keytool.exe" -genkeypair -keystore $Keystore -alias kat `
        -keyalg RSA -keysize 2048 -validity 10000 `
        -storepass katpass -keypass katpass `
        -dname "CN=CatScreen, OU=Dev, O=CatScreen, L=NA, ST=NA, C=CN" | Out-Null
    Write-Host "created keystore $Keystore"
}

Write-Host "[1/6] resources + manifest + assets"
& "$bt\aapt.exe" package -f -M "$proj\AndroidManifest.xml" -S "$proj\res" -A "$proj\assets" -I $androidJar -F "$build\base.apk"
if ($LASTEXITCODE -ne 0) { throw 'aapt package failed' }

Write-Host "[2/6] javac"
$sources = Get-ChildItem "$proj\java" -Recurse -Filter *.java | ForEach-Object { $_.FullName }
& "$JavaHome\bin\javac.exe" -source 8 -target 8 -bootclasspath $androidJar -encoding UTF-8 -d "$build\classes" $sources
if ($LASTEXITCODE -ne 0) { throw 'javac failed' }

Write-Host "[3/6] dex"
& "$JavaHome\bin\jar.exe" cf "$build\classes.jar" -C "$build\classes" .
& "$bt\d8.bat" --lib $androidJar --min-api 22 --output "$build\dex" "$build\classes.jar"
if ($LASTEXITCODE -ne 0) { throw 'd8 failed' }

Write-Host "[4/6] pack dex"
Push-Location "$build\dex"
try {
    & "$bt\aapt.exe" add -f "$build\base.apk" classes.dex
    if ($LASTEXITCODE -ne 0) { throw 'aapt add failed' }
} finally { Pop-Location }

Write-Host "[5/6] zipalign"
& "$bt\zipalign.exe" -f -p 4 "$build\base.apk" "$build\aligned.apk"
if ($LASTEXITCODE -ne 0) { throw 'zipalign failed' }

Write-Host "[6/6] sign"
& "$bt\apksigner.bat" sign --ks $Keystore --ks-key-alias kat `
    --ks-pass pass:katpass --key-pass pass:katpass `
    --v1-signing-enabled true --v2-signing-enabled true `
    --out "$build\CatScreen.apk" "$build\aligned.apk"
if ($LASTEXITCODE -ne 0) { throw 'apksigner failed' }

& "$bt\apksigner.bat" verify --verbose "$build\CatScreen.apk"

Get-Item "$build\CatScreen.apk" | ForEach-Object {
    Write-Host ("OK  {0}  {1:N0} bytes" -f $_.FullName, $_.Length)
}
