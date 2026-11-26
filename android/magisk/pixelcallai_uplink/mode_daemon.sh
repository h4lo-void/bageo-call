#!/system/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

# Bageo: switches the phone between "ai" and "normal" for the app, as root.
#
# The app cannot use su itself (KernelSU only offers it to apps on its allow list),
# so service.sh starts this helper once per boot, in its own session. Every 2
# seconds it looks for a request from the app in the app's device-protected folder:
#
#   APPDIR/request   one line: <ai|normal> <id> [0 to 6 volumes]
#                    id: letters, digits and dashes, at most 64; volume: 1 to 3 digits
#   APPDIR/audio_helper
#                    one line: start <id>
#                    starts bin/bageo-audio-helper, the root audio helper for phones
#                    whose call audio path the app cannot use by itself. Only when no
#                    switch request waits, at most once every 10 seconds, not while
#                    one runs, and not while /data/adb/pixelcallai/audio_helper.disabled
#                    exists. Nothing starts it otherwise.
#
# It takes the request (a rename, so the app can still withdraw one nobody took),
# checks it, runs apply_mode.sh with it and reports to the app in APPDIR/state:
#
#   daemon=1
#   mode=ai|normal          /data/adb/pixelcallai/mode (no file = ai)
#   pending=0|1             a switch has not finished all its steps
#   running=0|1             a switch is running
#   updated=<epoch>         when this was written
#   uptime=<seconds>        time since boot then (the app tells an old boot's state apart)
#   request=<id>            the last request taken
#   exit=<code>             apply_mode.sh's exit code, once that run ended
#   error=invalid_request   the last request was malformed and was not run
#   audio_helper=<id>       the last valid audio_helper request taken
#   audio_helper_result=started|running|busy|disabled|missing|invalid_request
#                           running: one runs already; busy: one was started less
#                           than 10 seconds ago; disabled: the owner turned it off;
#                           missing: this module has no helper for the phone
#   log.<key>=<value>       every line of /data/adb/pixelcallai/last_apply
#
# The state is written when this starts, before and after every run, whenever the
# folder has no state file (the app was just installed or its data cleared), and
# whenever the mode, pending or last_apply file or the run lock changed since it was
# last written (apply_mode.sh run by hand or by another script), so the app never
# acts on an old mode. It belongs to the app and only the app can read it.
trap '' HUP

# Run like a root shell from adb: the system shell and toybox. Boot scripts run in the
# root manager's BusyBox standalone mode, and apply_mode.sh must not pick up its applets.
unset ASH_STANDALONE
PATH=/product/bin:/apex/com.android.runtime/bin:/apex/com.android.art/bin:/system_ext/bin:/system/bin:/system/xbin:/odm/bin:/vendor/bin:/vendor/xbin
export PATH

