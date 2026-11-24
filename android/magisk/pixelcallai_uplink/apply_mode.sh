#!/system/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

# Bageo: make the phone an AI answering phone ("ai") or an ordinary phone ("normal").
#
#   sh apply_mode.sh <ai|normal> <id> [ring notification voice_call music alarm system]
#
# Called as root by mode_daemon.sh (in this module folder) when the app asks for a
# switch. It runs outside the app, so it finishes even when Android stops the app on
# the way: taking the phone-app role away from Bageo can do that, which is why that
# step comes last. The numbers are the volumes to set in normal mode (worked out by
# the app; 0 = leave that one alone).
#
#   /data/adb/pixelcallai/mode        the mode, written first; service.sh reads it at boot
#   /data/adb/pixelcallai/pending     exists until every step has worked; the app asks
#                                     for the switch again when its service starts and
#                                     finds it
#                                     (a failed switch to AI that undid all its steps
#                                     and put the phone back as a normal phone removes it)
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
NODND=0
case "$ID" in nodnd-*|app-nodnd-*) NODND=1 ;; esac
STANDALONE_MOD=0
if [ -d "${0%/*}/system/priv-app/com.pixelcallai.streamer/bageo_assets" ] || [ -d "${0%/*}/bageo_assets" ]; then
  STANDALONE_MOD=1
fi

mkdir -p "$DIR" || exit 3
if ! mkdir "$LOCK" 2>/dev/null; then
  OLD=$(cat "$LOCK/pid" 2>/dev/null)
  # Still held only when that pid is alive and really is this script (after a reboot
  # or a SIGKILL the pid may belong to something else; service.sh also clears the
  # lock at boot).
  if [ -n "$OLD" ] && kill -0 "$OLD" 2>/dev/null && grep -qE 'apply_mode|heal_mode' "/proc/$OLD/cmdline" 2>/dev/null; then
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
IN_REVERT=0
IN_NORMAL=0
# Set before the first command of each AI step: what a failed switch to AI has to undo.
T_DIALER=0
T_GOOGLE=0
T_DND=0
T_VOL=0
OLD_ZEN=
OLD_VOL_RING=
OLD_VOL_NOTIFICATION=
PREV_MODE=$(tr -d "$STRIP" < "$DIR/mode" 2>/dev/null || echo ai)
[ "$PREV_MODE" = normal ] || PREV_MODE=ai
step() { note "step_$1=$2"; }
fail() {
  step "$1" fail
  if [ "$IN_REVERT" -eq 0 ]; then
    IN_REVERT=1
    if [ "$MODE" = normal ] && [ "$IN_NORMAL" -eq 1 ]; then
      note "revert=ai"
      echo ai > "$DIR/mode"
      apply_ai || true
      rm -f "$DIR/pending"
    elif [ "$MODE" = ai ] && [ "$PREV_MODE" = normal ]; then
      # The phone was a normal phone and the AI switch broke half way: undo every step
      # it did, so the owner's phone still rings and Google Phone answers.
      note "revert=normal"
      revert_to_normal
    else
      echo "$PREV_MODE" > "$DIR/mode"
    fi
  fi
  note "error=$2"
  note "finished=$(date +%s)"
  note "done=1"
  exit 1
}
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
(: > "$DIR/.mode.test" && rm -f "$DIR/.mode.test") || fail save save_failed

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
# Do Not Disturb stayed on: note in last_apply which mode keeps it on, so the app can
# tell the owner what to turn off in Settings > Modes. Android 15+ prints each mode of the
# current config in dumpsys as "ZenRule[id=..,state=STATE_TRUE,enabled=TRUE,..,name=..,
# zenMode=..,..pkg=..,"; older versions print them differently and only "unknown" is noted.
#   dnd_blocker=app      Bageo's own mode (left on by an older app version)
#   dnd_blocker=mode     another mode (a schedule such as Bedtime, or another app's)
#   dnd_blocker=manual   the phone's own Do Not Disturb
#   dnd_blocker=unknown  none of these could be found
#   dnd_mode=<name>      that mode's name as Settings > Modes shows it (app and mode only)
note_dnd_blocker() {
  rules=$(o dumpsys notification | sed -n '/mConfig=ZenModeConfig/,/deletedRules=/p' |
    grep 'ZenRule\[id=[^,]*,state=STATE_TRUE,enabled=TRUE' | grep -v 'OVERRIDE_DEACTIVATE')
  own=$(printf '%s\n' "$rules" | grep -F ",pkg=$PKG," | head -n 1)
  other=$(printf '%s\n' "$rules" | grep -v 'manualRule=' | grep -vF ",pkg=$PKG," | head -n 1)
  line=""
  if [ -n "$own" ]; then
    kind=app
    line=$own
  elif [ -n "$other" ]; then
    kind=mode
    line=$other
  elif printf '%s\n' "$rules" | grep -q 'manualRule='; then
    kind=manual
  else
    kind=unknown
  fi
  note "dnd_blocker=$kind"
  [ -n "$line" ] || return 0
  name=$(printf '%s\n' "$line" | sed -n 's/.*,name=\(.*\),zenMode=.*/\1/p' | head -n 1 | tr -d '\r\n')
  if [ -n "$name" ] && [ "$name" != null ]; then note "dnd_mode=$name"; fi
  return 0
}
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
# mode (Bageo is the phone app again and answers), then report the failure.
dialer_fail() {
  IN_REVERT=1
  step dialer fail
  note "revert=ai"
  echo ai > "$DIR/mode"
  apply_ai || true
  rm -f "$DIR/pending"
  note "error=dialer_failed"
  note "finished=$(date +%s)"
  note "done=1"
  exit 1
}

