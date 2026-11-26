# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

# Build the release APK and package the Magisk / KernelSU module zip (Windows).
#
#   .\scripts\package-uplink-module.ps1                  # bridge (PC-connected) module
#   .\scripts\package-uplink-module.ps1 -Standalone      # standalone phone-only module
#
# Output:
#   android\magisk\dist\bageo-phone-module-<versionName>.zip
#   android\magisk\dist\bageo-phone-standalone-<versionName>.zip
param(
    [switch]$Standalone,
    [switch]$SkipRustBuild
)

$ErrorActionPreference = "Stop"

$android = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$repoRoot = (Resolve-Path (Join-Path $android "..")).Path
$mod = Join-Path $android "magisk\pixelcallai_uplink"
$outDir = Join-Path $android "magisk\dist"

if (-not $env:ANDROID_HOME -and -not $env:ANDROID_SDK_ROOT -and -not (Test-Path (Join-Path $android "local.properties"))) {
    $defaultSdk = if ($env:LOCALAPPDATA) { Join-Path $env:LOCALAPPDATA "Android\Sdk" } else { "" }
    if ($defaultSdk -and (Test-Path $defaultSdk)) { $env:ANDROID_HOME = $defaultSdk }
}

$gradleFile = Join-Path $android "app\build.gradle.kts"
$versionMatch = Select-String -Path $gradleFile -Pattern 'versionName\s*=\s*"([^"]+)"' | Select-Object -First 1
if (-not $versionMatch) { throw "versionName not found in $gradleFile" }
$version = $versionMatch.Matches[0].Groups[1].Value
$apkVcodeMatch = Select-String -Path $gradleFile -Pattern 'versionCode\s*=\s*(\d+)' | Select-Object -First 1
if (-not $apkVcodeMatch -or [int]$apkVcodeMatch.Matches[0].Groups[1].Value -le 0) {
    throw "valid positive versionCode not found in $gradleFile"
}
$propFile = Join-Path $mod "module.prop"
$propVerMatch = Select-String -Path $propFile -Pattern '^version=(.+)$' | Select-Object -First 1
if (-not $propVerMatch -or $propVerMatch.Matches[0].Groups[1].Value.Trim() -ne "v$version") {
    throw "module.prop version does not match v$version"
}
$propVcodeMatch = Select-String -Path $propFile -Pattern '^versionCode=(\d+)' | Select-Object -First 1
if (-not $propVcodeMatch -or [int]$propVcodeMatch.Matches[0].Groups[1].Value -le 0) {
    throw "valid positive versionCode not found in $propFile"
}

if ($Standalone) {
    if (-not $SkipRustBuild) {
        & (Join-Path $PSScriptRoot "build-rust-android.ps1")
        if ($LASTEXITCODE -ne 0) { throw "build-rust-android.ps1 failed: $LASTEXITCODE" }
    }
    $gradleTask = ":app:assembleStandaloneRelease"
    $apk = Join-Path $android "app\build\outputs\apk\standalone\release\app-standalone-release.apk"
    $zipOut = Join-Path $outDir "bageo-phone-standalone-$version.zip"
} else {
    $gradleTask = ":app:assembleBridgeRelease"
    $apk = Join-Path $android "app\build\outputs\apk\bridge\release\app-bridge-release.apk"
    $zipOut = Join-Path $outDir "bageo-phone-module-$version.zip"
}

Write-Host "Building APK ($gradleTask) $version ..."
Push-Location $android
try {
    & .\gradlew.bat $gradleTask --quiet
    if ($LASTEXITCODE -ne 0) { throw "gradle $gradleTask failed: $LASTEXITCODE" }
} finally {
    Pop-Location
}

if (-not (Test-Path $apk)) { throw "APK missing: $apk" }

Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

# Verify single-dex invariant (Android 16 /system/priv-app) and ensure .so is NOT packed inside base.apk
# (priv-app keeps extractNativeLibs=false; standalone installs libbageo_agent.so under priv-app/lib/arm64).
$apkZip = [System.IO.Compression.ZipFile]::OpenRead($apk)
try {
    $names = $apkZip.Entries | ForEach-Object { $_.FullName }
    if ($names -notcontains "classes.dex") { throw "classes.dex missing in $apk" }
    if ($names -contains "classes2.dex") { throw "classes2.dex found in $apk (must stay single-dex)" }
    if ($names -contains "lib/arm64-v8a/libbageo_agent.so") {
        throw "lib/arm64-v8a/libbageo_agent.so must live in priv-app/lib/arm64, not inside $apk"
    }
} finally {
    $apkZip.Dispose()
}

