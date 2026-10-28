# Waits for the panel's adb transport to come back, then installs the panel APK.
# Polls every two seconds; stops after a successful install or when the deadline
# passes. Log is plain ASCII so it survives any console code page.
param(
    [string]$Adb = "C:\Users\wbj\Tools\platform-tools\adb.exe",
    [string]$Apk = "D:\catscreen\dist\CatScreen-0.1.4-android.apk",
    [string]$Log = "D:\catscreen\adb-watch.log",
    [int]$Minutes = 30
)

$deadline = (Get-Date).AddMinutes($Minutes)
$installed = $false
$lastSeen = ""

function Write-Log([string]$text) {
    $line = "{0} {1}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss"), $text
    Add-Content -Path $Log -Value $line -Encoding ascii
}

Write-Log "watch start; apk=$Apk"

while (-not $installed -and (Get-Date) -lt $deadline) {
    $listing = & $Adb devices 2>$null
    $ready = $null
    $state = $null

    foreach ($line in $listing) {
        if ($line -match '^(\S+)\s+(device|offline|unauthorized|recovery|sideload)$') {
            if ($Matches[2] -eq "device") { $ready = $Matches[1] }
            else { $state = "$($Matches[1]) $($Matches[2])" }
        }
    }

    if ($ready) {
        if ($lastSeen -ne "ready:$ready") {
            Write-Log "transport up: $ready"
            $lastSeen = "ready:$ready"
        }
        $result = & $Adb -s $ready install -r -d $Apk 2>&1 | Out-String
        $result = $result.Trim()
        Write-Log "install: $result"
        if ($result -match "Success") {
            Write-Log "installed on $ready"
            $installed = $true
        }
    }
    elseif ($state) {
        if ($lastSeen -ne $state) {
            Write-Log "transport present but not usable: $state"
            $lastSeen = $state
        }
    }
    else {
        if ($lastSeen -ne "absent") {
            Write-Log "no device on the bus"
            $lastSeen = "absent"
        }
    }

    Start-Sleep -Seconds 2
}

if ($installed) {
    Write-Log "watch done: installed"
    exit 0
}
Write-Log "watch done: deadline reached without install"
exit 1
