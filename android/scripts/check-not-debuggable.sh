#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>
#
# Fail when the app inside a phone module zip can be debugged. Usage:
#   check-not-debuggable.sh <module.zip>...
# Needs aapt2 from the Android SDK build-tools.
set -euo pipefail

SDK_ROOT="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/usr/local/lib/android/sdk}}"
AAPT2=$(find "$SDK_ROOT/build-tools" -name aapt2 -type f | sort -V | tail -n 1)
if [ -z "$AAPT2" ]; then
    echo "aapt2 not found under $SDK_ROOT/build-tools" >&2
    exit 1
fi
if [ "$#" -eq 0 ]; then
    echo "usage: $0 <module.zip>..." >&2
    exit 1
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
for ZIP in "$@"; do
    unzip -p "$ZIP" system/priv-app/com.pixelcallai.streamer/base.apk > "$TMP/base.apk"
    BADGING=$("$AAPT2" dump badging "$TMP/base.apk")
    if ! echo "$BADGING" | grep -q "^package: name='com.pixelcallai.streamer'"; then
        echo "$ZIP: aapt2 could not read the app in the module" >&2
        exit 1
    fi
    if echo "$BADGING" | grep -q "application-debuggable"; then
        echo "$ZIP: the app is debuggable (must be a release build)" >&2
        exit 1
    fi
    echo "$ZIP: app is not debuggable"
done
