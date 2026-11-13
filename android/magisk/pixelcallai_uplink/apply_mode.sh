#!/system/bin/sh
# Myna: make the phone an AI answering phone ("ai") or an ordinary phone ("normal").
#
#   sh apply_mode.sh <ai|normal> <id> [ring notification voice_call music alarm system]
#
# Called as root by mode_daemon.sh (in this module folder) when the app asks for a
# switch. It runs outside the app, so it finishes even when Android stops the app on
# the way: taking the phone-app role away from Myna can do that, which is why that
# step comes last. The numbers are the volumes to set in normal mode (worked out by
# the app; 0 = leave that one alone).
#
#   /data/adb/pixelcallai/mode        the mode, written first; service.sh reads it at boot
#   /data/adb/pixelcallai/pending     exists until every step has worked; the app asks
#                                     for the switch again when its service starts and
#                                     finds it
#   /data/adb/pixelcallai/last_apply  key=value lines: id, mode, step_<name>=ok|fail,
#                                     then error=<code> or result=ok, and done=1 last
#   /data/adb/pixelcallai/apply.lock  held while a run is going (its pid inside)
trap '' HUP

DIR=/data/adb/pixelcallai
PKG=com.pixelcallai.streamer
GD=com.google.android.dialer
LOG=$DIR/last_apply
LOCK=$DIR/apply.lock
# Same rule as service.sh and the app: drop every blank character and a UTF-8 BOM.
STRIP=' \t\r\n\v\f\357\273\277'

MODE=$1
ID=$2
case "$MODE" in ai|normal) ;; *) echo "usage: $0 ai|normal <id> [volumes]" >&2; exit 2 ;; esac
case "$ID" in ''|*[!A-Za-z0-9-]*) ID=invalid ;; esac

mkdir -p "$DIR" || exit 3
if ! mkdir "$LOCK" 2>/dev/null; then
  OLD=$(cat "$LOCK/pid" 2>/dev/null)
  # Still held only when that pid is alive and really is this script (after a reboot
  # or a SIGKILL the pid may belong to something else; service.sh also clears the
  # lock at boot).
  if [ -n "$OLD" ] && kill -0 "$OLD" 2>/dev/null && grep -q apply_mode "/proc/$OLD/cmdline" 2>/dev/null; then
    exit 4
  fi
  rm -rf "$LOCK"
  mkdir "$LOCK" || exit 4
fi
echo $$ > "$LOCK/pid"
trap 'rm -rf "$LOCK"' EXIT

# Stay out of any app's cgroup: Android kills the whole cgroup when it stops an app.
# Started by mode_daemon.sh this changes nothing. Best effort; ignored where not allowed.
for cg in /sys/fs/cgroup/cgroup.procs /dev/cpuctl/cgroup.procs /dev/cpuset/cgroup.procs; do
  [ -w "$cg" ] && echo $$ > "$cg" 2>/dev/null
done

if command -v timeout >/dev/null 2>&1; then T="timeout 15"; else T=""; fi
# q: run quietly with a time limit; o: same, keep stdout.
q() { $T "$@" >/dev/null 2>&1; }
o() { $T "$@" 2>/dev/null; }
note() { echo "$1" >> "$LOG"; }
step() { note "step_$1=$2"; }
fail() { step "$1" fail; note "error=$2"; note "finished=$(date +%s)"; note "done=1"; exit 1; }
# Retry a check for about 8 seconds (roles and settings take a moment).
until_ok() {
  n=0
  while [ "$n" -lt 20 ]; do
    "$@" && return 0
    sleep 0.4
    n=$((n + 1))
  done
  return 1
}
dialer() { o telecom get-default-dialer | tr -d "$STRIP"; }
zen() { o settings get global zen_mode | tr -d "$STRIP"; }
remove_holder() { q cmd role remove-role-holder "$1" "$2" 1 || q cmd role remove-role-holder "$1" "$2"; }
add_holder() { q cmd role add-role-holder "$1" "$2" 1 || q cmd role add-role-holder "$1" "$2"; }
# 0: PKG holds the role, 1: it does not, 2: cannot tell (command missing).
holds() {
  h=$(o cmd role get-role-holders "$1") || return 2
  case "$h" in *"$2"*) return 0 ;; *) return 1 ;; esac
}

: > "$LOG"
note "id=$ID"
note "mode=$MODE"
note "started=$(date +%s)"
echo "$MODE" > "$DIR/pending"
echo "$MODE" > "$DIR/mode"
if [ "$(tr -d "$STRIP" < "$DIR/mode")" = "$MODE" ]; then step save ok; else fail save save_failed; fi

# ---------------------------------------------------------------- normal phone
set_volume() { # stream index setting
  case "$2" in ''|*[!0-9]*) return 0 ;; esac
  [ "$2" -gt 0 ] || return 0
  q cmd media_session volume --stream "$1" --set "$2" || q cmd audio set-volume "$1" "$2"
  [ -n "$3" ] && q settings put system "$3" "$2"
  return 0
}
ring_on() {
  v=$(o cmd media_session volume --stream 2 --get | sed -n 's/.*volume is \([0-9][0-9]*\).*/\1/p' | head -n 1)
  if [ -n "$v" ]; then
    [ "$v" -gt 0 ]
    return
  fi
  [ "$(o settings get global mode_ringer | tr -d "$STRIP")" = "2" ]
}
dnd_off() { [ "$(zen)" = "0" ]; }
google_ready() {
  [ "$(dialer)" = "$GD" ] || return 1
  o pm list packages -d | grep -qx "package:$GD" && return 1
  return 0
}
other_ready() {
  d=$(dialer)
  [ -n "$d" ] && [ "$d" != "$PKG" ] && [ "$d" != "null" ]
}
# The phone app step failed: never leave the phone without a phone app. Go back to AI
# mode (Myna is the phone app again and answers), then report the failure.
dialer_fail() {
  step dialer fail
  note "revert=ai"
  echo ai > "$DIR/mode"
  apply_ai
  rm -f "$DIR/pending"
  note "error=dialer_failed"
  note "finished=$(date +%s)"
  note "done=1"
  exit 1
}

