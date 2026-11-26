#!/system/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

# Late-start: roles + mute + arm service. Never abort boot; all ops best-effort.
#
# Phone mode, kept outside the module folder so it survives module updates:
#   /data/adb/pixelcallai/mode = "normal": an ordinary phone. Grant the app's
#     permissions and start its service; heal_mode.sh only puts Google Phone and the
#     phone app right when a cut-short switch left them wrong; Do Not Disturb and the
#     volumes are left alone.
#   No file, or anything else: AI mode, the takeover below (how every phone ran
#     before the switch existed). apply_mode.sh writes the file when the app switches
#     (Bageo > "AI answers my calls"); mode_daemon.sh runs it as root for the app.
MODDIR=${0%/*}
PKG=com.pixelcallai.streamer
GDIALER=com.google.android.dialer
MODE_FILE=/data/adb/pixelcallai/mode
# Before comparing, drop every blank character and a UTF-8 BOM (the app and the
# mode script use the same rule).
STRIP=' \t\r\n\v\f\357\273\277'

chmod 755 "$MODDIR/inject" 2>/dev/null || true
# Clean up any world-writable legacy uplink dump left by pre-1.0 versions.
rm -f /data/local/tmp/pixelcallai_uplink.pcm 2>/dev/null || true
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

  # A call served by the audio helper (bin/bageo-audio-helper) ended without its
  # clean-up (the phone restarted mid-call, or the helper was killed): put the mixer
  # and audio settings it changed back. On its own, with a time limit, so the rest of
  # this (the mode helper above all) never waits for it. Nothing else about the helper
  # runs at boot.
  if [ -f /data/adb/pixelcallai/audio_session ] && [ -f "$MODDIR/bin/bageo-audio-helper" ]; then
    chmod 0700 "$MODDIR/bin/bageo-audio-helper" 2>/dev/null || true
    if timeout -k 5 60 true >/dev/null 2>&1; then
      BAGEO_BOOT_RESTORE=1 timeout -k 5 60 "$MODDIR/bin/bageo-audio-helper" restore </dev/null >/dev/null 2>&1 &
    else
      (
        BAGEO_BOOT_RESTORE=1 "$MODDIR/bin/bageo-audio-helper" restore </dev/null >/dev/null 2>&1 &
        rpid=$!
        t=0
        while [ -d "/proc/$rpid" ] && ! grep -q '^State:.*[XZ]' "/proc/$rpid/status" 2>/dev/null && [ "$t" -lt 60 ]; do
          sleep 1
          t=$((t + 1))
        done
        if [ -d "/proc/$rpid" ] && ! grep -q '^State:.*[XZ]' "/proc/$rpid/status" 2>/dev/null; then
          kill -TERM "$rpid" 2>/dev/null || true
          sleep 5
          kill -KILL "$rpid" 2>/dev/null || true
        fi
        wait "$rpid" 2>/dev/null || true
      ) </dev/null >/dev/null 2>&1 &
    fi
  fi

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

  STANDALONE_MOD=0
  if [ -d "$MODDIR/system/priv-app/$PKG/bageo_assets" ] || [ -d "$MODDIR/bageo_assets" ]; then
    STANDALONE_MOD=1
  fi

  for p in RECORD_AUDIO READ_PHONE_STATE READ_PHONE_NUMBERS ANSWER_PHONE_CALLS ACCESS_NOTIFICATION_POLICY \
      POST_NOTIFICATIONS READ_CALL_LOG CALL_PHONE SEND_SMS; do
    pm grant "$PKG" "android.permission.$p" >/dev/null 2>&1 || true
  done
  if [ "$STANDALONE_MOD" -eq 1 ]; then
    pm grant "$PKG" android.permission.READ_CONTACTS >/dev/null 2>&1 || true
  else
    for p in RECEIVE_SMS READ_SMS; do
      pm grant "$PKG" "android.permission.$p" >/dev/null 2>&1 || true
    done
  fi
  cmd appops set "$PKG" ACCESS_NOTIFICATION_POLICY allow >/dev/null 2>&1 || true
  cmd appops set "$PKG" ANSWER_PHONE_CALLS allow >/dev/null 2>&1 || true
  cmd appops set "$PKG" USE_FULL_SCREEN_INTENT allow >/dev/null 2>&1 || true
  dumpsys deviceidle whitelist +"$PKG" >/dev/null 2>&1 || true

  if [ "$MODE" = "normal" ]; then
    DEF=$(telecom get-default-dialer 2>/dev/null || true)
    log -t pixelcallai_uplink "service.sh: normal phone mode ($MODE_FILE) — no takeover"
    # The mode file wins: undo what a cut-short switch left behind (Google Phone off,
    # Bageo still the phone app).
    /system/bin/sh "$MODDIR/heal_mode.sh" </dev/null >/dev/null 2>&1 || true
    DEF=$(telecom get-default-dialer 2>/dev/null || true)
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

    # Do Not Disturb on, as apply_mode.sh does: Android puts a bare zen_mode write back,
    # set_dnd is what turns it on. On standalone Daily Sound mode (no spare_mute flag),
    # keep DND and notification/media volumes untouched.
    if [ "$STANDALONE_MOD" -ne 1 ] || [ -f /data/adb/pixelcallai/spare_mute ]; then
      settings put global zen_mode 2 >/dev/null 2>&1 || true
      cmd notification set_dnd on >/dev/null 2>&1 || true
      echo 1 > /data/adb/pixelcallai/bageo_dnd 2>/dev/null || true
      settings put system volume_ring 0 >/dev/null 2>&1 || true
      settings put system volume_notification 0 >/dev/null 2>&1 || true
    fi
  fi

  if [ "$STANDALONE_MOD" -eq 0 ] || [ "$MODE" = "ai" ]; then
    am start-foreground-service -n "$PKG/.CallAudioService" -a "$PKG.START" >/dev/null 2>&1 || true
  fi
  CLI=/data/adb/modules/zygisk_vector/cli
  if [ "$STANDALONE_MOD" -eq 0 ] && [ -x "$CLI" ]; then
    "$CLI" modules enable "$PKG" >/dev/null 2>&1 || true
    "$CLI" scope set "$PKG" com.android.phone/0 >/dev/null 2>&1 || true
  fi
  log -t pixelcallai_uplink "service.sh done mode=$MODE pkg=$PKG def=$DEF"
) &
