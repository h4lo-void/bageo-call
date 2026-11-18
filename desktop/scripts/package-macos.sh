#!/bin/sh
# Builds "Bageo Status.app" from a release binary.
# Usage: package-macos.sh <release binary> <output directory>
set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <release binary> <output directory>" >&2
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

APP="$OUT/Bageo Status.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/bageo-status"
chmod 755 "$APP/Contents/MacOS/bageo-status"
cp "$DESKTOP/icons/icon.icns" "$APP/Contents/Resources/icon.icns"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>app.bageo.status</string>
  <key>CFBundleName</key><string>Bageo Status</string>
  <key>CFBundleDisplayName</key><string>Bageo Status</string>
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
