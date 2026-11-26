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
# Taking a request and writing the state never cross between the app's folder and
# root's own $DIR: the two have different encryption policies (a rename or link between
# them fails as if on another file system, and mv would fall back to copying). Both
# happen in a folder of root's own made inside the app's folder for that one step
# (see enter_own_dir), and move between it and the app's folder by rename only.
#
# A request (switch or audio helper) is run only for the app the system lists
# (packages.list) exactly once, not debuggable (a debuggable app can be entered with
# `adb shell run-as` by anyone with adb), with a user ID no other package shares, that
# owns its folder, and with its usual data folder; anything else is refused
# (error=app_refused). The module installs a release build of the app.
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
#   error=take_failed       a request was there but could not be taken; it was left
#                           where it is and not run
#   audio_helper=<id>       the last valid audio_helper request taken
#   audio_helper_result=started|running|busy|disabled|missing|invalid_request|app_refused|take_failed
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
# A stop request ends the loop between steps, so a take or a publish is never cut short.
STOP=
trap 'STOP=1' INT TERM
# Files an earlier version took into root's folder but never read: never run.
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
# recursively, copied, or read unless it is a plain file in a folder only root can
# change. Each step runs in a subshell, so this shell stays where it is.
APP_UID=
ME_UID=$(id -u)
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

# Makes the folder $1 inside the app's folder and enters it: root's own (mkdir never
# follows a link and fails when the name is taken), mode 700, so the app can neither
# read nor change anything in it, nor move it elsewhere (that needs write access to
# the folder itself). Being inside the app's folder it has the app folder's encryption
# policy, so a rename between the two is a plain rename: never a copy. Run in a
# subshell; `..` is then the app's folder.
enter_own_dir() {
  enter_app_dir || return 1
  mkdir -m 700 "$1" 2>/dev/null || return 1
  cd -P "$1" 2>/dev/null || return 1
  [ "$(pwd -P)" = "$APPDIR/$1" ] || return 1
  [ "$(stat -c %u . 2>/dev/null)" = "$ME_UID" ] || return 1
}
# Removes such a folder (run in the app's folder): only the names this script puts
# there, then the empty folder; a link or a folder of someone else's is left alone.
drop_own_dir() {
  [ -d "$1" ] && [ ! -L "$1" ] || return 0
  ( cd -P "$1" 2>/dev/null && [ "$(pwd -P)" = "$APPDIR/$1" ] &&
    [ "$(stat -c %u . 2>/dev/null)" = "$ME_UID" ] && rm -f r s ) 2>/dev/null
  rmdir "$1" 2>/dev/null
}

# Takes the app's file $1 and prints the one line it holds. Returns 0 when it was a
# plain file of the app's holding exactly one line that ends in a newline, at most $2
# bytes with the newline (the line is printed without it); 2 when something was taken
# but dropped (a link, a pipe, a folder, a file of someone else's, too large, or not one
# whole line; a folder is handed back to the app under another name, untouched); 1
# when nothing was there (or the app withdrew it); 3 when it was there but could not be
# taken (it is left where it is).
#
# The name is renamed into a folder of root's own inside the app's folder (see
# enter_own_dir): from then on the app cannot replace or change what is read, so the
# file can be checked and read there by name. The caller counts TAKE_N up first, as
# this runs in a subshell.
TAKE_N=0
take_app_file() {
  tname=$1
  tdir=.bageo-take.$$.$TAKE_N.$(date +%s)
  line=$( (
    enter_app_dir || exit 1
    { [ -e "$tname" ] || [ -L "$tname" ]; } || exit 1
    enter_own_dir "$tdir" || exit 3
    if ! mv -fT "../$tname" r; then
      if [ -e "../$tname" ] || [ -L "../$tname" ]; then exit 3; fi
      exit 1
    fi
    if [ -L r ]; then
      rm -f r
      exit 2
    fi
    if [ -d r ]; then
      mv -fT r "../$tname.rejected.${tdir#.bageo-take.}"
      exit 2
    fi
    if [ ! -f r ]; then
      rm -f r
      exit 2
    fi
    size=$(stat -c %s r)
    case "$size" in (''|*[!0-9]*) exit 2 ;; esac
    [ "$(stat -c %u r)" = "$APP_UID" ] && [ "$size" -le "$2" ] || exit 2
    content=$(head -c "$(($2 + 1))" r; echo x)
    content=${content%x}
    [ "${#content}" -le "$2" ] || exit 2
    case "$content" in (*"$NL") ;; (*) exit 2 ;; esac
    line=${content%"$NL"}
    case "$line" in (''|*"$NL"*) exit 2 ;; esac
    printf '%s' "$line"
    exit 0
  ) 2>/dev/null )
  rc=$?
  ( enter_app_dir && drop_own_dir "$tdir" ) 2>/dev/null
  case "$rc" in
    0) printf '%s' "$line"; return 0 ;;
    1|3) return "$rc" ;;
    *)
      log -t "$TAG" "$tname dropped: not one line in a plain file of the app's" 2>/dev/null
      return 2
      ;;
  esac
}

