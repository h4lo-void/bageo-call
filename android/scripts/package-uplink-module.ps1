# Build the debug APK and package the Magisk / KernelSU module zip (Windows).
#
#   .\scripts\package-uplink-module.ps1
#
# Output: android\magisk\dist\pixelcallai_uplink-<versionName>.zip
# Module layout: system/priv-app/com.pixelcallai.streamer/base.apk + privapp-permissions XML.
$ErrorActionPreference = "Stop"

$android = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$mod = Join-Path $android "magisk\pixelcallai_uplink"
$apk = Join-Path $android "app\build\outputs\apk\debug\app-debug.apk"
$outDir = Join-Path $android "magisk\dist"
$privDir = Join-Path $mod "system\priv-app\com.pixelcallai.streamer"

$gradleFile = Join-Path $android "app\build.gradle.kts"
$versionMatch = Select-String -Path $gradleFile -Pattern 'versionName\s*=\s*"([^"]+)"' | Select-Object -First 1
if (-not $versionMatch) { throw "versionName not found in $gradleFile" }
$version = $versionMatch.Matches[0].Groups[1].Value
$zipOut = Join-Path $outDir "pixelcallai_uplink-$version.zip"

Write-Host "Building debug APK $version ..."
Push-Location $android
try {
    & .\gradlew.bat :app:assembleDebug --quiet
    if ($LASTEXITCODE -ne 0) { throw "gradle assembleDebug failed: $LASTEXITCODE" }
} finally {
    Pop-Location
}

if (-not (Test-Path $apk)) { throw "APK missing: $apk" }

New-Item -ItemType Directory -Force -Path $outDir | Out-Null
New-Item -ItemType Directory -Force -Path $privDir | Out-Null
Copy-Item $apk (Join-Path $privDir "base.apk") -Force

if (Test-Path $zipOut) { Remove-Item $zipOut -Force }
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
# Magisk needs forward-slash entry names (Compress-Archive / CreateFromDirectory write
# backslashes on Windows PowerShell), and the scripts must have LF line endings.
$fs = [System.IO.File]::Open($zipOut, [System.IO.FileMode]::Create)
$zip = New-Object System.IO.Compression.ZipArchive($fs, [System.IO.Compression.ZipArchiveMode]::Create)
try {
    Get-ChildItem $mod -Recurse -File | Sort-Object FullName | ForEach-Object {
        $rel = $_.FullName.Substring($mod.Length).TrimStart('\', '/')
        $entryName = ($rel -replace '\\', '/')
        $entry = $zip.CreateEntry($entryName, [System.IO.Compression.CompressionLevel]::Optimal)
        $bytes = [System.IO.File]::ReadAllBytes($_.FullName)
        # Everything except the APK is text (sh, prop, xml, update-binary, inject).
        if ($entryName -notmatch '\.apk$') {
            $text = [System.Text.Encoding]::UTF8.GetString($bytes) -replace "`r`n", "`n" -replace "`r", "`n"
            $bytes = (New-Object System.Text.UTF8Encoding($false)).GetBytes($text.TrimStart([char]0xFEFF))
        }
        $outStream = $entry.Open()
        try { $outStream.Write($bytes, 0, $bytes.Length) } finally { $outStream.Dispose() }
    }
} finally {
    $zip.Dispose(); $fs.Dispose()
}

$zipItem = Get-Item $zipOut
if ($zipItem.Length -lt 100000) { throw "ZIP too small ($($zipItem.Length) bytes); APK not packaged" }

Write-Host "APK: $apk ($((Get-Item $apk).Length) bytes)"
Write-Host "ZIP: $zipOut ($($zipItem.Length) bytes)"
