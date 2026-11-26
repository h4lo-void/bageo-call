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
      audio)
        if [ "$2" = set-ringer-mode ] && [ "$3" = NORMAL ]; then echo 2 > "$ST/ringer"; fi ;;
    esac ;;
  settings)
    case "$1 $2 $3" in
      "get global zen_mode") rd zen ;;
      "get system volume_ring") rd vol_2 ;;
      "get system volume_notification") rd vol_5 ;;
      "get global mode_ringer") r=$(rd ringer); echo "${r:-2}" ;;
      "put global mode_ringer") echo "$4" > "$ST/ringer" ;;
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

# Standalone nodnd switch to normal clears Bageo's own DND and un-silences mode_ringer.
setup normal_nodnd_clears_bageo_dnd; ai_phone; echo 2 > "$ST/zen"; echo 0 > "$ST/ringer"; echo 1 > "$ST/data/bageo_dnd"
run_apply normal app-nodnd-daily-3 5 4 4 5 5 5
eq "exit" "$RC" 0; is_normal_phone; eq "Bageo DND cleared" "$(rd zen)" 0
eq "ringer restored" "$(rd ringer)" 2; eq "ring vol restored" "$(rd vol_2)" 5
eq "bageo_dnd flag removed" "$([ -e "$ST/data/bageo_dnd" ] && echo present || echo absent)" absent

# Standalone layout detection via system/priv-app/<pkg>/bageo_assets
setup standalone_privapp_assets; normal_phone; echo 0 > "$ST/zen"
mkdir -p "$ST/system/priv-app/$PKG/bageo_assets"
run_apply ai sa-mute-1
eq "exit" "$RC" 0; is_ai_phone
eq "READ_CONTACTS granted" "$(grep -c 'pm grant com.pixelcallai.streamer android.permission.READ_CONTACTS' "$ST/calls")" 1
eq "spare_mute set" "$(cat "$ST/data/spare_mute" 2>/dev/null | tr -d '\n')" 1
run_apply ai app-nodnd-sa-2
eq "exit" "$RC" 0; eq "spare_mute cleared on nodnd" "$([ -e "$ST/data/spare_mute" ] && echo present || echo absent)" absent

# Daily Sound turned on while the phone is the silent AI phone (Bageo's Do Not Disturb on),
# then a switch to a normal phone: Do Not Disturb must not stay on.
setup ai_nodnd_after_silent_ai; ai_phone; echo 0 > "$ST/ringer"; echo 1 > "$ST/data/bageo_dnd"
run_apply ai app-nodnd-daily-4 5 4 4 5 5 5
eq "exit" "$RC" 0; is_ai_phone; eq "Bageo DND cleared" "$(rd zen)" 0
eq "ringer back on" "$(rd ringer)" 2; eq "ring vol back" "$(rd vol_2)" 5; eq "notif vol back" "$(rd vol_5)" 4
eq "bageo_dnd flag removed" "$([ -e "$ST/data/bageo_dnd" ] && echo present || echo absent)" absent
run_apply normal app-nodnd-daily-5 5 4 4 5 5 5
eq "exit to normal" "$RC" 0; is_normal_phone; eq "DND off as a normal phone" "$(rd zen)" 0

# A Daily Sound switch to AI leaves the owner's own Do Not Disturb and volumes alone.
setup ai_nodnd_owner_dnd; normal_phone; echo 1 > "$ST/zen"
run_apply ai app-nodnd-daily-6 5 4 4 5 5 5
eq "exit" "$RC" 0; is_ai_phone; eq "owner DND untouched" "$(rd zen)" 1; eq "ring vol unchanged" "$(rd vol_2)" 4

# Bageo's Do Not Disturb will not go off on that switch: the phone stays an AI phone and
# keeps the marker, so the switch to a normal phone still turns it off later.
setup ai_nodnd_dnd_stuck; ai_phone; echo 1 > "$ST/data/bageo_dnd"
inject 'noop|set_dnd off'; inject 'noop|put global zen_mode 0'
run_apply ai app-nodnd-daily-7 5 4 4 5 5 5
eq "exit" "$RC" 1; eq "error" "$(last error)" dnd_failed; is_ai_phone
eq "bageo_dnd flag kept" "$([ -e "$ST/data/bageo_dnd" ] && echo present || echo absent)" present
rm -f "$ST/inject"
run_apply normal app-nodnd-daily-8 5 4 4 5 5 5
eq "exit to normal" "$RC" 0; is_normal_phone; eq "DND off as a normal phone" "$(rd zen)" 0