# Whether a request may be run (see the top of this file); REFUSED says why not.
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
  if [ "$3" != 0 ]; then
    REFUSED="app is debuggable or its entry malformed"
    return 1
  fi
  if [ "$4" != "/data/user/0/$PKG" ]; then
    REFUSED="app data folder unexpected"
    return 1
  fi
  shared=$(grep -c "^[^ ]* $APP_UID " "$PKGLIST" 2>/dev/null)
  if [ "$shared" != 1 ]; then
    REFUSED="app shares its user ID"
    return 1
  fi
  return 0
}

# Write the state: made in a folder of root's own inside the app's folder, given to the
# app (owner, mode 600, the app folder's SELinux label), then renamed over the old one
# (mv -T: a name, never a folder to move into; a link there is replaced, not followed;
# the rename stays inside the app's folder, so it is never a copy). Skipped while the
# folder is missing or not the app's.
PUB_N=0
publish() {
  app_uid || return 1
  quick_sig
  sig=$QSIG
  owner=$(stat -c %u:%g "$APPHOME" 2>/dev/null)
  case "$owner" in ''|:*|*:|*[!0-9:]*) return 1 ;; esac
  label=$(stat -c %C "$APPHOME" 2>/dev/null)
  PUB_N=$((PUB_N + 1))
  pdir=.bageo-state.$$.$PUB_N
  (
    enter_own_dir "$pdir" || exit 1
    state_lines "$1" > s || exit 1
    [ -s s ] && chown "$owner" s && chmod 600 s || exit 1
    case "$label" in u:object_r:*) chcon "$label" s ;; esac
    mv -fT s ../state || exit 1
  ) 2>/dev/null
  rc=$?
  ( enter_app_dir && drop_own_dir "$pdir" ) 2>/dev/null
  [ "$rc" = 0 ] || return 1
  PUB_SIG=$sig
  return 0
}

# Take one request, check it and run it. Returns 1 when nothing was there.
handle_request() {
  app_uid || return 1
  TAKE_N=$((TAKE_N + 1))
  req=$(take_app_file request 129)
  case $? in
    0) ok=1 ;;
    2) ok=0; req= ;;
    3)
      # Left where it is; said once, and the audio helper request still goes on.
      if [ "$LAST_ERR" != take_failed ]; then
        log -t "$TAG" "cannot take the request; left where it is" 2>/dev/null
        LAST_REQ=
        LAST_EXIT=2
        LAST_ERR=take_failed
        publish 0
      fi
      return 1
      ;;
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
# 1 when nothing was there.
handle_helper() {
  app_uid || return 1
  TAKE_N=$((TAKE_N + 1))
  req=$(take_app_file audio_helper 81)
  case $? in
    0) ok=1 ;;
    2) ok=0; req= ;;
    3)
      if [ "$LAST_HELPER_RESULT" != take_failed ]; then
        log -t "$TAG" "cannot take the audio helper request; left where it is" 2>/dev/null
        LAST_HELPER_RESULT=take_failed
        publish 0
      fi
      return 1
      ;;
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
  elif ! app_allowed; then
    LAST_HELPER=$hid
    LAST_HELPER_RESULT=app_refused
    log -t "$TAG" "audio helper request refused: $REFUSED" 2>/dev/null
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
# Folders of root's own an earlier run left in the app's folder (it was stopped half way).
if app_uid; then
  ( enter_app_dir && for d in .bageo-take.* .bageo-state.*; do drop_own_dir "$d"; done ) 2>/dev/null
fi
publish 0
while [ -z "$STOP" ]; do
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