New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$stage = Join-Path ([System.IO.Path]::GetTempPath()) ("bageo-mod-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $stage | Out-Null
try {
    Copy-Item -Path (Join-Path $mod "*") -Destination $stage -Recurse -Force
    $stagePrivDir = Join-Path $stage "system\priv-app\com.pixelcallai.streamer"
    New-Item -ItemType Directory -Force -Path $stagePrivDir | Out-Null
    Copy-Item $apk (Join-Path $stagePrivDir "base.apk") -Force

    if ($Standalone) {
        $agentSo = Join-Path $android "rust-jni\arm64-v8a\libbageo_agent.so"
        if (-not (Test-Path $agentSo)) {
            throw "libbageo_agent.so missing at $agentSo (run build-rust-android.ps1 first)"
        }
        $stageLibDir = Join-Path $stagePrivDir "lib\arm64"
        New-Item -ItemType Directory -Force -Path $stageLibDir | Out-Null
        Copy-Item $agentSo (Join-Path $stageLibDir "libbageo_agent.so") -Force

        $srcAssets = Join-Path $repoRoot "assets"
        $dstAssets = Join-Path $stagePrivDir "bageo_assets"
        New-Item -ItemType Directory -Force -Path $dstAssets | Out-Null
        Get-ChildItem -Path $srcAssets -Force | ForEach-Object {
            if ($_.Name -eq "voices" -and $_.PSIsContainer) {
                $dstVoices = Join-Path $dstAssets "voices"
                New-Item -ItemType Directory -Force -Path $dstVoices | Out-Null
                $enVoices = Join-Path $_.FullName "en"
                if (Test-Path $enVoices) {
                    Copy-Item -Path $enVoices -Destination (Join-Path $dstVoices "en") -Recurse -Force
                }
            } else {
                Copy-Item -Path $_.FullName -Destination (Join-Path $dstAssets $_.Name) -Recurse -Force
            }
        }
        # Override module.prop metadata cleanly for standalone zip.
        $stageProp = Join-Path $stage "module.prop"
        $propText = Get-Content -Path $stageProp -Raw -Encoding UTF8
        $propText = [regex]::Replace($propText, '(?m)^name=.*$', 'name=Bageo Standalone')
        $propText = [regex]::Replace(
            $propText,
            '(?m)^description=.*$',
            'description=All-in-one AI phone assistant running directly on your rooted Pixel without a computer. Rollback: switch to a normal phone in the app, then disable or remove this module and reboot. Do not also install the APK as a normal user app.'
        )
        [System.IO.File]::WriteAllText($stageProp, $propText, (New-Object System.Text.UTF8Encoding($false)))
    }

    if (Test-Path $zipOut) { Remove-Item $zipOut -Force }
    # Magisk needs forward-slash entry names (Compress-Archive writes backslashes on Windows).
    $fs = [System.IO.File]::Open($zipOut, [System.IO.FileMode]::Create)
    $zip = New-Object System.IO.Compression.ZipArchive($fs, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        Get-ChildItem $stage -Recurse -File | Sort-Object FullName | ForEach-Object {
            $rel = $_.FullName.Substring($stage.Length).TrimStart('\', '/')
            $entryName = ($rel -replace '\\', '/')
            $entry = $zip.CreateEntry($entryName, [System.IO.Compression.CompressionLevel]::Optimal)
            $bytes = [System.IO.File]::ReadAllBytes($_.FullName)
            # Normalize LF only on known text files (keep APK and binaries intact).
            if ($entryName -match '(^|/)(update-binary|updater-script|inject)$' -or $entryName -match '\.(sh|prop|xml|txt|md|json)$') {
                $text = [System.Text.Encoding]::UTF8.GetString($bytes) -replace "`r`n", "`n" -replace "`r", "`n"
                $bytes = (New-Object System.Text.UTF8Encoding($false)).GetBytes($text.TrimStart([char]0xFEFF))
            }
            $outStream = $entry.Open()
            try { $outStream.Write($bytes, 0, $bytes.Length) } finally { $outStream.Dispose() }
        }
    } finally {
        $zip.Dispose(); $fs.Dispose()
    }
} finally {
    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
}

$zipItem = Get-Item $zipOut
if ($zipItem.Length -lt 100000) { throw "ZIP too small ($($zipItem.Length) bytes); APK not packaged" }

$verifyZip = [System.IO.Compression.ZipFile]::OpenRead($zipOut)
try {
    $znames = $verifyZip.Entries | ForEach-Object { $_.FullName }
    $soEntry = "system/priv-app/com.pixelcallai.streamer/lib/arm64/libbageo_agent.so"
    $assetEntry = "system/priv-app/com.pixelcallai.streamer/bageo_assets/ambience_office.wav"
    if ($Standalone) {
        if ($znames -notcontains $soEntry) { throw "$soEntry missing in $zipOut" }
        if ($znames -notcontains $assetEntry) { throw "$assetEntry missing in $zipOut" }
    } else {
        if ($znames -contains $soEntry) { throw "$soEntry unexpectedly present in $zipOut" }
        if ($znames -contains $assetEntry) { throw "$assetEntry unexpectedly present in $zipOut" }
    }
} finally {
    $verifyZip.Dispose()
}

Write-Host "APK: $apk ($((Get-Item $apk).Length) bytes)"
Write-Host "ZIP: $zipOut ($($zipItem.Length) bytes)"
