#!/system/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

# Safe flash checklist:
# 1) Uninstall user /data/app com.pixelcallai.streamer FIRST (never dual-install).
# 2) magisk --install-module this zip → reboot.
# 3) Rollback if boot fails: Magisk Safe Mode (vol key), or from adb/recovery:
#      touch /data/adb/modules/pixelcallai_uplink/disable && reboot
#      OR touch /data/adb/modules/pixelcallai_uplink/remove && reboot

ui_print "- pixelcallai_uplink: priv-app + telephony uplink permissions"

if pm path com.pixelcallai.streamer 2>/dev/null | grep -q '^package:/data/app/'; then
  ui_print "! FATAL: user-installed com.pixelcallai.streamer detected"
  abort "! Uninstall user-installed com.pixelcallai.streamer before flashing module"
fi

if [ -f "$MODPATH/inject" ]; then
  chmod 755 "$MODPATH/inject"
fi

APK="$MODPATH/system/priv-app/com.pixelcallai.streamer/base.apk"
PERM="$MODPATH/system/etc/permissions/privapp-permissions-com.pixelcallai.streamer.xml"

if [ ! -f "$APK" ]; then
  ui_print "! FATAL: base.apk missing — aborting module install"
  abort "! base.apk missing from module zip"
fi

# Reject stub/empty packages that previously risked PackageManager boot loops.
APK_SIZE=$(wc -c < "$APK" 2>/dev/null | tr -d ' ')
if [ -z "$APK_SIZE" ] || [ "$APK_SIZE" -lt 100000 ]; then
  ui_print "! FATAL: base.apk too small (${APK_SIZE:-0} bytes)"
  abort "! base.apk too small — refusing unsafe install"
fi

chmod 644 "$APK"
chcon u:object_r:system_file:s0 "$APK" 2>/dev/null || true

if [ -f "$PERM" ]; then
  chmod 644 "$PERM"
  chcon u:object_r:system_file:s0 "$PERM" 2>/dev/null || true
  ui_print "- privapp-permissions XML present"
else
  ui_print "! FATAL: privapp-permissions XML missing — aborting module install"
  abort "! privapp-permissions XML missing"
fi

ui_print "- priv-app APK: ${APK_SIZE} bytes"
ui_print "- rollback: touch \$MODDIR/disable && reboot"

# bageo-audio-helper, the root audio helper for phones whose call audio path the app
# cannot use by itself: keep only the build for this phone's ABI, in bin/. It runs
# only when the app asks for it (mode_daemon.sh), never at boot.
HELPER_SRC="$MODPATH/roothelper"
if [ -d "$HELPER_SRC" ]; then
  HELPER_ABI=
  case "${ARCH:-}" in
    arm64) HELPER_ABI=arm64-v8a ;;
    arm) HELPER_ABI=armeabi-v7a ;;
    '')
      case "$(getprop ro.product.cpu.abi)" in
        arm64-v8a) HELPER_ABI=arm64-v8a ;;
        armeabi-v7a|armeabi) HELPER_ABI=armeabi-v7a ;;
      esac ;;
  esac
  if [ -n "$HELPER_ABI" ] && [ -f "$HELPER_SRC/$HELPER_ABI/bageo-audio-helper" ]; then
    # Root's alone: the helper refuses a parameter tool others could write.
    mkdir -p "$MODPATH/bin" &&
      chown 0:0 "$MODPATH/bin" && chmod 0755 "$MODPATH/bin" &&
      cp -f "$HELPER_SRC/$HELPER_ABI/bageo-audio-helper" "$MODPATH/bin/bageo-audio-helper" &&
      chown 0:0 "$MODPATH/bin/bageo-audio-helper" && chmod 0700 "$MODPATH/bin/bageo-audio-helper" ||
      abort "! cannot install the audio helper with root-only permissions"
    if [ -f "$HELPER_SRC/bageo-audio-params.jar" ]; then
      cp -f "$HELPER_SRC/bageo-audio-params.jar" "$MODPATH/bin/bageo-audio-params.jar" &&
        chown 0:0 "$MODPATH/bin/bageo-audio-params.jar" && chmod 0600 "$MODPATH/bin/bageo-audio-params.jar" ||
        abort "! cannot install the audio parameter tool with root-only permissions"
    fi
    ui_print "- audio helper: $HELPER_ABI (runs only when the app asks)"
  else
    ui_print "- audio helper: none for this phone (${ARCH:-unknown})"
  fi
  rm -rf "$HELPER_SRC"
fi

# pixelcallai_uplink (computer-linked) and bageo_ai_call_agent (phone-only)
# install the same app. Keep only the module being installed: the other one
# is removed at the next reboot.
SELF_ID=$(sed -n 's/^id=//p' "$MODPATH/module.prop" | tr -d '\r' | head -n 1)
for OTHER in pixelcallai_uplink bageo_ai_call_agent; do
  [ "$OTHER" = "$SELF_ID" ] && continue
  [ -d "/data/adb/modules/$OTHER" ] || continue
  touch "/data/adb/modules/$OTHER/remove"
  ui_print "- replacing $OTHER: removed at the next reboot"
done