apply_normal() {
  IN_NORMAL=1
  # 1. Bageo no longer screens (silences) incoming calls.
  remove_holder android.app.role.CALL_SCREENING "$PKG"
  holds android.app.role.CALL_SCREENING "$PKG"
  [ $? -eq 0 ] && fail screening screening_failed
  step screening ok

  # 2. Do Not Disturb off (skipped in Daily Sound / no-DND mode). Both commands
  #    only reach the phone's own Do Not Disturb: a mode that stays on fails here.
  if [ "$NODND" -eq 1 ]; then
    step dnd ok
    set_volume 2 "$3" volume_ring
    set_volume 5 "$4" volume_notification
    set_volume 0 "$5" ""
    set_volume 3 "$6" volume_music
    set_volume 4 "$7" volume_alarm
    set_volume 1 "$8" volume_system
    step volume ok
  else
    q cmd notification set_dnd off
    q settings put global zen_mode 0
    until_ok dnd_off || { note_dnd_blocker; fail dnd dnd_failed; }
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
  fi

  # 4. Undo the AI mode's root microphone mutes (controls a phone lacks are ignored).
  q cmd media.audio_flinger mute mic 0
  for c in 'Mic Mute' 'Microphone Mute' 'ADC1 Mute' 'ADC2 Mute' 'IN1 Mute' 'IN2 Mute'; do
    q tinymix "$c" 0
  done
  step mic ok

  # 5. Last, because Android may stop Bageo when it loses the role: the phone app.
  #    The new phone app takes the role over before Bageo lets go, so the phone is
  #    never without one; on any failure dialer_fail puts Bageo back.
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
    # The DIALER role has one holder: adding Google Phone replaces Bageo.
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
bageo_dialer() { [ "$(dialer)" = "$PKG" ]; }
dnd_on() { z=$(zen); [ -n "$z" ] && [ "$z" != "0" ]; }

# A failed switch to AI: put back what the steps of apply_ai changed (the phone app, the
# phone app that was turned off, Do Not Disturb, the two volumes), then the mode file.
# Google Phone goes first so the phone is never without a phone app. Best effort; the
# pending flag goes only when the phone is checked to be a normal phone again.
revert_to_normal() {
  if [ "$T_GOOGLE" -eq 1 ]; then
    q pm enable "$GD"
    q pm enable "$GD/com.android.incallui.InCallServiceImpl"
    q pm enable "$GD/com.android.incallui.InCallActivity"
    q pm enable "$GD/com.android.incallui.LegacyInCallActivity"
  fi
  if [ "$T_DND" -eq 1 ]; then
    case "$OLD_ZEN" in
      # The owner's own Do Not Disturb was on before the switch: leave it as it was.
      1|2|3) q settings put global zen_mode "$OLD_ZEN" ;;
      *) q cmd notification set_dnd off; q settings put global zen_mode 0 ;;
    esac
  fi
  if [ "$T_VOL" -eq 1 ]; then
    case "$OLD_VOL_RING" in ''|*[!0-9]*|0) ;; *) q settings put system volume_ring "$OLD_VOL_RING" ;; esac
    case "$OLD_VOL_NOTIFICATION" in ''|*[!0-9]*|0) ;; *) q settings put system volume_notification "$OLD_VOL_NOTIFICATION" ;; esac
  fi
  good=1
  if [ "$T_DIALER" -eq 1 ]; then
    remove_holder android.app.role.CALL_SCREENING "$PKG"
    if o pm list packages | grep -qx "package:$GD"; then
      add_holder android.app.role.DIALER "$GD"
      q telecom set-default-dialer "$GD"
      until_ok google_ready || good=0
    else
      remove_holder android.app.role.DIALER "$PKG"
      until_ok other_ready || good=0
    fi
  fi
  if [ "$T_DND" -eq 1 ]; then
    case "$OLD_ZEN" in 1|2|3) ;; *) until_ok dnd_off || good=0 ;; esac
  fi
  echo normal > "$DIR/mode"
  if [ "$good" -eq 1 ]; then
    rm -f "$DIR/pending"
    note "revert_ok=1"
  else
    note "revert_ok=0"
  fi
}

