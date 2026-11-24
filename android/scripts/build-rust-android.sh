#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ANDROID_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$ANDROID_DIR/.." && pwd)"

find_ndk() {
  for var in ANDROID_NDK_HOME ANDROID_NDK_ROOT ANDROID_NDK_LATEST_HOME; do
    eval "val=\${$var:-}"
    if [ -n "$val" ] && [ -d "$val" ]; then
      echo "$val"
      return 0
    fi
  done
  for sdk in "${ANDROID_HOME:-}" "${ANDROID_SDK_ROOT:-}" "$HOME/Android/Sdk" "/usr/local/lib/android/sdk"; do
    if [ -n "$sdk" ] && [ -d "$sdk/ndk" ]; then
      best="$(ls -1 "$sdk/ndk" | sort -V | tail -n1 || true)"
      if [ -n "$best" ] && [ -d "$sdk/ndk/$best" ]; then
        echo "$sdk/ndk/$best"
        return 0
      fi
    fi
  done
  return 1
}

NDK_DIR="$(find_ndk)" || {
  echo "error: Android NDK not found (set ANDROID_NDK_HOME)" >&2
  exit 1
}

BIN_DIR=""
for host_tag in linux-x86_64 darwin-arm64 darwin-x86_64 windows-x86_64; do
  if [ -d "$NDK_DIR/toolchains/llvm/prebuilt/$host_tag/bin" ]; then
    BIN_DIR="$NDK_DIR/toolchains/llvm/prebuilt/$host_tag/bin"
    break
  fi
done
if [ -z "$BIN_DIR" ]; then
  echo "error: LLVM prebuilt toolchain not found in $NDK_DIR" >&2
  exit 1
fi

export CC_aarch64_linux_android="$BIN_DIR/aarch64-linux-android29-clang"
export CXX_aarch64_linux_android="$BIN_DIR/aarch64-linux-android29-clang++"
export AR_aarch64_linux_android="$BIN_DIR/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$BIN_DIR/aarch64-linux-android29-clang"

echo "==> Using NDK: $NDK_DIR"
echo "==> Building agent library for aarch64-linux-android (release)..."
cargo build --manifest-path "$REPO_ROOT/Cargo.toml" -p agent --lib --target aarch64-linux-android --release

BUILT_SO="$REPO_ROOT/target/aarch64-linux-android/release/libbageo_agent.so"
if [ ! -f "$BUILT_SO" ]; then
  echo "error: expected library missing: $BUILT_SO" >&2
  exit 1
fi

OUT_DIR="$ANDROID_DIR/app/src/standalone/jniLibs/arm64-v8a"
mkdir -p "$OUT_DIR"
OUT_SO="$OUT_DIR/libbageo_agent.so"
cp -f "$BUILT_SO" "$OUT_SO"

if [ -x "$BIN_DIR/llvm-strip" ]; then
  "$BIN_DIR/llvm-strip" --strip-unneeded "$OUT_SO"
fi
if [ -x "$BIN_DIR/llvm-nm" ]; then
  "$BIN_DIR/llvm-nm" -D --defined-only "$OUT_SO" | grep -q "Java_com_pixelcallai_streamer_EmbeddedAgent_nativeStart"
fi

ls -lh "$OUT_SO"
