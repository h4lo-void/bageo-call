#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>
# Which voice samples ship inside the installers and the Docker image.
# Only English (US) is bundled; the other voices are downloaded when they are
# first needed (crates/agent/src/voice_packs.rs). The background sounds
# (ambience_*.wav) are not bundled at all; they come from bageo-packs when the
# owner picks one (crates/agent/src/ambience.rs). The Windows installer, the
# macOS app and the Docker image all use this one script.
#
#   bundle-assets.sh copy <assets folder> <destination>   copy it, keeping only the voices below and no background sounds
#   bundle-assets.sh check <assets folder>                fail when another voice language or a background sound is in it
set -eu

BUNDLED_VOICES="en"

check() {
  found=$(find "$1" -name 'ambience_*.wav' 2>/dev/null | sort)
  if [ -n "$found" ]; then
    echo "$1 has background sounds that are not bundled:" >&2
    echo "$found" >&2
    return 1
  fi
  [ -d "$1/voices" ] || return 0
  bad=""
  for entry in "$1"/voices/*; do
    [ -e "$entry" ] || continue
    name=$(basename "$entry")
    case " $BUNDLED_VOICES " in
      *" $name "*) ;;
      *) bad="$bad $name" ;;
    esac
  done
  if [ -n "$bad" ]; then
    echo "$1/voices has voices that are not bundled:$bad (only: $BUNDLED_VOICES)" >&2
    return 1
  fi
}

case "${1:-}" in
  copy)
    [ "$#" -eq 3 ] || { echo "usage: $0 copy <assets folder> <destination>" >&2; exit 2; }
    src=$2
    dest=$3
    mkdir -p "$dest"
    for entry in "$src"/* "$src"/.[!.]*; do
      [ -e "$entry" ] || continue
      name=$(basename "$entry")
      case "$name" in ambience_*.wav) continue ;; esac
      if [ "$name" = voices ] && [ -d "$entry" ]; then
        mkdir -p "$dest/voices"
        for lang in $BUNDLED_VOICES; do
          [ -d "$entry/$lang" ] && cp -R "$entry/$lang" "$dest/voices/"
        done
      else
        cp -R "$entry" "$dest/"
      fi
    done
    check "$dest"
    ;;
  check)
    [ "$#" -eq 2 ] || { echo "usage: $0 check <assets folder>" >&2; exit 2; }
    check "$2"
    echo "$2: only the bundled voices ($BUNDLED_VOICES), no background sounds"
    ;;
  *)
    echo "usage: $0 copy <assets folder> <destination> | check <assets folder>" >&2
    exit 2
    ;;
esac