apply_normal() {
  # 1. Myna no longer screens (silences) incoming calls.
  remove_holder android.app.role.CALL_SCREENING "$PKG"
  holds android.app.role.CALL_SCREENING "$PKG"
  [ $? -eq 0 ] && fail screening screening_failed
  step screening ok

  # 2. Do Not Disturb off.
  q cmd notification set_dnd off
  q settings put global zen_mode 0
  until_ok dnd_off || fail dnd dnd_failed
  step dnd ok

  # 3. Ringer on, volumes back (ring, notification, earpiece, media, alarm, system).
  q cmd audio set-ringer-mode NORMAL
  q settings put global mode_ringer 2
  set_volume 2 "$3" volume_ring
  set_volume 5 "$4" volume_notification
  set_volume 0 "$5" ""
  set_volume 3 "$6" volume_music
  set_volume 4 "$7" volume_alarm
  set_volume 1 "$8" volume_system
  until_ok ring_on || fail volume volume_failed
  step volume ok

  # 4. Undo the AI mode's root microphone mutes (controls a phone lacks are ignored).
  q cmd media.audio_flinger mute mic 0
  for c in 'Mic Mute' 'Microphone Mute' 'ADC1 Mute' 'ADC2 Mute' 'IN1 Mute' 'IN2 Mute'; do
    q tinymix "$c" 0
  done
  step mic ok

  # 5. Last, because Android may stop Myna when it loses the role: the phone app.
  #    The new phone app takes the role over before Myna lets go, so the phone is
  #    never without one; on any failure dialer_fail puts Myna back.
  pkgs=$(o pm list packages)
  [ -n "$pkgs" ] || dialer_fail
  if echo "$pkgs" | grep -qx "package:$GD"; then
    ok=1
    q pm enable "$GD" || ok=0
    q pm enable "$GD/com.android.incallui.InCallServiceImpl" || ok=0
    # Only in some Google Phone versions (current ones have neither): best effort.
    q pm enable "$GD/com.android.incallui.InCallActivity"
    q pm enable "$GD/com.android.incallui.LegacyInCallActivity"
    [ "$ok" = 1 ] || dialer_fail
    o pm list packages -d | grep -qx "package:$GD" && dialer_fail
    # The DIALER role has one holder: adding Google Phone replaces Myna.
    add_holder android.app.role.DIALER "$GD"
    q telecom set-default-dialer "$GD"
    until_ok google_ready || dialer_fail
  else
    # No Google Phone: let go, and Android falls back to the phone's own phone app.
    remove_holder android.app.role.DIALER "$PKG"
    until_ok other_ready || dialer_fail
  fi
  step dialer ok
}

# ---------------------------------------------------------------- AI phone
myna_dialer() { [ "$(dialer)" = "$PKG" ]; }
dnd_on() { z=$(zen); [ -n "$z" ] && [ "$z" != "0" ]; }

apply_ai() {
  for p in RECORD_AUDIO READ_PHONE_STATE ANSWER_PHONE_CALLS ACCESS_NOTIFICATION_POLICY \
      POST_NOTIFICATIONS READ_CALL_LOG CALL_PHONE SEND_SMS; do
    q pm grant "$PKG" "android.permission.$p"
  done
  q cmd appops set "$PKG" ACCESS_NOTIFICATION_POLICY allow

  # 1. Myna is the phone app and the call screener (as service.sh does at boot).
  q cmd role add-role-holder android.app.role.DIALER "$PKG"
  q cmd role add-role-holder android.app.role.CALL_SCREENING "$PKG"
  q telecom set-default-dialer "$PKG"
  until_ok myna_dialer || fail dialer dialer_failed
  step dialer ok
  holds android.app.role.CALL_SCREENING "$PKG"
  [ $? -eq 1 ] && fail screening screening_failed
  step screening ok

  # 2. Only now that Myna answers calls: Google Phone off.
  q pm disable "$GD/com.android.incallui.InCallServiceImpl"
  q pm disable "$GD/com.android.incallui.InCallActivity"
  q pm disable-user --user 0 "$GD"
  step google ok

  # 3. Do Not Disturb on, ringer and notification volume settings zero (the app's
  #    audio guard zeroes the live volumes when its service arms).
  q settings put global zen_mode 2
  q cmd notification set_dnd on
  until_ok dnd_on || fail dnd dnd_failed
  step dnd ok
  q settings put system volume_ring 0
  q settings put system volume_notification 0
  step volume ok
}

if [ "$MODE" = normal ]; then apply_normal "$@"; else apply_ai; fi
rm -f "$DIR/pending"
note "result=ok"
note "finished=$(date +%s)"
note "done=1"
exit 0
