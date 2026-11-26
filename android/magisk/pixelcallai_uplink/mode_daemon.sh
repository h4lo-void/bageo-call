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
# Taking a request and publishing APPDIR/state are performed by `bageo-audio-helper`
# (`ipc-take`, `ipc-publish`, `ipc-sweep`), which opens APPDIR step by step with
# O_NOFOLLOW, verifies owner and SELinux label on the open descriptors, stages inside a
# pinned 0700 root working folder under APPDIR (same fscrypt policy), and moves entries
# with pure renameat only — never shell mv, never copy fallback.
#
# A request (switch or audio helper) is run only for the app the system lists
# (packages.list) exactly once, not debuggable (a debuggable app can be entered with
# `adb shell run-as` by anyone with adb), with a user ID no other package shares, that
# owns its folder, and with its usual data folder; anything else is refused
# (error=app_refused). The module installs a release build of the app.
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
OLD=$(cat "$PIDFILE" 2>/dev/null)
if [ -n "$OLD" ] && [ "$OLD" != "$$" ] && kill -0 "$OLD" 2>/dev/null &&
    grep -q mode_daemon "/proc/$OLD/cmdline" 2>/dev/null; then
  exit 0
fi
echo $$ > "$PIDFILE"
trap '[ "$(cat "$PIDFILE" 2>/dev/null)" = "$$" ] && rm -f "$PIDFILE"' EXIT
STOP=
trap 'STOP=1' INT TERM
rm -f "$DIR/request.taken" "$DIR/audio_helper.taken" "$DIR"/request.taken.* "$DIR"/audio_helper.taken.* \
  "$DIR/request.stuck" "$DIR/audio_helper.stuck" 2>/dev/null

LAST_REQ=
LAST_EXIT=
LAST_ERR=
PUB_SIG=
PUB_PENDING=1
HELPER=$MODDIR/bin/bageo-audio-helper
FS_HELPER=${BAGEO_FS_HELPER:-$HELPER}
HELPER_PID=$DIR/audio_helper.pid
HELPER_OFF=$DIR/audio_helper.disabled
HELPER_GAP=10
LAST_HELPER=
LAST_HELPER_RESULT=
LAST_HELPER_START=0

# QSIG: everything the published state depends on, read with shell builtins so the 2 s
# loop stays cheap.
quick_sig() {
  QSIG="$LAST_REQ|$LAST_EXIT|$LAST_ERR|$LAST_HELPER|$LAST_HELPER_RESULT"
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

# The state lines. $1 = 1: a run starts now.
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

APP_UID=
app_uid() {
  [ ! -L "$APPHOME" ] && [ -d "$APPHOME" ] || { APP_UID=; return 1; }
  APP_UID=$(stat -c %u "$APPHOME" 2>/dev/null)
  case "$APP_UID" in ''|0|*[!0-9]*) APP_UID=; return 1 ;; esac
  return 0
}

ensure_fs_helper() {
  [ -f "$FS_HELPER" ] || return 1
  [ -x "$FS_HELPER" ] || chmod 0700 "$FS_HELPER" 2>/dev/null
  [ -x "$FS_HELPER" ]
}

# Takes the app's file $1 (at most $2 bytes with its trailing newline) through
# `FS_HELPER ipc-take` (FD-pinned pure renameat). Returns 0 (line printed), 1 (nothing
# there), 2 (dropped invalid/non-plain entry), or 3 (present but could not be taken).
take_app_file() {
  ensure_fs_helper || return 3
  line=$("$FS_HELPER" ipc-take "$APPHOME" "$1" "$2" 2>/dev/null)
  rc=$?
  case "$rc" in
    0) printf '%s' "$line"; return 0 ;;
    1|3) return "$rc" ;;
    *)
      log -t "$TAG" "$1 dropped: not one line in a plain file of the app's" 2>/dev/null
      return 2
      ;;
  esac
}

# Whether a request may be run; REFUSED says why not.
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

# Publishes `APPDIR/state` through `FS_HELPER ipc-publish` (FD-pinned pure renameat and
# required SELinux label). `PUB_PENDING` stays set until publication succeeds, so a
# temporarily blocked state is retried even when QSIG has not changed since.
publish() {
  PUB_PENDING=1
  app_uid || return 1
  ensure_fs_helper || return 1
  quick_sig
  sig=$QSIG
  if ! state_lines "$1" | "$FS_HELPER" ipc-publish "$APPHOME" >/dev/null 2>&1; then
    return 1
  fi
  PUB_SIG=$sig
  PUB_PENDING=
  return 0
}

# Take one request, check it and run it. Returns 1 when nothing was there.
handle_request() {
  app_uid || return 1
  req=$(take_app_file request 129)
  case $? in
    0) ok=1 ;;
    2) ok=0; req= ;;
    3)
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

helper_running() {
  p=$(head -c 16 "$HELPER_PID" 2>/dev/null)
  case "$p" in ''|*[!0-9]*) return 1 ;; esac
  kill -0 "$p" 2>/dev/null && grep -q bageo-audio-helper "/proc/$p/cmdline" 2>/dev/null
}

handle_helper() {
  app_uid || return 1
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
if app_uid && ensure_fs_helper; then
  "$FS_HELPER" ipc-sweep "$APPHOME" </dev/null >/dev/null 2>&1
fi
publish 0
while [ -z "$STOP" ]; do
  took=0
  if [ -e "$APPDIR/request" ] || [ -L "$APPDIR/request" ]; then
    handle_request && took=1
  fi
  if [ "$took" = 0 ] && { [ -e "$APPDIR/audio_helper" ] || [ -L "$APPDIR/audio_helper" ]; }; then
    handle_helper && took=1
  fi
  if [ "$took" = 0 ]; then
    if [ -n "$PUB_PENDING" ]; then
      publish 0
    elif [ -d "$APPDIR" ] && [ ! -L "$APPDIR" ] && { [ -L "$APPDIR/state" ] || [ ! -f "$APPDIR/state" ]; }; then
      publish 0
    else
      quick_sig
      [ "$QSIG" != "$PUB_SIG" ] && publish 0
    fi
  fi
  sleep 2
done
