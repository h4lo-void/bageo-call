#!/system/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

# Bageo: switches the phone between "ai" and "normal" for the app, as root.
#
# The app cannot use su itself (KernelSU only offers it to apps on its allow list),
# so service.sh starts this helper once per boot, in its own session. Every 2
# seconds it looks for a request from the app in the app's device-protected folder:
#
#   APPDIR/request   one line ending in a newline, nothing else, at most 128 bytes:
#                    <ai|normal> <id> [0 to 6 volumes], single spaces
#                    id: letters, digits and dashes, at most 64; volume: 1 to 3 digits
#   APPDIR/audio_helper
#                    one line ending in a newline, at most 80 bytes: start <id>
#                    starts bin/bageo-audio-helper, the root audio helper for phones
#                    whose call audio path the app cannot use by itself. Only when no
#                    switch request was taken in the same round, at most once every 10
#                    seconds, not while one runs, and not while
#                    /data/adb/pixelcallai/audio_helper.disabled exists. Nothing starts
#                    it otherwise.
#
# It takes the request (a rename, so the app can still withdraw one nobody took),
# checks it, runs apply_mode.sh with it and reports to the app in APPDIR/state.
#
# A switch request is run only for the app the system lists (packages.list) exactly
# once, with the user ID that owns its folder and its usual data folder. A debuggable
# app can be entered with `adb shell run-as` by anyone with adb, who could then write
# requests as the app; so a debuggable app's request is refused (error=app_refused),
# with one exception: while the app is the module's own build (no update of it installed
# under /data/app), so that a module built with a debug build of the app (as before the
# modules shipped release builds) keeps switching. The module ships a release build, so
# a debuggable app there can only be an update installed over it, and is refused.
#
# The state:
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
#   error=app_refused       the last request did not come from an app it serves
#                           (see above) and was not run
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
PKGLIST=/data/system/packages.list
APPCODE=/data/app
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
# Requests an earlier run took but never read (it was stopped half way): never run.
rm -f "$DIR/request.taken" "$DIR/audio_helper.taken" "$DIR"/request.taken.* "$DIR"/audio_helper.taken.* \
  "$DIR/request.stuck" "$DIR/audio_helper.stuck" 2>/dev/null

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
# path afterwards cannot send it elsewhere. Nothing the app made is ever deleted
# recursively, copied, or opened unless it is a plain file. Each step runs in a
# subshell, so this shell stays where it is.
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

# Takes the app's file $1 and prints the one line it holds. Returns 0 when it was a
# plain file of the app's holding exactly one line that ends in a newline, at most $2
# bytes with the newline (the line is printed without it); 2 when something was taken
# but dropped (a link, a pipe, an empty folder, a file of someone else's, too large, or
# not one whole line; logged); 1 when nothing was taken: nothing there, the app withdrew
# it meanwhile, or it cannot be taken (a folder with something in it: left where it is,
# logged once, and it holds up nothing else).
#
# The file is taken with a hard link from the app's folder into root's own $DIR, under a
# name of its own for every take (the caller counts TAKE_N up first, as this runs in a
# subshell), then its name in the app's folder is removed. A hard link never copies and
# never moves anything into the app's reach: on another file system, or for a folder, it
# just fails. Only that link in root's folder is ever read, once, then removed.
TAKE_N=0
take_app_file() {
  tname=$1
  tag=$$.$TAKE_N.$(date +%s)
  taken=$DIR/$tname.taken.$tag
  stuck=$DIR/$tname.stuck
  # One subshell, so every step works in the one folder that was checked.
  (
    enter_app_dir || exit 1
    { [ -e "$tname" ] || [ -L "$tname" ]; } || exit 1
    if [ -L "$tname" ]; then
      rm -f "$tname"
      exit 2
    fi
    if ! ln "$tname" "$taken"; then
      if [ -d "$tname" ] && rmdir "$tname"; then exit 2; fi
      exit 4
    fi
    # Taken only while the name still is that file: the app may have withdrawn it (and
    # written a new one) meanwhile.
    if [ "$(stat -c %i "$tname")" != "$(stat -c %i "$taken")" ]; then
      rm -f "$taken"
      exit 1
    fi
    rm -f "$tname"
    exit 0
  ) 2>/dev/null
  case $? in
    0) rm -f "$stuck" ;;
    1) return 1 ;;
    4)
      if [ ! -e "$stuck" ]; then
        log -t "$TAG" "cannot take $tname; left where it is" 2>/dev/null
        : > "$stuck"
      fi
      return 1
      ;;
    *)
      log -t "$TAG" "$tname dropped: not a plain file" 2>/dev/null
      return 2
      ;;
  esac
  size=$(stat -c %s "$taken" 2>/dev/null)
  if [ -L "$taken" ] || [ ! -f "$taken" ] || [ "$(stat -c %u "$taken" 2>/dev/null)" != "$APP_UID" ]; then
    rm -f "$taken"
    log -t "$TAG" "$tname dropped: not a plain file of the app's" 2>/dev/null
    return 2
  fi
  case "$size" in ''|*[!0-9]*) size=$(($2 + 1)) ;; esac
  if [ "$size" -gt "$2" ]; then
    rm -f "$taken"
    log -t "$TAG" "$tname dropped: too large" 2>/dev/null
    return 2
  fi
  content=$(head -c "$(($2 + 1))" "$taken" 2>/dev/null; echo x)
  rm -f "$taken"
  content=${content%x}
  [ "${#content}" -le "$2" ] || return 2
  case "$content" in *"$NL") ;; *) return 2 ;; esac
  line=${content%"$NL"}
  case "$line" in ''|*"$NL"*) return 2 ;; esac
  printf '%s' "$line"
  return 0
}

