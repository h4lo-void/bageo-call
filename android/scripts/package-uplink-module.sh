#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

# Build the debug APK and package the Magisk / KernelSU module zip (macOS / Linux).
#
#   ./scripts/package-uplink-module.sh
#
# Output: android/magisk/dist/bageo-phone-module-<versionName>.zip
# Module layout: system/priv-app/com.pixelcallai.streamer/base.apk + privapp-permissions XML.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ANDROID_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
MODULE_DIR="$ANDROID_DIR/magisk/pixelcallai_uplink"
APK="$ANDROID_DIR/app/build/outputs/apk/debug/app-debug.apk"
PRIV_DIR="$MODULE_DIR/system/priv-app/com.pixelcallai.streamer"
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
ZIP_OUT="$OUT_DIR/bageo-phone-module-$VERSION.zip"

echo "Building debug APK $VERSION ..."
(cd "$ANDROID_DIR" && sh ./gradlew :app:assembleDebug --quiet)

if [ ! -f "$APK" ]; then
    echo "APK missing: $APK" >&2
    exit 1
fi

mkdir -p "$PRIV_DIR" "$OUT_DIR"
cp "$APK" "$PRIV_DIR/base.apk"

# Zip from a staging copy so line endings can be normalized without touching the tree.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp -R "$MODULE_DIR/." "$STAGE/"

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
    # Fallback when zip is not installed. Check that the interpreter really runs.
    PYTHON=""
    for candidate in python3 python; do
        if "$candidate" -c "import zipfile" >/dev/null 2>&1; then
            PYTHON="$candidate"
            break
        fi
    done
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

echo "APK: $APK ($(wc -c < "$APK" | tr -d ' ') bytes)"
echo "ZIP: $ZIP_OUT ($ZIP_SIZE bytes)"
