#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

# Runs apply_mode.sh and heal_mode.sh on a computer, with the phone's commands (cmd, pm,
# settings, telecom ...) replaced by a small fake phone, and checks that every run, failed
# or not, ends with the phone in the state its mode file names.
#
#   sh android/magisk/tests/test_mode_scripts.sh
#
# The scripts are copied first with the phone's /data/adb/pixelcallai folder pointed at a
# temporary one; nothing on this computer is touched. Not part of the module zip.
HERE=$(cd "$(dirname "$0")" && pwd)
MOD=$HERE/../pixelcallai_uplink
TMP=$(mktemp -d "${TMPDIR:-/tmp}/bageo-mode-test.XXXXXX") || exit 2
trap 'rm -rf "$TMP"' EXIT
export LC_ALL=C
PKG=com.pixelcallai.streamer
GD=com.google.android.dialer

# ---------------------------------------------------------------- the fake phone
# State lives in files under $FAKE_ST: gd_enabled, dialer, screening, zen, vol_2 (ring),
# vol_5 (notification), no_google. Every call is logged to $FAKE_ST/calls. A line
# "fail|<text>" or "noop|<text>" in $FAKE_ST/inject makes any command containing <text>
# fail, or succeed without doing anything.
cat > "$TMP/fake.sh" <<'FAKE'
name=$1; shift
ST=$FAKE_ST
GD=com.google.android.dialer
echo "$name $*" >> "$ST/calls"
full="$name $*"
if [ -f "$ST/inject" ]; then
  while IFS='|' read -r kind pat; do
    [ -n "$kind" ] || continue
    case "$full" in
      *"$pat"*) if [ "$kind" = fail ]; then exit 1; fi; exit 0 ;;
    esac
  done < "$ST/inject"
fi
rd() { cat "$ST/$1" 2>/dev/null; }
gd_ok() { [ ! -f "$ST/no_google" ] && [ "$(rd gd_enabled)" = 1 ]; }
fallback() { if gd_ok; then echo "$GD"; else echo com.android.dialer; fi; }
case "$name" in
  cmd)
    case "$1" in
      role)
        act=$2; role=$3; pkg=$4
        case "$act" in
          add-role-holder)
            if [ "$role" = android.app.role.DIALER ]; then
              if [ "$pkg" = "$GD" ] && ! gd_ok; then exit 1; fi
              echo "$pkg" > "$ST/dialer"
            else
              echo "$pkg" > "$ST/screening"
            fi ;;
          remove-role-holder)
            if [ "$role" = android.app.role.DIALER ]; then
              if [ "$(rd dialer)" = "$pkg" ]; then fallback > "$ST/dialer"; fi
            else
              if [ "$(rd screening)" = "$pkg" ]; then : > "$ST/screening"; fi
            fi ;;
          get-role-holders)
            if [ "$role" = android.app.role.DIALER ]; then rd dialer; else rd screening; fi ;;
        esac ;;
      notification)
        if [ "$2" = set_dnd ]; then
          if [ "$3" = on ]; then echo 2 > "$ST/zen"; else echo 0 > "$ST/zen"; fi
        fi ;;
      media_session)
        if [ "$4" = --get ]; then echo "volume is $(rd vol_$3)"; else echo "$5" > "$ST/vol_$3"; fi ;;
    esac ;;
  settings)
    case "$1 $2 $3" in
      "get global zen_mode") rd zen ;;
      "get system volume_ring") rd vol_2 ;;
      "get system volume_notification") rd vol_5 ;;
      "get global mode_ringer") echo 2 ;;
      "put global zen_mode") echo "$4" > "$ST/zen" ;;
      "put system volume_ring") echo "$4" > "$ST/vol_2" ;;
      "put system volume_notification") echo "$4" > "$ST/vol_5" ;;
    esac ;;
  telecom)
    case "$1" in
      get-default-dialer) rd dialer ;;
      set-default-dialer)
        if [ "$2" = "$GD" ] && ! gd_ok; then exit 1; fi
        echo "$2" > "$ST/dialer" ;;
    esac ;;
  pm)
    case "$1" in
      list)
        if [ "$3" = -d ]; then
          if [ ! -f "$ST/no_google" ] && [ "$(rd gd_enabled)" != 1 ]; then echo "package:$GD"; fi
        else
          echo "package:com.pixelcallai.streamer"
          if [ ! -f "$ST/no_google" ]; then echo "package:$GD"; fi
        fi ;;
      enable) if [ "$2" = "$GD" ]; then echo 1 > "$ST/gd_enabled"; fi ;;
      disable-user) if [ "$4" = "$GD" ]; then echo 0 > "$ST/gd_enabled"; fi ;;
    esac ;;
