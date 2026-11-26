#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

# Build bageo-audio-helper (android/roothelper) for arm64-v8a and armeabi-v7a, and its
# audio parameter class, and put them where package-uplink-module.sh picks them up:
#
#   android/magisk/pixelcallai_uplink/roothelper/arm64-v8a/bageo-audio-helper
#   android/magisk/pixelcallai_uplink/roothelper/armeabi-v7a/bageo-audio-helper
#   android/magisk/pixelcallai_uplink/roothelper/bageo-audio-params.jar
#
# The module's customize.sh keeps the one for the phone's ABI. Needs the Android NDK
# (ANDROID_NDK_HOME or an SDK with ndk/), the Rust targets aarch64-linux-android and
# armv7-linux-androideabi, a JDK (javac) and the SDK build-tools (d8).
#
# Fixed versions (CI sets them; each must be installed, nothing falls back):
#   BAGEO_RUST_TOOLCHAIN       e.g. 1.97.0         (cargo +<toolchain>)
#   BAGEO_NDK_VERSION          e.g. 27.3.13750724  (<sdk>/ndk/<version>)
#   BAGEO_BUILD_TOOLS_VERSION  e.g. 35.0.0         (<sdk>/build-tools/<version>/d8)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ANDROID_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$ANDROID_DIR/.." && pwd)"
CRATE="$ANDROID_DIR/roothelper"
OUT="$ANDROID_DIR/magisk/pixelcallai_uplink/roothelper"
# Oldest Android the helper runs on (8.0); it only needs the C library and liblog.
API=26

find_sdk() {
  for sdk in "${ANDROID_HOME:-}" "${ANDROID_SDK_ROOT:-}" "$HOME/Android/Sdk" "$HOME/Library/Android/sdk" "/usr/local/lib/android/sdk"; do
    if [ -n "$sdk" ] && [ -d "$sdk" ]; then
      echo "$sdk"
      return 0
    fi
  done
  return 1
}

find_ndk() {
  for var in ANDROID_NDK_HOME ANDROID_NDK_ROOT ANDROID_NDK_LATEST_HOME; do
    eval "val=\${$var:-}"
    if [ -n "$val" ] && [ -d "$val" ]; then
      echo "$val"
      return 0
    fi
  done
  sdk="$(find_sdk)" || return 1
  if [ -d "$sdk/ndk" ]; then
    best="$(ls -1 "$sdk/ndk" | sort -V | tail -n1 || true)"
    if [ -n "$best" ] && [ -d "$sdk/ndk/$best" ]; then
      echo "$sdk/ndk/$best"
      return 0
    fi
  fi
  return 1
}

if [ -n "${BAGEO_NDK_VERSION:-}" ]; then
  NDK_DIR="$(find_sdk)/ndk/$BAGEO_NDK_VERSION"
  if [ ! -d "$NDK_DIR" ]; then
    echo "error: NDK $BAGEO_NDK_VERSION is not installed" >&2
    exit 1
  fi
else
  NDK_DIR="$(find_ndk)" || {
    echo "error: Android NDK not found (set ANDROID_NDK_HOME)" >&2
    exit 1
  }
fi
CARGO=(cargo)
if [ -n "${BAGEO_RUST_TOOLCHAIN:-}" ]; then
  CARGO=(cargo "+$BAGEO_RUST_TOOLCHAIN")
fi
BIN_DIR=""
for host_tag in linux-x86_64 darwin-arm64 darwin-x86_64; do
  if [ -d "$NDK_DIR/toolchains/llvm/prebuilt/$host_tag/bin" ]; then
    BIN_DIR="$NDK_DIR/toolchains/llvm/prebuilt/$host_tag/bin"
    break
  fi
done
if [ -z "$BIN_DIR" ]; then
  echo "error: LLVM prebuilt toolchain not found in $NDK_DIR" >&2
  exit 1
fi

export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$BIN_DIR/aarch64-linux-android$API-clang"
export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER="$BIN_DIR/armv7a-linux-androideabi$API-clang"
CARGO_HOME_DIR="${CARGO_HOME:-$HOME/.cargo}"
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=${REPO_ROOT}=/bageo --remap-path-prefix=${CARGO_HOME_DIR}=/cargo"
TARGET_DIR="${CARGO_TARGET_DIR:-$CRATE/target}"

echo "==> Using NDK: $NDK_DIR"
rm -rf "$OUT"
for pair in aarch64-linux-android:arm64-v8a armv7-linux-androideabi:armeabi-v7a; do
  triple="${pair%%:*}"
  abi="${pair##*:}"
  echo "==> Building bageo-audio-helper for $abi"
  "${CARGO[@]}" build --manifest-path "$CRATE/Cargo.toml" --locked --release --target "$triple"
  mkdir -p "$OUT/$abi"
  cp -f "$TARGET_DIR/$triple/release/bageo-audio-helper" "$OUT/$abi/bageo-audio-helper"
  "$BIN_DIR/llvm-strip" --strip-all "$OUT/$abi/bageo-audio-helper"
done

echo "==> Building bageo-audio-params.jar"
SDK_DIR="$(find_sdk)" || {
  echo "error: Android SDK not found (set ANDROID_HOME)" >&2
  exit 1
}
if [ -n "${BAGEO_BUILD_TOOLS_VERSION:-}" ]; then
  D8="$SDK_DIR/build-tools/$BAGEO_BUILD_TOOLS_VERSION/d8"
  [ -f "$D8" ] || D8=
else
  D8="$(find "$SDK_DIR/build-tools" -name d8 -type f 2>/dev/null | sort -V | tail -n 1)"
fi
if [ -z "$D8" ]; then
  echo "error: d8 not found under $SDK_DIR/build-tools" >&2
  exit 1
fi
CLS="$(mktemp -d)"
trap 'rm -rf "$CLS"' EXIT
javac --release 8 -nowarn -d "$CLS" "$CRATE/java/com/pixelcallai/roothelper/AudioParams.java"
"$D8" --release --min-api "$API" --output "$OUT/bageo-audio-params.jar" "$CLS/com/pixelcallai/roothelper/AudioParams.class"

ls -l "$OUT"/*/bageo-audio-helper "$OUT/bageo-audio-params.jar"
