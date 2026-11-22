#!/system/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

# Bageo: at boot, in normal phone mode, make the phone match the mode file.
#
# A switch that was cut short (or an older version) can leave a normal phone with Google
# Phone turned off, or with Bageo still the phone app and call screener. The mode file
# says normal, so Google Phone is turned on again and the phone app and screening go
# back to the system. Do Not Disturb and the volumes are left alone.
# Run by service.sh before the app starts. Best effort: never stops the boot.
DIR=/data/adb/pixelcallai
PKG=com.pixelcallai.streamer
GD=com.google.android.dialer
STRIP=' \t\r\n\v\f\357\273\277'

# A switch from the app is running: it ends in a consistent state itself.
[ -d "$DIR/apply.lock" ] && exit 0

if command -v timeout >/dev/null 2>&1; then T="timeout 15"; else T=""; fi
q() { $T "$@" >/dev/null 2>&1; }
o() { $T "$@" 2>/dev/null; }
say() { log -t pixelcallai_uplink "heal_mode.sh: $1" 2>/dev/null || true; }

# Only a normal phone is put right here; the AI phone's takeover is done by service.sh.
[ "$(tr -d "$STRIP" < "$DIR/mode" 2>/dev/null)" = normal ] || exit 0

if o pm list packages | grep -qx "package:$GD"; then
  if o pm list packages -d | grep -qx "package:$GD"; then
    say "Google Phone was off; turning it on"
    q pm enable "$GD"
    q pm enable "$GD/com.android.incallui.InCallServiceImpl"
    q pm enable "$GD/com.android.incallui.InCallActivity"
    q pm enable "$GD/com.android.incallui.LegacyInCallActivity"
  fi
  google=1
else
  google=0
fi

if o cmd role get-role-holders android.app.role.CALL_SCREENING | grep -qF "$PKG"; then
  say "Bageo was still the call screener; letting go"
  q cmd role remove-role-holder android.app.role.CALL_SCREENING "$PKG" 1 ||
    q cmd role remove-role-holder android.app.role.CALL_SCREENING "$PKG"
fi

d=$(o telecom get-default-dialer | tr -d "$STRIP")
if [ "$d" = "$PKG" ]; then
  say "Bageo was still the phone app; handing it back"
  if [ "$google" -eq 1 ]; then
    q cmd role add-role-holder android.app.role.DIALER "$GD" 1 ||
      q cmd role add-role-holder android.app.role.DIALER "$GD"
    q telecom set-default-dialer "$GD"
  else
    q cmd role remove-role-holder android.app.role.DIALER "$PKG" 1 ||
      q cmd role remove-role-holder android.app.role.DIALER "$PKG"
  fi
fi
exit 0
