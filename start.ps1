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
#   5. the first time the agent answers, puts a "Myna Console" shortcut to the
#      web console on the desktop (-NoShortcut or MYNA_NO_SHORTCUT=1: don't)
param(
    [switch]$NoBrowser,      # do not open the web console
    [switch]$Yes,            # answer "yes" to the platform-tools download
    [switch]$Build,          # build from source even when a program is already there
    [switch]$NoShortcut      # do not put a shortcut to the web console on the desktop
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

# 5. desktop shortcut to the web console, once the agent answers; kept when it exists
$noShortcutEnv = @("1", "true", "yes", "on") -contains ("$env:MYNA_NO_SHORTCUT").ToLower()
if (-not $NoShortcut -and -not $noShortcutEnv) {
    # The name in the system's language (written as escapes: this file is read as ANSI).
    $label = switch -Regex ((Get-UICulture).Name) {
        "^zh" { [regex]::Unescape("Myna \u63a7\u5236\u53f0"); break }
        "^ja" { [regex]::Unescape("Myna \u30b3\u30f3\u30bd\u30fc\u30eb"); break }
        "^ko" { [regex]::Unescape("Myna \ucf58\uc194"); break }
        "^es" { "Consola de Myna"; break }
        "^fr" { "Console Myna"; break }
        "^de" { "Myna-Konsole"; break }
        "^pt" { "Console do Myna"; break }
        default { "Myna Console" }
    }
    $desktop = [Environment]::GetFolderPath("Desktop")
    if ($desktop) {
        $shortcut = Join-Path $desktop "$label.url"
        if (-not (Test-Path -LiteralPath $shortcut)) {
            # Ask the agent where it listens: 127.0.0.1 unless AGENT_HOST names one address.
            $probeHost = "127.0.0.1"
            $listenHost = "$env:AGENT_HOST".Trim()
            if ($listenHost -and $listenHost -ne "0.0.0.0" -and $listenHost -ne "::") {
                $probeHost = if ($listenHost.Contains(":")) { "[$listenHost]" } else { $listenHost }
            }
            Start-Job -ScriptBlock {
                param($probe, $file, $target)
                for ($i = 0; $i -lt 60; $i++) {
                    Start-Sleep -Seconds 2
                    try { Invoke-WebRequest -Uri $probe -UseBasicParsing -TimeoutSec 2 | Out-Null } catch { continue }
                    Set-Content -LiteralPath $file -Value "[InternetShortcut]`r`nURL=$target" -Encoding ASCII
                    break
                }
            } -ArgumentList "http://${probeHost}:$port/health", $shortcut, $url | Out-Null
        }
    }
}
& $agent
exit $LASTEXITCODE
