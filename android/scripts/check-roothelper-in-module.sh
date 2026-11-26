#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

# Checks that each phone module zip given carries bageo-audio-helper for both ABIs
# (built for the right CPU) and its audio parameter jar.
#
#   bash android/scripts/check-roothelper-in-module.sh android/magisk/dist/bageo-phone-*.zip
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <module zip>..." >&2
  exit 2
fi
JAR="$(mktemp)"
trap 'rm -f "$JAR"' EXIT
for zip in "$@"; do
  for entry in roothelper/arm64-v8a/bageo-audio-helper roothelper/armeabi-v7a/bageo-audio-helper roothelper/bageo-audio-params.jar; do
    if ! unzip -l "$zip" "$entry" >/dev/null 2>&1; then
      echo "$entry missing in $zip" >&2
      exit 1
    fi
  done
  kind64="$(unzip -p "$zip" roothelper/arm64-v8a/bageo-audio-helper | file -)"
  kind32="$(unzip -p "$zip" roothelper/armeabi-v7a/bageo-audio-helper | file -)"
  case "$kind64" in *"ELF 64-bit"*aarch64*) ;; *) echo "arm64-v8a helper in $zip is: $kind64" >&2; exit 1 ;; esac
  case "$kind32" in *"ELF 32-bit"*ARM*) ;; *) echo "armeabi-v7a helper in $zip is: $kind32" >&2; exit 1 ;; esac
  if ! unzip -p "$zip" roothelper/bageo-audio-params.jar > "$JAR" ||
    ! unzip -l "$JAR" classes.dex >/dev/null 2>&1; then
    echo "bageo-audio-params.jar in $zip has no classes.dex" >&2
    exit 1
  fi
  echo "$zip: audio helper arm64-v8a, armeabi-v7a and parameter jar present"
done
