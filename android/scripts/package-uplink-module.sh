#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

# Build the release APK and package the Magisk / KernelSU module zip (macOS / Linux).
#
#   ./scripts/package-uplink-module.sh                  # bridge (PC-connected) module
#   ./scripts/package-uplink-module.sh --standalone     # standalone phone-only module
#
# Output:
#   android/magisk/dist/bageo-phone-module-<versionName>.zip
#   android/magisk/dist/bageo-phone-standalone-<versionName>.zip
set -euo pipefail

STANDALONE=0
SKIP_RUST_BUILD=0
for arg in "$@"; do
    case "$arg" in
        --standalone) STANDALONE=1 ;;
        --skip-rust-build) SKIP_RUST_BUILD=1 ;;
        *)
            echo "unknown option: $arg" >&2
            exit 2
            ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ANDROID_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$ANDROID_DIR/.." && pwd)"
MODULE_DIR="$ANDROID_DIR/magisk/pixelcallai_uplink"
OUT_DIR="$ANDROID_DIR/magisk/dist"

VERSION="$(sed -n 's/^[[:space:]]*versionName[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$ANDROID_DIR/app/build.gradle.kts" | head -n 1)"
APK_VCODE="$(sed -n 's/^[[:space:]]*versionCode[[:space:]]*=[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
    "$ANDROID_DIR/app/build.gradle.kts" | head -n 1)"
PROP_VERSION="$(sed -n 's/^version=//p' "$MODULE_DIR/module.prop" | tr -d '\r' | head -n 1)"
PROP_VCODE="$(sed -n 's/^versionCode=//p' "$MODULE_DIR/module.prop" | tr -d '\r' | head -n 1)"
if [ -z "$VERSION" ]; then
    echo "versionName not found in app/build.gradle.kts" >&2
    exit 1
fi
if [ "$PROP_VERSION" != "v$VERSION" ]; then
    echo "module.prop version ($PROP_VERSION) != v$VERSION from app/build.gradle.kts" >&2
    exit 1
fi
case "$APK_VCODE" in ''|0|*[!0-9]*) echo "invalid app versionCode: $APK_VCODE" >&2; exit 1 ;; esac
case "$PROP_VCODE" in ''|0|*[!0-9]*) echo "invalid module.prop versionCode: $PROP_VCODE" >&2; exit 1 ;; esac

if [ "$STANDALONE" -eq 1 ]; then
    if [ "$SKIP_RUST_BUILD" -ne 1 ]; then
        bash "$SCRIPT_DIR/build-rust-android.sh"
    fi
    GRADLE_TASK=":app:assembleStandaloneRelease"
    APK="$ANDROID_DIR/app/build/outputs/apk/standalone/release/app-standalone-release.apk"
    ZIP_OUT="$OUT_DIR/bageo-phone-standalone-$VERSION.zip"
else
    GRADLE_TASK=":app:assembleBridgeRelease"
    APK="$ANDROID_DIR/app/build/outputs/apk/bridge/release/app-bridge-release.apk"
    ZIP_OUT="$OUT_DIR/bageo-phone-module-$VERSION.zip"
fi

echo "Building APK ($GRADLE_TASK) $VERSION ..."
(cd "$ANDROID_DIR" && sh ./gradlew "$GRADLE_TASK" --quiet)

if [ ! -f "$APK" ]; then
    echo "APK missing: $APK" >&2
    exit 1
fi

# Verify single-dex invariant and keep native libraries outside base.apk
# (Android 16 priv-app sets extractNativeLibs=false, so libbageo_agent.so is
# installed under system/priv-app/<pkg>/lib/arm64/ instead of inside base.apk).
PYTHON=""
for candidate in python3 python; do
    if command -v "$candidate" >/dev/null 2>&1 && "$candidate" -c "import zipfile" >/dev/null 2>&1; then
        PYTHON="$candidate"
        break
    fi
done
if [ -n "$PYTHON" ]; then
    "$PYTHON" - "$APK" <<'PY'
import sys, zipfile
apk = sys.argv[1]
with zipfile.ZipFile(apk) as zf:
    names = set(zf.namelist())
assert "classes.dex" in names, f"classes.dex missing in {apk}"
assert "classes2.dex" not in names, f"classes2.dex found in {apk} (must stay single-dex)"
if "lib/arm64-v8a/libbageo_agent.so" in names:
    raise SystemExit(f"lib/arm64-v8a/libbageo_agent.so must live in priv-app/lib/arm64, not inside {apk}")
