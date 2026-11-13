#!/system/bin/sh
# Late-start: roles + mute + arm service. Never abort boot; all ops best-effort.
#
# Phone mode, kept outside the module folder so it survives module updates:
#   /data/adb/pixelcallai/mode = "normal": an ordinary phone. Only grant the app's
#     permissions and start its service; leave the phone app, Google Phone,
#     Do Not Disturb and the volumes alone.
#   No file, or anything else: AI mode, the takeover below (how every phone ran
#     before the switch existed). apply_mode.sh writes the file when the app switches
#     (Myna > "AI answers my calls"); mode_daemon.sh runs it as root for the app.
MODDIR=${0%/*}
PKG=com.pixelcallai.streamer
GDIALER=com.google.android.dialer
MODE_FILE=/data/adb/pixelcallai/mode
# Before comparing, drop every blank character and a UTF-8 BOM (the app and the
# mode script use the same rule).
STRIP=' \t\r\n\v\f\357\273\277'

chmod 755 "$MODDIR/inject" 2>/dev/null || true
mkdir -p /data/local/tmp
touch /data/local/tmp/pixelcallai_uplink.pcm
chmod 666 /data/local/tmp/pixelcallai_uplink.pcm 2>/dev/null || true
# No mode switch can be running this early: drop a lock left by a reboot mid-switch,
# and the pid of the last boot's mode helper.
rm -rf /data/adb/pixelcallai/apply.lock 2>/dev/null || true
rm -f /data/adb/pixelcallai/daemon.pid 2>/dev/null || true

(
  i=0
  while [ "$(getprop sys.boot_completed)" != "1" ] && [ "$i" -lt 90 ]; do
    sleep 2
    i=$((i + 1))
  done
  sleep 10

  # Wait until PackageManager sees our priv-app before touching Dialer.
  j=0
  while [ "$j" -lt 30 ]; do
    if pm path "$PKG" 2>/dev/null | grep -q "priv-app.*${PKG}\|/${PKG}/"; then
      break
    fi
    sleep 2
    j=$((j + 1))
  done

  if ! pm path "$PKG" >/dev/null 2>&1; then
    log -t pixelcallai_uplink "service.sh: $PKG not installed — skip dialer takeover (safe)"
    exit 0
  fi

  # The app cannot use su itself (KernelSU only offers it to apps on its allow list):
  # this root helper runs its mode switches, in both modes. The system shell, not the
  # root manager's BusyBox, as for a root shell from adb.
  if command -v setsid >/dev/null 2>&1; then
    setsid /system/bin/sh "$MODDIR/mode_daemon.sh" </dev/null >/dev/null 2>&1 &
  else
    /system/bin/sh "$MODDIR/mode_daemon.sh" </dev/null >/dev/null 2>&1 &
  fi

  MODE=ai
  if [ -f "$MODE_FILE" ] && [ "$(tr -d "$STRIP" < "$MODE_FILE" 2>/dev/null)" = "normal" ]; then
    MODE=normal
  fi

  pm grant "$PKG" android.permission.RECORD_AUDIO >/dev/null 2>&1 || true
  pm grant "$PKG" android.permission.READ_PHONE_STATE >/dev/null 2>&1 || true
  pm grant "$PKG" android.permission.ANSWER_PHONE_CALLS >/dev/null 2>&1 || true
  pm grant "$PKG" android.permission.ACCESS_NOTIFICATION_POLICY >/dev/null 2>&1 || true
  cmd appops set "$PKG" ACCESS_NOTIFICATION_POLICY allow >/dev/null 2>&1 || true

  if [ "$MODE" = "normal" ]; then
    DEF=$(telecom get-default-dialer 2>/dev/null || true)
    log -t pixelcallai_uplink "service.sh: normal phone mode ($MODE_FILE) — no takeover"
  else
    cmd role add-role-holder android.app.role.DIALER "$PKG" >/dev/null 2>&1 || true
    cmd role add-role-holder android.app.role.CALL_SCREENING "$PKG" >/dev/null 2>&1 || true
    telecom set-default-dialer "$PKG" >/dev/null 2>&1 || true

    # Only disable Google Dialer after we are default dialer.
    DEF=$(telecom get-default-dialer 2>/dev/null || true)
    if [ "$DEF" = "$PKG" ]; then
      pm disable "$GDIALER/com.android.incallui.InCallServiceImpl" >/dev/null 2>&1 || true
      pm disable "$GDIALER/com.android.incallui.InCallActivity" >/dev/null 2>&1 || true
      pm disable-user --user 0 "$GDIALER" >/dev/null 2>&1 || true
    else
      log -t pixelcallai_uplink "service.sh: default dialer is '$DEF' — leave $GDIALER enabled"
    fi

    settings put global zen_mode 2 >/dev/null 2>&1 || true
    settings put system volume_ring 0 >/dev/null 2>&1 || true
    settings put system volume_notification 0 >/dev/null 2>&1 || true
  fi

  am start-foreground-service -n "$PKG/.CallAudioService" -a "$PKG.START" >/dev/null 2>&1 || true
  CLI=/data/adb/modules/zygisk_vector/cli
  if [ -x "$CLI" ]; then
    "$CLI" modules enable "$PKG" >/dev/null 2>&1 || true
    "$CLI" scope set "$PKG" com.android.phone/0 >/dev/null 2>&1 || true
  fi
  log -t pixelcallai_uplink "service.sh done mode=$MODE pkg=$PKG def=$DEF"
) &