apply_ai() {
  for p in RECORD_AUDIO READ_PHONE_STATE READ_PHONE_NUMBERS ANSWER_PHONE_CALLS ACCESS_NOTIFICATION_POLICY \
      POST_NOTIFICATIONS READ_CALL_LOG CALL_PHONE SEND_SMS; do
    q pm grant "$PKG" "android.permission.$p"
  done
  if [ "$STANDALONE_MOD" -eq 1 ]; then
    q pm grant "$PKG" "android.permission.READ_CONTACTS"
    if [ "$NODND" -eq 1 ]; then rm -f "$DIR/spare_mute"; else echo 1 > "$DIR/spare_mute"; fi
  fi
  q cmd appops set "$PKG" ACCESS_NOTIFICATION_POLICY allow

  # 1. Bageo is the phone app and the call screener (as service.sh does at boot).
  T_DIALER=1
  q cmd role add-role-holder android.app.role.DIALER "$PKG"
  q cmd role add-role-holder android.app.role.CALL_SCREENING "$PKG"
  q telecom set-default-dialer "$PKG"
  until_ok bageo_dialer || fail dialer dialer_failed
  step dialer ok
  holds android.app.role.CALL_SCREENING "$PKG"
  [ $? -eq 1 ] && fail screening screening_failed
  step screening ok

  # 2. Only now that Bageo answers calls: Google Phone off.
  T_GOOGLE=1
  q pm disable "$GD/com.android.incallui.InCallServiceImpl"
  q pm disable "$GD/com.android.incallui.InCallActivity"
  q pm disable-user --user 0 "$GD"
  step google ok

  # 3. Do Not Disturb on, ringer and notification volume settings zero (skipped in
  #    Daily Sound / no-DND mode so notification and media audio remain normal).
  if [ "$NODND" -eq 1 ]; then
    step dnd ok
    step volume ok
  else
    T_DND=1
    OLD_ZEN=$(zen)
    q settings put global zen_mode 2
    q cmd notification set_dnd on
    until_ok dnd_on || fail dnd dnd_failed
    step dnd ok
    T_VOL=1
    OLD_VOL_RING=$(o settings get system volume_ring | tr -d "$STRIP")
    OLD_VOL_NOTIFICATION=$(o settings get system volume_notification | tr -d "$STRIP")
    q settings put system volume_ring 0
    q settings put system volume_notification 0
    step volume ok
  fi
}

if [ "$MODE" = normal ]; then apply_normal "$@"; else apply_ai; fi
echo "$MODE" > "$DIR/mode"
if [ "$(tr -d "$STRIP" < "$DIR/mode")" = "$MODE" ]; then step save ok; else fail save save_failed; fi
rm -f "$DIR/pending"
note "result=ok"
note "finished=$(date +%s)"
note "done=1"
exit 0