# ---------------------------------------------------------------- mode_daemon.sh
# apply_mode.sh run by hand (not through the helper): the helper reports the new mode,
# its log and pending within a few seconds, so the app does not keep an old mode and let
# calls ring unseen. The helper runs for real (2 s loop shortened), with toybox's stat -c
# and mv -T stood in for, and the app's folder and the package list in the case folder.
#
# As on a phone, the app's folder and root's folder have different encryption policies:
# the stand-in mv and ln refuse to rename or link between them ("Cross-device link") and
# note every try in $ST/crossed, which must stay empty.
mkdir "$TMP/dbin"
if [ -n "${BAGEO_FS_HELPER:-}" ] && [ -x "$BAGEO_FS_HELPER" ]; then
  FS_HELPER_BIN=$BAGEO_FS_HELPER
else
  CARGO_TARGET_DIR="$TMP/rh-target" cargo build --quiet --manifest-path "$HERE/../../roothelper/Cargo.toml" || exit 2
  FS_HELPER_BIN="$TMP/rh-target/debug/bageo-audio-helper"
fi
printf '#!/bin/sh\necho "$*" >> "${DAEMON_LOG:-/dev/null}"\nexit 0\n' > "$TMP/dbin/log"
printf '#!/bin/sh\nexit 0\n' > "$TMP/dbin/chcon"
cat > "$TMP/dbin/stat" <<'FAKE'
#!/bin/sh
# toybox stat -c with %u, %g, %u:%g and %s, read from ls; anything else is unknown.
[ "$1" = -c ] || exit 1
[ -e "$3" ] || [ -L "$3" ] || exit 1
set -- "$2" $(ls -lnd "$3")
case "$1" in
  %u) echo "$4" ;;
  %g) echo "$5" ;;
  %u:%g) echo "$4:$5" ;;
  %s) echo "$6" ;;
  *) exit 1 ;;
