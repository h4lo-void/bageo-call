#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>
# Builds a Mac app from release binaries.
# Usage: package-macos.sh <status binary> <output directory> [<agent binary> <repo folder>]
#   With only the status program: "Bageo Status.app", which watches a Bageo
#   found next to it or elsewhere.
#   With the agent and the repo folder: "Bageo.app", the whole of Bageo in one
#   app. Bageo runs from inside it, reads its sounds from Contents/Resources
#   and keeps its data in ~/Library/Application Support/Bageo.
set -eu

if [ "$#" -ne 2 ] && [ "$#" -ne 4 ]; then
  echo "usage: $0 <status binary> <output directory> [<agent binary> <repo folder>]" >&2
  exit 2
fi

BIN=$1
OUT=$2
HERE=$(cd "$(dirname "$0")" && pwd)
DESKTOP=$(cd "$HERE/.." && pwd)
VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$DESKTOP/Cargo.toml" | head -n 1)
if [ -z "$VERSION" ]; then
  echo "cannot read version from $DESKTOP/Cargo.toml" >&2
  exit 1
fi

if [ "$#" -eq 4 ]; then
  NAME="Bageo"
else
  NAME="Bageo Status"
fi
APP="$OUT/$NAME.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/bageo-status"
chmod 755 "$APP/Contents/MacOS/bageo-status"
cp "$DESKTOP/icons/icon.icns" "$APP/Contents/Resources/icon.icns"
if [ "$#" -eq 4 ]; then
  REPO=$4
  cp "$3" "$APP/Contents/MacOS/agent"
  chmod 755 "$APP/Contents/MacOS/agent"
  cp -R "$REPO/assets" "$REPO/LICENSES" "$APP/Contents/Resources/"
  cp "$REPO/LICENSE" "$REPO/NOTICE.md" "$REPO/.env.example" "$APP/Contents/Resources/"
fi

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>app.bageo.status</string>
  <key>CFBundleName</key><string>$NAME</string>
  <key>CFBundleDisplayName</key><string>$NAME</string>
  <key>CFBundleExecutable</key><string>bageo-status</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
</dict>
</plist>
EOF

codesign --force --deep -s - "$APP"
echo "$APP"
