# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

<#
.SYNOPSIS
    Cross-compile crates/agent into arm64-v8a libbageo_agent.so for the Android standalone flavor.
#>
param(
    [string]$NdkPath = ""
)

$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$androidDir = Split-Path -Parent $scriptDir
$repoRoot = Split-Path -Parent $androidDir

function Find-Ndk([string]$explicit) {
    if ($explicit -and (Test-Path $explicit)) { return (Resolve-Path $explicit).Path }
    foreach ($envName in @("ANDROID_NDK_HOME", "ANDROID_NDK_ROOT", "ANDROID_NDK_LATEST_HOME")) {
        $val = [Environment]::GetEnvironmentVariable($envName)
        if ($val -and (Test-Path $val)) { return (Resolve-Path $val).Path }
    }
    $sdkRoots = @()
    if ($env:ANDROID_HOME) { $sdkRoots += $env:ANDROID_HOME }
    if ($env:ANDROID_SDK_ROOT) { $sdkRoots += $env:ANDROID_SDK_ROOT }
    if ($env:LOCALAPPDATA) { $sdkRoots += (Join-Path $env:LOCALAPPDATA "Android\Sdk") }
    foreach ($sdk in $sdkRoots) {
        $ndkParent = Join-Path $sdk "ndk"
        if (Test-Path $ndkParent) {
            $best = Get-ChildItem -Path $ndkParent -Directory | Sort-Object Name -Descending | Select-Object -First 1
            if ($best) { return $best.FullName }
        }
    }
    throw "Android NDK not found. Set ANDROID_NDK_HOME or install NDK via Android SDK Manager."
}

$ndk = Find-Ndk $NdkPath
$binDir = $null
foreach ($hostTag in @("windows-x86_64", "linux-x86_64", "darwin-x86_64", "darwin-arm64")) {
    $cand = Join-Path $ndk "toolchains\llvm\prebuilt\$hostTag\bin"
    if (Test-Path $cand) { $binDir = $cand; break }
}
if (-not $binDir) {
    throw "LLVM prebuilt toolchain not found under NDK: $ndk"
}

$clang = Join-Path $binDir "aarch64-linux-android29-clang.cmd"
$clangPp = Join-Path $binDir "aarch64-linux-android29-clang++.cmd"
$ar = Join-Path $binDir "llvm-ar.exe"
$strip = Join-Path $binDir "llvm-strip.exe"
$nm = Join-Path $binDir "llvm-nm.exe"
if (-not (Test-Path $clang)) { $clang = Join-Path $binDir "aarch64-linux-android29-clang" }
if (-not (Test-Path $clangPp)) { $clangPp = Join-Path $binDir "aarch64-linux-android29-clang++" }
if (-not (Test-Path $ar)) { $ar = Join-Path $binDir "llvm-ar" }
if (-not (Test-Path $strip)) { $strip = Join-Path $binDir "llvm-strip" }
if (-not (Test-Path $nm)) { $nm = Join-Path $binDir "llvm-nm" }

$env:CC_aarch64_linux_android = $clang
$env:CXX_aarch64_linux_android = $clangPp
$env:AR_aarch64_linux_android = $ar
$env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = $clang

Write-Host "==> Using NDK: $ndk"
Write-Host "==> Building agent library for aarch64-linux-android (release)..."
Push-Location $repoRoot
try {
    & cargo build --manifest-path (Join-Path $repoRoot "Cargo.toml") -p agent --lib --target aarch64-linux-android --release
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    Pop-Location
}

$builtSo = Join-Path $repoRoot "target\aarch64-linux-android\release\libbageo_agent.so"
if (-not (Test-Path $builtSo)) {
    throw "Expected native library missing: $builtSo"
}

$outDir = Join-Path $androidDir "rust-jni\arm64-v8a"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$outSo = Join-Path $outDir "libbageo_agent.so"
Copy-Item -Force $builtSo $outSo
$legacyJni = Join-Path $androidDir "app\src\standalone\jniLibs"
if (Test-Path $legacyJni) { Remove-Item -Recurse -Force $legacyJni }

if (Test-Path $strip) {
    & $strip --strip-unneeded $outSo
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

if (Test-Path $nm) {
    $exports = & $nm -D --defined-only $outSo | Select-String "Java_com_pixelcallai_streamer_EmbeddedAgent_nativeStart"
    if (-not $exports) {
        throw "JNI symbol Java_com_pixelcallai_streamer_EmbeddedAgent_nativeStart missing from $outSo"
    }
}

$mb = [math]::Round((Get-Item $outSo).Length / 1MB, 2)
Write-Host "==> Built & stripped: $outSo ($mb MB)"