esac
FAKE
cat > "$TMP/dbin/crossing" <<'FAKE'
# Sourced by the stand-in mv and ln: fails (and notes it) when $1 and $2 lie on the two
# sides of the app folder / root folder line.
side() {
  d=$(cd "$(dirname "$1")" 2>/dev/null && pwd -P)
  case "$d" in "$FAKE_ROOT"|"$FAKE_ROOT"/*) echo root ;; "$FAKE_APP"|"$FAKE_APP"/*) echo app ;; *) echo other ;; esac
}
a=$(side "$1")
b=$(side "$2")
if { [ "$a" = root ] && [ "$b" = app ]; } || { [ "$a" = app ] && [ "$b" = root ]; }; then
  echo "$1 -> $2" >> "$FAKE_ST/crossed"
  echo "Cross-device link" >&2
  exit 1
fi
FAKE
cat > "$TMP/dbin/mv" <<'FAKE'
#!/bin/sh
# mv [-f|-fT] SRC DST. -T: DST is a name, never a folder to move into. A source whose
# name is in $FAKE_ST/mvfail cannot be moved.
t=
case "$1" in -fT) t=1; shift ;; -f) shift ;; esac
. "$(dirname "$0")/crossing"
if [ -f "$FAKE_ST/mvfail" ] && [ "$(basename "$1")" = "$(cat "$FAKE_ST/mvfail")" ]; then exit 1; fi
if [ -n "$t" ] && [ -d "$2" ] && [ ! -L "$2" ]; then echo "is a folder" >&2; exit 1; fi
exec /bin/mv -f "$1" "$2"
FAKE
cat > "$TMP/dbin/ln" <<'FAKE'
#!/bin/sh
case "$1" in -*) exec /bin/ln "$@" ;; esac
. "$(dirname "$0")/crossing"
exec /bin/ln "$1" "$2"
FAKE
chmod +x "$TMP/dbin/stat" "$TMP/dbin/mv" "$TMP/dbin/ln" "$TMP/dbin/log" "$TMP/dbin/chcon"
ME=$(id -u)
state_has() { grep -qx "$1" "$ST/app/files/mode/state" 2>/dev/null; }
wait_state() {
  n=0
  while [ "$n" -lt 50 ]; do
    state_has "$1" && return 0
    n=$((n + 1))
    sleep 0.1
  done
  return 1
}
# The package list: the app once, as $1 (debuggable 1 or not 0), owned by this user.
pkg_list() {
  echo "com.android.shell 2000 0 /data/user/0/com.android.shell platform:privapp none" > "$ST/packages.list"
  echo "$PKG $ME $1 /data/user/0/$PKG default:privapp:targetSdkVersion=34 3003 0 27 1 @system" >> "$ST/packages.list"
}
# The daemon for a case: its folders in the case folder (by their real path, as on a
# phone), the 2 s loop shortened, apply_mode.sh replaced by a stub that logs its run.
# Unless the case set one up, the app is a release build, as the module installs.
start_daemon() {
  mkdir -p "$ST/app/files/mode"
  [ -f "$ST/packages.list" ] || pkg_list 0
  real=$(cd "$ST" && pwd -P)
  printf '#!/bin/sh\necho "apply $*" >> "%s/order"\n' "$ST" > "$ST/apply_mode.sh"
  sed -e "s#/data/adb/pixelcallai#$ST/data#g" -e "s#^APPHOME=.*#APPHOME=$real/app#" \
    -e "s#^PKGLIST=.*#PKGLIST=$ST/packages.list#" \
    -e "s#^PATH=.*#PATH=$TMP/dbin:/usr/bin:/bin:/usr/sbin:/sbin#" -e 's#^  sleep 2$#  sleep 0.2#' \
    -e 's#/system/bin/sh "$MODDIR/apply_mode.sh"#sh "$MODDIR/apply_mode.sh"#' \
    "$MOD/mode_daemon.sh" > "$ST/mode_daemon.sh"
  FAKE_ROOT=$real/data FAKE_APP=$real/app DAEMON_LOG=$ST/daemon.log \
    BAGEO_FS_HELPER="$FS_HELPER_BIN" BAGEO_TEST_NO_SELINUX=1 BAGEO_TEST_FAIL_FILE="$ST/mvfail" \
    sh "$ST/mode_daemon.sh" </dev/null >/dev/null 2>&1 &
  DPID=$!
}
stop_daemon() {
  kill "$DPID" 2>/dev/null
  wait "$DPID" 2>/dev/null
  eq "nothing renamed or linked across the encryption line" "$(cat "$ST/crossed" 2>/dev/null)" ""
  eq "no folder of root's left in the app's folder" "$(ls -a "$ST/app/files/mode" 2>/dev/null | grep '^\.bageo-' | tr '\n' ' ')" ""
}
order_count() { c=$(grep -c "^$1" "$ST/order" 2>/dev/null); echo "${c:-0}"; }
# Waits until $1 is gone from the app's folder (taken), at most 5 s.
wait_gone() {
  n=0
  while { [ -e "$ST/app/files/mode/$1" ] || [ -L "$ST/app/files/mode/$1" ]; } && [ "$n" -lt 50 ]; do
    sleep 0.1
    n=$((n + 1))
  done
  [ ! -e "$ST/app/files/mode/$1" ] && [ ! -L "$ST/app/files/mode/$1" ]
}
wait_order() {
  n=0
  while [ "$(order_count "$1")" -lt "$2" ] && [ "$n" -lt 50 ]; do sleep 0.1; n=$((n + 1)); done
}

setup daemon_follows_hand_run
echo normal > "$ST/data/mode"
# A folder of root's an earlier run left (stopped half way) goes at the start.
mkdir -p "$ST/app/files/mode/.bageo-state.1.1"
: > "$ST/app/files/mode/.bageo-state.1.1/s"
start_daemon
wait_state mode=normal; eq "state at start" "$?" 0
echo ai > "$ST/data/mode"
printf 'id=byhand-1\nmode=ai\nresult=ok\ndone=1\n' > "$ST/data/last_apply"
wait_state mode=ai; eq "mode switched by hand reported" "$?" 0
state_has log.id=byhand-1; eq "its run reported" "$?" 0
: > "$ST/data/pending"
wait_state pending=1; eq "pending reported" "$?" 0
rm -f "$ST/data/pending"
wait_state pending=0; eq "pending gone reported" "$?" 0
echo normal > "$ST/data/mode"
wait_state mode=normal; eq "back to normal reported" "$?" 0
stop_daemon

# Switches with volumes, both ways, with the two folders on two encryption policies.
setup daemon_switches
start_daemon
wait_state daemon=1; eq "daemon up" "$?" 0
printf 'normal sw-n 4 3 4 5 5 5\n' > "$ST/app/files/mode/request"
wait_order apply 1
printf 'ai sw-a\n' > "$ST/app/files/mode/request"
wait_order apply 2
eq "both switches ran, volumes passed on" "$(cat "$ST/order" 2>/dev/null)" "apply normal sw-n 4 3 4 5 5 5
apply ai sw-a"
wait_state request=sw-a; eq "the last one reported" "$?" 0
wait_state exit=0; eq "its exit code reported" "$?" 0
stop_daemon

# The app asks for the audio helper: it starts once, a second request right after is
# turned away, a bad request starts nothing, and turned off or missing it says so.
setup daemon_audio_helper
mkdir -p "$ST/bin"
printf '#!/bin/sh\necho "helper $*" >> "%s/order"\n' "$ST" > "$ST/bin/bageo-audio-helper"
chmod 600 "$ST/bin/bageo-audio-helper"
start_daemon
wait_state daemon=1; eq "daemon up" "$?" 0
printf 'start call-1\n' > "$ST/app/files/mode/audio_helper"
wait_state audio_helper_result=started; eq "helper started" "$?" 0
state_has audio_helper=call-1; eq "its request reported" "$?" 0
wait_order helper 1
eq "helper ran once" "$(order_count helper)" 1
eq "helper asked to serve" "$(cat "$ST/order" 2>/dev/null)" "helper serve"
eq "request taken" "$([ -e "$ST/app/files/mode/audio_helper" ] && echo left || echo taken)" taken
printf 'start call-2\n' > "$ST/app/files/mode/audio_helper"
wait_state audio_helper_result=busy; eq "second start within 10 s turned away" "$?" 0
printf 'start ../../x\n' > "$ST/app/files/mode/audio_helper"
wait_state audio_helper_result=invalid_request; eq "bad id refused" "$?" 0
: > "$ST/data/audio_helper.disabled"
printf 'start call-3\n' > "$ST/app/files/mode/audio_helper"
wait_state audio_helper_result=disabled; eq "turned off reported" "$?" 0
rm -f "$ST/data/audio_helper.disabled" "$ST/bin/bageo-audio-helper"
printf 'start call-4\n' > "$ST/app/files/mode/audio_helper"
wait_state audio_helper_result=missing; eq "no helper reported" "$?" 0
state_has audio_helper=call-4; eq "that request reported" "$?" 0
printf 'start call-5' > "$ST/app/files/mode/audio_helper"
wait_state audio_helper_result=invalid_request; eq "no newline at the end: refused" "$?" 0
eq "nothing else ran" "$(order_count helper)" 1
stop_daemon

# A phone mode switch goes before the audio helper when both wait.
setup daemon_switch_first
mkdir -p "$ST/bin"
printf '#!/bin/sh\necho "helper $*" >> "%s/order"\n' "$ST" > "$ST/bin/bageo-audio-helper"
chmod 700 "$ST/bin/bageo-audio-helper"
mkdir -p "$ST/app/files/mode"
printf 'start call-1\n' > "$ST/app/files/mode/audio_helper"
printf 'ai sw-1\n' > "$ST/app/files/mode/request"
start_daemon
wait_state audio_helper_result=started; eq "helper started after" "$?" 0
wait_order helper 1
eq "switch ran first" "$(head -n 1 "$ST/order" 2>/dev/null)" "apply ai sw-1"
stop_daemon

# Only a request exactly as the app writes it is run: one line, a newline at the end,
# single spaces, not too long.
setup daemon_request_whole
start_daemon
wait_state daemon=1; eq "daemon up" "$?" 0
for bad in 'ai x1' 'ai  x2\n' ' ai x3\n' 'ai x4 \n' 'ai x5\nnormal x6\n' 'ai x7\n\n'; do
  printf "$bad" > "$ST/app/files/mode/request"
  wait_gone request; eq "malformed request taken: $bad" "$?" 0
  wait_state error=invalid_request; eq "malformed request refused: $bad" "$?" 0
done
long=$(printf 'normal %s' "$(printf 'x%.0s' $(seq 1 140))")
printf '%s\n' "$long" > "$ST/app/files/mode/request"
wait_gone request; eq "a request too large is taken" "$?" 0
wait_state error=invalid_request; eq "and refused" "$?" 0
eq "none of them ran" "$(order_count apply)" 0
printf 'ai ok-1 4 3\n' > "$ST/app/files/mode/request"
wait_order apply 1
eq "a whole request runs" "$(cat "$ST/order" 2>/dev/null)" "apply ai ok-1 4 3"
stop_daemon

# What cannot be read as a request holds up nothing: a pipe is dropped unread, a folder
# is handed back whole under another name, a link is removed without reading what it
# points to; the audio helper request and the state go on.
setup daemon_nothing_blocks
mkdir -p "$ST/bin"
printf '#!/bin/sh\necho "helper $*" >> "%s/order"\n' "$ST" > "$ST/bin/bageo-audio-helper"
chmod 700 "$ST/bin/bageo-audio-helper"
start_daemon
wait_state daemon=1; eq "daemon up" "$?" 0
mkfifo "$ST/app/files/mode/request"
wait_gone request; eq "a pipe is taken away" "$?" 0
wait_state error=invalid_request; eq "and refused" "$?" 0
mkdir -p "$ST/app/files/mode/request"
: > "$ST/app/files/mode/request/keep"
printf 'start call-9\n' > "$ST/app/files/mode/audio_helper"
wait_state audio_helper_result=started; eq "the helper request goes on" "$?" 0
eq "the folder is handed back whole" "$(ls "$ST"/app/files/mode/request.rejected.*/keep 2>/dev/null | wc -l | tr -d ' ')" 1
echo ai > "$ST/data/mode"
wait_state mode=ai; eq "the state goes on" "$?" 0
mkdir -p "$ST/secret"
printf 'normal evil-1\n' > "$ST/secret/req"
ln -s "$ST/secret/req" "$ST/app/files/mode/request"
wait_gone request; eq "a link is taken away" "$?" 0
eq "what it points to is left" "$(cat "$ST/secret/req")" "normal evil-1"
eq "nothing ran from any of them" "$(order_count apply)" 0
stop_daemon