esac
exit 0
FAKE
mkdir "$TMP/bin"
for n in cmd settings telecom pm dumpsys tinymix log; do
  printf '#!/bin/sh\nexec sh "%s" %s "$@"\n' "$TMP/fake.sh" "$n" > "$TMP/bin/$n"
  chmod +x "$TMP/bin/$n"
done
printf '#!/bin/sh\nexit 0\n' > "$TMP/bin/sleep"
chmod +x "$TMP/bin/sleep"

# ---------------------------------------------------------------- helpers
PASS=0
FAILS=0
CASE=
bad() { FAILS=$((FAILS + 1)); echo "FAIL [$CASE] $1"; }
eq() { if [ "$2" = "$3" ]; then PASS=$((PASS + 1)); else bad "$1: expected '$3', got '$2'"; fi; }
rd() { cat "$ST/$1" 2>/dev/null | tr -d '\n'; }
mode_file() { cat "$ST/data/mode" 2>/dev/null | tr -d '\n'; }
pending() { if [ -e "$ST/data/pending" ]; then echo yes; else echo no; fi; }
last() { sed -n "s/^$1=//p" "$ST/data/last_apply" 2>/dev/null | tail -n 1; }
calls_changing() { grep -Ec '^(pm enable|pm disable|cmd role (add|remove)|telecom set|cmd notification|settings put)' "$ST/calls"; }

setup() {
  CASE=$1
  ST=$TMP/case-$1
  rm -rf "$ST"
  mkdir -p "$ST/data"
  export FAKE_ST=$ST
  : > "$ST/calls"
  sed "s#/data/adb/pixelcallai#$ST/data#g" "$MOD/apply_mode.sh" > "$ST/apply_mode.sh"
  if [ -f "$MOD/heal_mode.sh" ]; then
    sed "s#/data/adb/pixelcallai#$ST/data#g" "$MOD/heal_mode.sh" > "$ST/heal_mode.sh"
  fi
}
# An ordinary phone: Google Phone is the phone app, calls ring.
normal_phone() {
  echo 1 > "$ST/gd_enabled"; echo $GD > "$ST/dialer"; : > "$ST/screening"
  echo 0 > "$ST/zen"; echo 4 > "$ST/vol_2"; echo 3 > "$ST/vol_5"
  echo normal > "$ST/data/mode"
}
# The AI phone: Bageo is the phone app and screens, Google Phone off, Do Not Disturb on.
ai_phone() {
  echo 0 > "$ST/gd_enabled"; echo $PKG > "$ST/dialer"; echo $PKG > "$ST/screening"
  echo 2 > "$ST/zen"; echo 0 > "$ST/vol_2"; echo 0 > "$ST/vol_5"
  echo ai > "$ST/data/mode"
}
inject() { echo "$1" >> "$ST/inject"; }
run_apply() { PATH="$TMP/bin:$PATH" sh "$ST/apply_mode.sh" "$@" >/dev/null 2>&1; RC=$?; }
run_heal() { PATH="$TMP/bin:$PATH" sh "$ST/heal_mode.sh" >/dev/null 2>&1; RC=$?; }