# Whether a switch request may be run (see the top of this file); REFUSED says why not.
APP_LIST_MAX=8388608
app_allowed() {
  REFUSED=
  if [ -L "$PKGLIST" ] || [ ! -f "$PKGLIST" ]; then
    REFUSED="package list unreadable"
    return 1
  fi
  size=$(stat -c %s "$PKGLIST" 2>/dev/null)
  case "$size" in ''|*[!0-9]*) REFUSED="package list unreadable"; return 1 ;; esac
  if [ "$size" -gt "$APP_LIST_MAX" ]; then
    REFUSED="package list too large"
    return 1
  fi
  lines=$(grep '^com\.pixelcallai\.streamer ' "$PKGLIST" 2>/dev/null)
  case "$lines" in ''|*"$NL"*) REFUSED="app not listed exactly once"; return 1 ;; esac
  set -f
  set -- $lines
  set +f
  if [ "$2" != "$APP_UID" ]; then
    REFUSED="app user ID differs from its folder's owner"
    return 1
  fi
  if [ "$4" != "/data/user/0/$PKG" ]; then
    REFUSED="app data folder unexpected"
    return 1
  fi
  case "$3" in
    0) return 0 ;;
    1) ;;
    *) REFUSED="package list malformed"; return 1 ;;
  esac
  # Debuggable: only while it is this module's own build, with no update installed.
  for d in "$APPCODE/$PKG"-* "$APPCODE"/*/"$PKG"-*; do
    if [ -e "$d" ] || [ -L "$d" ]; then
      REFUSED="debuggable app installed as an update"
      return 1
    fi
  done
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
  # mv -T: the target is a name, never a folder to move into (a link there is replaced,
  # not followed); the toybox of every Android this module supports (10 and later)
  # has it. Without it nothing is published.
  if ! ( enter_app_dir && mv -fT "$tmp" state ) 2>/dev/null; then
    rm -f "$tmp"
    return 1
  fi
  PUB_SIG=$sig
  return 0
}

# Take one request, check it and run it. Returns 1 when nothing was taken.
handle_request() {
  app_uid || return 1
  TAKE_N=$((TAKE_N + 1))
  req=$(take_app_file request 129)
  case $? in
    0) ok=1 ;;
    2) ok=0; req= ;;
    *) return 1 ;;
  esac

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
    canon="$1 $2"
    shift 2
    for v in "$@"; do
      case "$v" in ''|*[!0-9]*) ok=0 ;; esac
      [ "${#v}" -le 3 ] || ok=0
      canon="$canon $v"
    done
    # Single spaces, nothing before or after: the request exactly as the app writes it.
    [ "$canon" = "$req" ] || ok=0
  fi

  LAST_REQ=$id
  if [ "$ok" != 1 ]; then
    LAST_EXIT=2
    LAST_ERR=invalid_request
    log -t "$TAG" "invalid request ignored" 2>/dev/null
    publish 0
    return 0
  fi
  if ! app_allowed; then
    LAST_EXIT=2
    LAST_ERR=app_refused
    log -t "$TAG" "request refused: $REFUSED" 2>/dev/null
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
# own session, unless it is turned off, runs already or was started just now. Returns
# 1 when nothing was taken.
handle_helper() {
  app_uid || return 1
  TAKE_N=$((TAKE_N + 1))
  req=$(take_app_file audio_helper 81)
  case $? in
    0) ok=1 ;;
    2) ok=0; req= ;;
    *) return 1 ;;
  esac

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
  [ "start $hid" = "$req" ] || ok=0

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
  # What cannot be taken holds up neither the other request nor the state.
  took=0
  if [ -e "$APPDIR/request" ] || [ -L "$APPDIR/request" ]; then
    handle_request && took=1
  fi
  if [ "$took" = 0 ] && { [ -e "$APPDIR/audio_helper" ] || [ -L "$APPDIR/audio_helper" ]; }; then
    handle_helper && took=1
  fi
  if [ "$took" = 0 ]; then
    if [ -d "$APPDIR" ] && [ ! -f "$APPDIR/state" ]; then
      publish 0
    else
      quick_sig
      [ "$QSIG" != "$PUB_SIG" ] && publish 0
    fi
  fi
  sleep 2
done