# A request that cannot be taken: the state says so at once (the app does not just time
# out), it is left where it is, and the audio helper request still goes on. Once it can
# be taken it runs.
setup daemon_take_failed
mkdir -p "$ST/bin"
printf '#!/bin/sh\necho "helper $*" >> "%s/order"\n' "$ST" > "$ST/bin/bageo-audio-helper"
chmod 700 "$ST/bin/bageo-audio-helper"
echo request > "$ST/mvfail"
start_daemon
wait_state daemon=1; eq "daemon up" "$?" 0
printf 'ai stuck-1\n' > "$ST/app/files/mode/request"
wait_state error=take_failed; eq "a request that cannot be taken is reported" "$?" 0
eq "it is left where it is" "$(cat "$ST/app/files/mode/request" 2>/dev/null)" "ai stuck-1"
printf 'start call-7\n' > "$ST/app/files/mode/audio_helper"
wait_state audio_helper_result=started; eq "the helper request still goes on" "$?" 0
eq "said once" "$(grep -c 'cannot take the request' "$ST/daemon.log" 2>/dev/null)" 1
rm -f "$ST/mvfail"
wait_order apply 1
eq "once it can be taken it runs" "$(cat "$ST/order" 2>/dev/null | grep apply)" "apply ai stuck-1"
stop_daemon