# The phone matches its mode file.
is_normal_phone() {
  eq "mode file" "$(mode_file)" normal
  eq "Google Phone on" "$(rd gd_enabled)" 1
  eq "phone app" "$(rd dialer)" "$GD"
  eq "Bageo not screening" "$(rd screening)" ""
}
is_ai_phone() {
  eq "mode file" "$(mode_file)" ai
  eq "Google Phone off" "$(rd gd_enabled)" 0
  eq "phone app" "$(rd dialer)" "$PKG"
}

# ---------------------------------------------------------------- switch to AI
setup ai_ok; normal_phone
run_apply ai t1
eq "exit" "$RC" 0; is_ai_phone; eq "pending" "$(pending)" no; eq "DND on" "$(rd zen)" 2

# Do Not Disturb will not turn on, after Bageo took the phone app and Google Phone went off.
setup ai_dnd_stuck; normal_phone
inject 'noop|set_dnd on'; inject 'noop|put global zen_mode 2'
run_apply ai t2
eq "exit" "$RC" 1; eq "error" "$(last error)" dnd_failed
is_normal_phone; eq "pending" "$(pending)" no; eq "ring volume back" "$(rd vol_2)" 4; eq "DND still off" "$(rd zen)" 0
eq "reverted" "$(last revert_ok)" 1

# The call screener step fails.
setup ai_screening_fails; normal_phone
inject 'noop|add-role-holder android.app.role.CALL_SCREENING'
run_apply ai t3
eq "exit" "$RC" 1; eq "error" "$(last error)" screening_failed
is_normal_phone; eq "pending" "$(pending)" no

# Bageo cannot become the phone app.
setup ai_dialer_fails; normal_phone
inject 'fail|add-role-holder android.app.role.DIALER com.pixelcallai'
inject 'noop|set-default-dialer com.pixelcallai'
run_apply ai t4
eq "exit" "$RC" 1; eq "error" "$(last error)" dialer_failed
is_normal_phone; eq "pending" "$(pending)" no

# The way back also fails (Google Phone will not take the phone app): the file still says
# normal, the switch stays pending, and the next boot's heal_mode.sh puts it right.
setup ai_revert_fails; normal_phone
inject 'noop|set_dnd on'; inject 'noop|put global zen_mode 2'
inject 'noop|add-role-holder android.app.role.DIALER com.google'; inject 'noop|set-default-dialer com.google'
run_apply ai t5
eq "exit" "$RC" 1; eq "mode file" "$(mode_file)" normal; eq "pending" "$(pending)" yes; eq "reverted" "$(last revert_ok)" 0
eq "Google Phone on" "$(rd gd_enabled)" 1
rm -f "$ST/inject"
run_heal
eq "heal exit" "$RC" 0; is_normal_phone

# An AI phone that is re-applied and fails stays an AI phone (nothing to put back to).
setup ai_again_fails; ai_phone; : > "$ST/screening"
inject 'noop|add-role-holder android.app.role.CALL_SCREENING'
run_apply ai t6
eq "exit" "$RC" 1; eq "mode file" "$(mode_file)" ai; eq "pending" "$(pending)" yes
eq "Google Phone off" "$(rd gd_enabled)" 0

# The owner's own Do Not Disturb was on, and the switch breaks at the very end (the mode
# file cannot be saved): the way back leaves Do Not Disturb as the owner had it.
# (A read-only file only stops a user other than root.)
if [ "$(id -u)" != 0 ]; then
  setup ai_owner_dnd_kept; normal_phone; echo 1 > "$ST/zen"; chmod 444 "$ST/data/mode"
  run_apply ai t10
  eq "exit" "$RC" 1; eq "error" "$(last error)" save_failed
  eq "owner's Do Not Disturb kept" "$(rd zen)" 1; eq "Google Phone on" "$(rd gd_enabled)" 1
  eq "phone app" "$(rd dialer)" "$GD"
  chmod 644 "$ST/data/mode"
