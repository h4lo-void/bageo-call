# Myna - start the agent on Windows.
#
#   Right-click > "Run with PowerShell", or double-click start.cmd, or:
#   powershell -ExecutionPolicy Bypass -File .\start.ps1
#
# What it does, in order:
#   1. reads settings from .env next to this script (optional, see .env.example)
#   2. finds the agent program (a downloaded release, or builds it with Rust)
#   3. makes sure adb (Android platform-tools) is there, offering to download it
#   4. starts the agent and opens the web console
param(
    [switch]$NoBrowser,      # do not open the web console
    [switch]$Yes,            # answer "yes" to the platform-tools download
    [switch]$Build           # build from source even when a program is already there
)
$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
Set-Location $root

function Say($msg) { Write-Host "[myna] $msg" }

# 1. .env -> environment (real environment variables win)
$envFile = Join-Path $root ".env"
if (Test-Path $envFile) {
    foreach ($line in Get-Content $envFile -Encoding UTF8) {
        $t = $line.Trim()
        if ($t -eq "" -or $t.StartsWith("#") -or -not $t.Contains("=")) { continue }
        $i = $t.IndexOf("=")
        $key = $t.Substring(0, $i).Trim()
        $val = $t.Substring($i + 1).Trim().Trim('"').Trim("'")
        if ($key -and -not (Test-Path "Env:$key")) { Set-Item -Path "Env:$key" -Value $val }
    }
}

# 2. the agent program
$candidates = @(
    (Join-Path $root "agent.exe"),
    (Join-Path $root "bin\agent.exe"),
    (Join-Path $root "target\release\agent.exe")
)
$agent = $null
if (-not $Build) { $agent = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1 }
if (-not $agent) {
    if (Get-Command cargo -ErrorAction SilentlyContinue) {
        Say "building the agent (first time takes a few minutes)..."
        & cargo build --release -p agent
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
        $agent = Join-Path $root "target\release\agent.exe"
    } else {
        Say "The agent program was not found."
        Say "Either download a release from the project's GitHub Releases page and unzip it here,"
        Say "or install Rust (https://rustup.rs) and run this script again to build it."
        exit 1
    }
}

# 3. adb (only needed for phones on USB)
$usbOff = @("0", "false", "off", "no") -contains ("$env:USB_BRIDGE").ToLower()
if (-not $usbOff -and -not $env:ADB_BIN) {
    $localAdb = Join-Path $root "platform-tools\adb.exe"
    $haveAdb = (Test-Path $localAdb) -or (Get-Command adb -ErrorAction SilentlyContinue)
    if (-not $haveAdb) {
        Say "adb (Android platform-tools) was not found. It is needed to reach a phone over USB."
        $answer = "y"
        if (-not $Yes) { $answer = Read-Host "Download it from Google (about 7 MB) into .\platform-tools? [Y/n]" }
        if ($answer -eq "" -or $answer -match "^[Yy]") {
            $zip = Join-Path $env:TEMP "platform-tools-latest-windows.zip"
            Say "downloading platform-tools..."
            Invoke-WebRequest -Uri "https://dl.google.com/android/repository/platform-tools-latest-windows.zip" -OutFile $zip -UseBasicParsing
            Expand-Archive -Path $zip -DestinationPath $root -Force
            Remove-Item $zip -Force
            if (-not (Test-Path $localAdb)) { throw "platform-tools download did not contain adb.exe" }
            Say "adb installed at $localAdb"
        } else {
            Say "continuing without adb: phones on USB will not connect until it is installed."
        }
    }
}

# 4. run
$port = if ($env:AGENT_PORT) { $env:AGENT_PORT } else { "8787" }
$url = "http://localhost:$port/"
Say "starting $agent"
Say "web console: $url   (Ctrl+C here stops the agent)"
if (-not $NoBrowser) {
    Start-Job -ScriptBlock { param($u) Start-Sleep -Seconds 2; Start-Process $u } -ArgumentList $url | Out-Null
}
& $agent
exit $LASTEXITCODE