# Whose requests run: only a release build that the package list shows plainly as the
# app that owns the folder, with a user ID of its own; the audio helper request too.
setup daemon_which_app
mkdir -p "$ST/bin"
printf '#!/bin/sh\necho "helper $*" >> "%s/order"\n' "$ST" > "$ST/bin/bageo-audio-helper"
chmod 700 "$ST/bin/bageo-audio-helper"
pkg_list 0
start_daemon
wait_state daemon=1; eq "daemon up" "$?" 0
printf 'ai rel-1\n' > "$ST/app/files/mode/request"
wait_order apply 1
eq "a release build switches" "$(order_count apply)" 1
refused() {
  printf 'ai %s\n' "$1" > "$ST/app/files/mode/request"
  wait_state "request=$1"; eq "$2: taken" "$?" 0
  state_has error=app_refused; eq "$2: refused" "$?" 0
}
pkg_list 1
refused dbg-1 "a debuggable build"
printf 'start call-8\n' > "$ST/app/files/mode/audio_helper"
wait_state audio_helper_result=app_refused; eq "a debuggable build's helper request is refused" "$?" 0
pkg_list 0
echo "com.example.shared $ME 0 /data/user/0/com.example.shared default" >> "$ST/packages.list"
refused shared-1 "a shared user ID"
pkg_list 0
echo "$PKG $ME 0 /data/user/0/$PKG x" >> "$ST/packages.list"
refused two-1 "listed twice"
pkg_list 0
sed "s#^$PKG $ME #$PKG $((ME + 1)) #" "$ST/packages.list" > "$ST/packages.list.new"
mv "$ST/packages.list.new" "$ST/packages.list"
refused uid-1 "another user ID"
pkg_list 0
sed "s#/data/user/0/$PKG #/data/data/x #" "$ST/packages.list" > "$ST/packages.list.new"
mv "$ST/packages.list.new" "$ST/packages.list"
refused dir-1 "another data folder"
rm -f "$ST/packages.list"
refused none-1 "no package list"
eq "none of those ran" "$(order_count apply)" 1
eq "no helper started" "$(order_count helper)" 0
stop_daemon