fi

# ---------------------------------------------------------------- switch to normal
setup normal_ok; ai_phone
run_apply normal t7 4 3 4 5 5 5
eq "exit" "$RC" 0; is_normal_phone; eq "pending" "$(pending)" no; eq "DND off" "$(rd zen)" 0; eq "ring" "$(rd vol_2)" 4

setup normal_dnd_stuck; ai_phone
inject 'noop|set_dnd off'; inject 'noop|put global zen_mode 0'
run_apply normal t8 4 3 4 5 5 5
eq "exit" "$RC" 1; eq "error" "$(last error)" dnd_failed; is_ai_phone; eq "pending" "$(pending)" no

setup normal_dialer_fails; ai_phone
inject 'noop|add-role-holder android.app.role.DIALER com.google'; inject 'noop|set-default-dialer com.google'
run_apply normal t9 4 3 4 5 5 5
eq "exit" "$RC" 1; eq "error" "$(last error)" dialer_failed; is_ai_phone; eq "pending" "$(pending)" no

# ---------------------------------------------------------------- boot: heal_mode.sh
setup heal_fixes; ai_phone; echo normal > "$ST/data/mode"; echo 2 > "$ST/zen"; echo 4 > "$ST/vol_2"
run_heal
eq "exit" "$RC" 0; eq "Google Phone on" "$(rd gd_enabled)" 1; eq "phone app" "$(rd dialer)" "$GD"
eq "not screening" "$(rd screening)" ""; eq "owner's Do Not Disturb kept" "$(rd zen)" 2; eq "volume kept" "$(rd vol_2)" 4

setup heal_nothing_to_do; normal_phone
run_heal
eq "exit" "$RC" 0; eq "changes" "$(calls_changing)" 0

setup heal_ai_untouched; ai_phone
run_heal
eq "exit" "$RC" 0; is_ai_phone; eq "changes" "$(calls_changing)" 0

setup heal_no_mode_file; ai_phone; rm -f "$ST/data/mode"
run_heal
eq "exit" "$RC" 0; eq "Google Phone off" "$(rd gd_enabled)" 0

setup heal_switch_running; ai_phone; echo normal > "$ST/data/mode"; mkdir "$ST/data/apply.lock"
run_heal
eq "exit" "$RC" 0; eq "Google Phone off" "$(rd gd_enabled)" 0

# heal_mode.sh holds the switch lock while it works and lets it go when done.
setup heal_lock_released; ai_phone; echo normal > "$ST/data/mode"
run_heal
eq "exit" "$RC" 0; eq "Google Phone on" "$(rd gd_enabled)" 1
eq "lock let go" "$([ -e "$ST/data/apply.lock" ] && echo held || echo free)" free

setup heal_no_google; ai_phone; echo normal > "$ST/data/mode"; touch "$ST/no_google"
run_heal
eq "exit" "$RC" 0; eq "phone app" "$([ "$(rd dialer)" = "$PKG" ] && echo bageo || echo system)" system

# Standalone Daily Sound mode (app-nodnd-*): switches dialer without touching zen_mode or volumes.
setup ai_nodnd; normal_phone; echo 0 > "$ST/zen"; echo 5 > "$ST/vol_2"; echo 6 > "$ST/vol_5"
run_apply ai app-nodnd-daily-1
eq "exit" "$RC" 0; is_ai_phone; eq "DND unchanged" "$(rd zen)" 0
eq "ring vol unchanged" "$(rd vol_2)" 5; eq "notif vol unchanged" "$(rd vol_5)" 6

setup normal_nodnd; ai_phone; echo 2 > "$ST/zen"; echo 3 > "$ST/vol_2"
run_apply normal app-nodnd-daily-2
eq "exit" "$RC" 0; is_normal_phone; eq "owner DND untouched" "$(rd zen)" 2

echo "checks passed: $PASS, failed: $FAILS"
[ "$FAILS" -eq 0 ]