MODDIR=${0%/*}
PKG=com.pixelcallai.streamer
DIR=/data/adb/pixelcallai
PIDFILE=$DIR/daemon.pid
APPHOME=/data/user_de/0/$PKG
APPDIR=$APPHOME/files/mode
TAG=pixelcallai_mode
# Same rule as service.sh, apply_mode.sh and the app: drop blanks and a UTF-8 BOM.
STRIP=' \t\r\n\v\f\357\273\277'
NL='
'

mkdir -p "$DIR" || exit 3
# One helper only. service.sh removes the pid file at boot; after that the pid
# counts only while it is alive and really is this script.
OLD=$(cat "$PIDFILE" 2>/dev/null)
if [ -n "$OLD" ] && [ "$OLD" != "$$" ] && kill -0 "$OLD" 2>/dev/null &&
    grep -q mode_daemon "/proc/$OLD/cmdline" 2>/dev/null; then
  exit 0
fi
echo $$ > "$PIDFILE"
trap '[ "$(cat "$PIDFILE" 2>/dev/null)" = "$$" ] && rm -f "$PIDFILE"' EXIT
trap 'exit 0' INT TERM

LAST_REQ=
LAST_EXIT=
LAST_ERR=
PUB_SIG=
HELPER=$MODDIR/bin/bageo-audio-helper
HELPER_PID=$DIR/audio_helper.pid
HELPER_OFF=$DIR/audio_helper.disabled
HELPER_GAP=10
LAST_HELPER=
LAST_HELPER_RESULT=
LAST_HELPER_START=0

# QSIG: the files the state reports (mode, pending, last_apply) and whether the run
# lock is held, read with shell builtins only so the 2 second loop stays cheap.
quick_sig() {
  QSIG=
  for f in mode pending last_apply; do
    QSIG="$QSIG|$f"
    [ -f "$DIR/$f" ] || continue
    QSIG="$QSIG="
    while IFS= read -r line || [ -n "$line" ]; do
      QSIG="$QSIG$line;"
    done < "$DIR/$f"
  done
  [ -d "$DIR/apply.lock" ] && QSIG="$QSIG|lock"
  return 0
}

# The state lines (see the top of this file). $1 = 1: a run starts now.
state_lines() {
  run=$1
  if [ "$run" != 1 ]; then
    run=0
    p=$(cat "$DIR/apply.lock/pid" 2>/dev/null)
    if [ -n "$p" ] && kill -0 "$p" 2>/dev/null && grep -qE 'apply_mode|heal_mode' "/proc/$p/cmdline" 2>/dev/null; then
      run=1
    fi
  fi
  m=ai
  [ -f "$DIR/mode" ] && [ "$(tr -d "$STRIP" < "$DIR/mode" 2>/dev/null)" = normal ] && m=normal
  pend=0
  [ -f "$DIR/pending" ] && pend=1
  up=
  read -r up rest < /proc/uptime
  up=${up%%.*}
  case "$up" in ''|*[!0-9]*) up=0 ;; esac
  echo daemon=1
  echo "mode=$m"
  echo "pending=$pend"
  echo "running=$run"
  echo "updated=$(date +%s)"
  echo "uptime=$up"
  [ -n "$LAST_REQ" ] && echo "request=$LAST_REQ"
  [ -n "$LAST_EXIT" ] && echo "exit=$LAST_EXIT"
  [ -n "$LAST_ERR" ] && echo "error=$LAST_ERR"
  [ -n "$LAST_HELPER" ] && echo "audio_helper=$LAST_HELPER"
  [ -n "$LAST_HELPER_RESULT" ] && echo "audio_helper_result=$LAST_HELPER_RESULT"
  [ -f "$DIR/last_apply" ] && sed 's/^/log./' "$DIR/last_apply"
  return 0
}

# The app owns files/ and files/mode/ and can rename or replace them at any moment,
# for example with a link into a root folder, also between a check and the step after
# it. So nothing here walks that path to act on a file: every step first enters the
# folder (cd -P), checks that the folder it is now in really is APPDIR and belongs to
# the app, and then works on names inside it relative to that folder; renames of the
# path afterwards cannot send it elsewhere. Files go between that folder and root's own
# $DIR by rename only (a rename never follows a link), and nothing the app made is ever
# deleted recursively. Each step runs in a subshell, so this shell stays where it is.
APP_UID=
app_uid() {
  # APPHOME's parent belongs to the system, so the app cannot swap APPHOME itself.
  APP_UID=$(stat -c %u "$APPHOME" 2>/dev/null)
  case "$APP_UID" in ''|0|*[!0-9]*) APP_UID=; return 1 ;; esac
  return 0
}
enter_app_dir() {
  [ -n "$APP_UID" ] || return 1
  cd -P "$APPDIR" 2>/dev/null || return 1
  [ "$(pwd -P)" = "$APPDIR" ] || return 1
  [ "$(stat -c %u . 2>/dev/null)" = "$APP_UID" ] || return 1
}

# Takes the app's file $1 into $DIR and prints at most $2 bytes of it, when it is a
# plain file of the app's. Anything else is never opened: a link is dropped (only the
# link itself), a folder or pipe is handed back to the app under another name.
# Returns 1 when nothing was read.
take_app_file() {
  tname=$1
  taken=$DIR/$tname.taken
  if [ -L "$taken" ] || [ -f "$taken" ]; then
    rm -f "$taken"
  elif [ -e "$taken" ]; then
    return 1
  fi
  ( enter_app_dir && { [ -e "$tname" ] || [ -L "$tname" ]; } && mv -fT "$tname" "$taken" ) 2>/dev/null || return 1
  if [ -L "$taken" ]; then
    rm -f "$taken"
    return 1
  fi
  if [ ! -f "$taken" ]; then
    ( enter_app_dir && mv -fT "$taken" "$tname.rejected.$$.$(date +%s)" ) 2>/dev/null
    return 1
  fi
  if [ "$(stat -c %u "$taken" 2>/dev/null)" != "$APP_UID" ]; then
    rm -f "$taken"
    return 1
  fi
  head -c "$2" "$taken" 2>/dev/null
  rm -f "$taken"
  return 0
}

# Write the state: a temporary file in $DIR, given to the app (owner, mode 600, the
# app folder's SELinux label), then renamed over the old one inside the app's folder.
# Skipped while the folder is missing or not the app's.
publish() {
  app_uid || return 1
  quick_sig
  sig=$QSIG
  own=$(stat -c %u:%g "$APPHOME" 2>/dev/null)
  case "$own" in ''|:*|*:|*[!0-9:]*) return 1 ;; esac
  tmp=$DIR/state.tmp
  if [ -e "$tmp" ] && [ ! -f "$tmp" ] && [ ! -L "$tmp" ]; then
    return 1
  fi
  rm -f "$tmp"
  state_lines "$1" > "$tmp" 2>/dev/null
  if [ ! -s "$tmp" ] || ! chown "$own" "$tmp" 2>/dev/null || ! chmod 600 "$tmp" 2>/dev/null; then
    rm -f "$tmp"
    return 1
  fi
  label=$(stat -c %C "$APPHOME" 2>/dev/null)
  case "$label" in u:object_r:*) chcon "$label" "$tmp" 2>/dev/null ;; esac
  if ! ( enter_app_dir && mv -fT "$tmp" state ) 2>/dev/null; then
    rm -f "$tmp"
    return 1
  fi
  PUB_SIG=$sig
  return 0
}

# Take one request, check it and run it.
handle_request() {
  # A rename: the app deletes a request nobody took in time, and that one must not run.
  app_uid || return 0
  req=$(take_app_file request 200) || return 0

  ok=1
  case "$req" in *"$NL"*) ok=0 ;; esac
  [ "${#req}" -le 128 ] || ok=0
  set -f
  old_ifs=$IFS
  IFS=' '
  set -- $req
  IFS=$old_ifs
  set +f
  mode=$1
  id=$2
  case "$mode" in ai|normal) ;; *) ok=0 ;; esac
  case "$id" in ''|*[!A-Za-z0-9-]*) ok=0; id= ;; esac
  [ "${#id}" -le 64 ] || { ok=0; id=; }
  if [ "$#" -lt 2 ] || [ "$#" -gt 8 ]; then
    ok=0
  else
    shift 2
    for v in "$@"; do
      case "$v" in ''|*[!0-9]*) ok=0 ;; esac
      [ "${#v}" -le 3 ] || ok=0
    done
  fi

  LAST_REQ=$id
  if [ "$ok" != 1 ]; then
    LAST_EXIT=2
    LAST_ERR=invalid_request
    log -t "$TAG" "invalid request ignored" 2>/dev/null
    publish 0
    return 0
  fi
  LAST_EXIT=
  LAST_ERR=
  log -t "$TAG" "switch to $mode" 2>/dev/null
  publish 1
  /system/bin/sh "$MODDIR/apply_mode.sh" "$mode" "$id" "$@" </dev/null >/dev/null 2>&1
  LAST_EXIT=$?
  log -t "$TAG" "switch to $mode ended with exit code $LAST_EXIT" 2>/dev/null
  publish 0
  return 0
}

# Whether the audio helper runs: the pid it wrote is alive and is that program.
helper_running() {
  p=$(head -c 16 "$HELPER_PID" 2>/dev/null)
  case "$p" in ''|*[!0-9]*) return 1 ;; esac
  kill -0 "$p" 2>/dev/null && grep -q bageo-audio-helper "/proc/$p/cmdline" 2>/dev/null
}

# Take one audio helper request ("start <id>"), check it and start the helper in its
# own session, unless it is turned off, runs already or was started just now.
handle_helper() {
  app_uid || return 0
  req=$(take_app_file audio_helper 100) || return 0

  ok=1
  case "$req" in *"$NL"*) ok=0 ;; esac
  [ "${#req}" -le 80 ] || ok=0
  set -f
  old_ifs=$IFS
  IFS=' '
  set -- $req
  IFS=$old_ifs
  set +f
  [ "$#" -eq 2 ] && [ "$1" = start ] || ok=0
  hid=$2
  case "$hid" in ''|*[!A-Za-z0-9-]*) ok=0 ;; esac
  [ "${#hid}" -le 64 ] || ok=0

  now=$(date +%s)
  since=$((now - LAST_HELPER_START))
  if [ "$ok" != 1 ]; then
    LAST_HELPER_RESULT=invalid_request
  else
    LAST_HELPER=$hid
    if [ -e "$HELPER_OFF" ]; then
      LAST_HELPER_RESULT=disabled
    elif [ ! -f "$HELPER" ]; then
      LAST_HELPER_RESULT=missing
    elif helper_running; then
      LAST_HELPER_RESULT=running
    elif [ "$since" -ge 0 ] && [ "$since" -lt "$HELPER_GAP" ]; then
      LAST_HELPER_RESULT=busy
    else
      [ -x "$HELPER" ] || chmod 0700 "$HELPER" 2>/dev/null
      if command -v setsid >/dev/null 2>&1; then
        setsid "$HELPER" serve </dev/null >/dev/null 2>&1 &
      else
        "$HELPER" serve </dev/null >/dev/null 2>&1 &
      fi
      LAST_HELPER_START=$now
      LAST_HELPER_RESULT=started
    fi
  fi
  log -t "$TAG" "audio helper request: $LAST_HELPER_RESULT" 2>/dev/null
  publish 0
  return 0
}

log -t "$TAG" "mode helper started (pid $$)" 2>/dev/null
publish 0
while :; do
  # A phone mode switch always goes first; the audio helper waits for the next round.
  if [ -e "$APPDIR/request" ] || [ -L "$APPDIR/request" ]; then
    handle_request
  elif [ -e "$APPDIR/audio_helper" ] || [ -L "$APPDIR/audio_helper" ]; then
    handle_helper
  elif [ -d "$APPDIR" ] && [ ! -f "$APPDIR/state" ]; then
    publish 0
  else
    quick_sig
    [ "$QSIG" != "$PUB_SIG" ] && publish 0
  fi
  sleep 2
done