PY
fi

mkdir -p "$OUT_DIR"

# Zip from a staging copy so line endings and standalone assets stay out of git working tree.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp -R "$MODULE_DIR/." "$STAGE/"
PKG_DIR="$STAGE/system/priv-app/com.pixelcallai.streamer"
mkdir -p "$PKG_DIR"
cp "$APK" "$PKG_DIR/base.apk"

if [ "$STANDALONE" -eq 1 ]; then
    AGENT_SO="$ANDROID_DIR/rust-jni/arm64-v8a/libbageo_agent.so"
    if [ ! -f "$AGENT_SO" ]; then
        echo "libbageo_agent.so missing at $AGENT_SO (run build-rust-android.sh first)" >&2
        exit 1
    fi
    mkdir -p "$PKG_DIR/lib/arm64"
    cp "$AGENT_SO" "$PKG_DIR/lib/arm64/libbageo_agent.so"
    sh "$REPO_ROOT/scripts/bundle-assets.sh" copy "$REPO_ROOT/assets" "$PKG_DIR/bageo_assets"
    # The phone-only edition is its own module: store ID bageo_ai_call_agent on
    # modules.kernelsu.org. customize.sh removes pixelcallai_uplink if present.
    sed -e 's/^id=.*/id=bageo_ai_call_agent/' \
      -e 's/^name=.*/name=Bageo AI Call Agent/' \
      -e 's/^description=.*/description=Phone only: runs all of Bageo on this phone, no computer needed. To remove: switch to a normal phone in the app, then disable or remove this module and reboot. Do not also install the APK as a normal user app./' \
      "$STAGE/module.prop" > "$STAGE/module.prop.new"
    mv "$STAGE/module.prop.new" "$STAGE/module.prop"
fi

# Normalize LF only on known text files (keep APK and other binaries intact).
find "$STAGE" -type f \( -name '*.sh' -o -name '*.prop' -o -name '*.xml' -o -name '*.txt' -o -name '*.md' -o -name '*.json' -o -name 'update-binary' -o -name 'updater-script' -o -name 'inject' \) -print0 | while IFS= read -r -d '' file; do
    tr -d '\r' < "$file" > "$file.lf"
    mv "$file.lf" "$file"
done

rm -f "$ZIP_OUT"
if command -v zip >/dev/null 2>&1; then
    # -D: no directory entries, -X: no extra file attributes. Entry names use "/".
    (cd "$STAGE" && zip -q -r -D -X "$ZIP_OUT" .)
else
    if [ -z "$PYTHON" ]; then
        echo "Neither zip nor python3 found; install one of them." >&2
        exit 1
    fi
    "$PYTHON" - "$STAGE" "$ZIP_OUT" <<'PY'
import pathlib
import sys
import zipfile

stage, out = pathlib.Path(sys.argv[1]), sys.argv[2]
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as archive:
    for path in sorted(p for p in stage.rglob("*") if p.is_file()):
        archive.write(path, path.relative_to(stage).as_posix())
PY
fi

ZIP_SIZE="$(wc -c < "$ZIP_OUT" | tr -d ' ')"
if [ "$ZIP_SIZE" -lt 100000 ]; then
    echo "ZIP too small ($ZIP_SIZE bytes); APK not packaged" >&2
    exit 1
fi

if [ -n "$PYTHON" ]; then
    "$PYTHON" - "$ZIP_OUT" "$STANDALONE" <<'PY'
import sys, zipfile
zip_out, standalone = sys.argv[1], sys.argv[2] == "1"
with zipfile.ZipFile(zip_out) as zf:
    names = set(zf.namelist())
so_path = "system/priv-app/com.pixelcallai.streamer/lib/arm64/libbageo_agent.so"
asset_path = "system/priv-app/com.pixelcallai.streamer/bageo_assets/ambience_office.wav"
if standalone:
    assert so_path in names, f"{so_path} missing in {zip_out}"
    assert asset_path in names, f"{asset_path} missing in {zip_out}"
else:
    assert so_path not in names, f"{so_path} unexpectedly present in {zip_out}"
    assert asset_path not in names, f"{asset_path} unexpectedly present in {zip_out}"
PY
fi

echo "APK: $APK ($(wc -c < "$APK" | tr -d ' ') bytes)"
echo "ZIP: $ZIP_OUT ($ZIP_SIZE bytes)"