# The app swaps its folder for a link to a folder that is not its own: nothing there is
# taken, run, deleted or written.
setup daemon_swapped_folder
start_daemon
wait_state daemon=1; eq "daemon up" "$?" 0
mkdir -p "$ST/secret"
printf 'ai evil-1\n' > "$ST/secret/request"
printf 'start evil-2\n' > "$ST/secret/audio_helper"
rm -rf "$ST/app/files/mode"
ln -s "$ST/secret" "$ST/app/files/mode"
sleep 1
eq "other folder's request left alone" "$(cat "$ST/secret/request" 2>/dev/null)" "ai evil-1"
eq "other folder's helper request left alone" "$(cat "$ST/secret/audio_helper" 2>/dev/null)" "start evil-2"
eq "no state written there" "$([ -e "$ST/secret/state" ] && echo written || echo none)" none
eq "no folder made there" "$(ls -a "$ST/secret" | grep -c '^\.bageo-')" 0
eq "nothing ran" "$(order_count apply)" 0
stop_daemon

# A symlink to a folder at `state` is replaced without touching that folder, and a
# blocked publication retries even when mode/pending/last_apply did not change.
setup daemon_state_link_and_retry
mkdir -p "$ST/bin" "$ST/elsewhere"
printf '#!/bin/sh\necho "helper $*" >> "%s/order"\n' "$ST" > "$ST/bin/bageo-audio-helper"
chmod 700 "$ST/bin/bageo-audio-helper"
start_daemon
wait_state daemon=1; eq "daemon up" "$?" 0
cp "$ST/app/files/mode/state" "$ST/old_state"
rm -f "$ST/app/files/mode/state"
ln -s "$ST/elsewhere" "$ST/app/files/mode/state"
wait_state daemon=1; eq "state published over dir symlink" "$?" 0
eq "state is a plain file" "$([ ! -L "$ST/app/files/mode/state" ] && [ -f "$ST/app/files/mode/state" ] && echo yes || echo no)" yes
eq "target folder untouched" "$(ls -A "$ST/elsewhere")" ""
# Block publication while an invalid request is processed, then restore old state file;
# daemon must still publish error=invalid_request on the next round.
chmod 500 "$ST/app/files/mode"
chmod 700 "$ST/app/files/mode"
printf 'bad-line\n' > "$ST/app/files/mode/request"
wait_state error=invalid_request; eq "invalid_request published" "$?" 0
cp "$ST/old_state" "$ST/app/files/mode/state"
# Force a fresh helper result while blocking publish via missing executable, then restore.
chmod 600 "$FS_HELPER_BIN" 2>/dev/null || true
chmod 700 "$FS_HELPER_BIN" 2>/dev/null || true
printf 'start retry-1\n' > "$ST/app/files/mode/audio_helper"
wait_state audio_helper=retry-1; eq "helper result published over stale state" "$?" 0
stop_daemon

echo "checks passed: $PASS, failed: $FAILS"
[ "$FAILS" -eq 0 ]
