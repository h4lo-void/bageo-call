#!/system/bin/sh
# Safe flash checklist:
# 1) Uninstall user /data/app com.pixelcallai.streamer FIRST (never dual-install).
# 2) magisk --install-module this zip → reboot.
# 3) Rollback if boot fails: Magisk Safe Mode (vol key), or from adb/recovery:
#      touch /data/adb/modules/pixelcallai_uplink/disable && reboot
#      OR touch /data/adb/modules/pixelcallai_uplink/remove && reboot

ui_print "- pixelcallai_uplink: priv-app + telephony uplink permissions"

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
  ui_print "! WARNING: privapp-permissions XML missing"
fi

ui_print "- priv-app APK: ${APK_SIZE} bytes"
ui_print "- rollback: touch \$MODDIR/disable && reboot"
