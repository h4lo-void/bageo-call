#!/usr/bin/env sh
# Bageo - start the agent on macOS / Linux.
#
#   ./start.sh            (macOS: or double-click start.command)
#
# What it does, in order:
#   1. reads settings from .env next to this script (optional, see .env.example)
#   2. finds the agent program (a downloaded release, or builds it with Rust)
#   3. makes sure adb (Android platform-tools) is there, offering to download it
#   4. starts the agent and opens the web console
#   5. the first time the agent answers, puts a "Bageo Console" shortcut to the
#      web console on the desktop (macOS: .webloc; Linux: an app menu entry,
#      copied to the desktop folder when there is one)
#
# Options: --no-browser   do not open the web console
#          --yes          answer "yes" to the platform-tools download
#          --build        build from source even when a program is already there
#          --no-shortcut  no desktop shortcut (or BAGEO_NO_SHORTCUT=1; the name
#                         from before the rename, MYNA_NO_SHORTCUT, still works)
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$root"

no_browser=0; yes=0; build=0; no_shortcut=0
for arg in "$@"; do
  case "$arg" in
    --no-browser) no_browser=1 ;;
    --yes) yes=1 ;;
    --build) build=1 ;;
    --no-shortcut) no_shortcut=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

say() { printf '[bageo] %s\n' "$*"; }

# 1. .env -> environment (real environment variables win)
if [ -f .env ]; then
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in ''|'#'*) continue ;; esac
    case "$line" in *=*) ;; *) continue ;; esac
    key=$(printf '%s' "${line%%=*}" | tr -d ' \t')
    val=${line#*=}
    val=$(printf '%s' "$val" | sed -e 's/^[ \t]*//' -e 's/[ \t\r]*$//' -e 's/^"\(.*\)"$/\1/' -e "s/^'\(.*\)'\$/\1/")
    case "$key" in ''|*[!A-Za-z0-9_]*) continue ;; esac
    if eval "[ -z \"\${$key+x}\" ]"; then export "$key=$val"; fi
  done < .env
fi
case "$(printf '%s' "${BAGEO_NO_SHORTCUT:-${MYNA_NO_SHORTCUT:-}}" | tr '[:upper:]' '[:lower:]')" in
  1|true|yes|on) no_shortcut=1 ;;
esac

# 2. the agent program
agent=""
if [ "$build" -eq 0 ]; then
  for c in "$root/agent" "$root/bin/agent" "$root/target/release/agent"; do
    if [ -x "$c" ]; then agent=$c; break; fi
  done
fi
if [ -z "$agent" ]; then
  if command -v cargo >/dev/null 2>&1; then
    say "building the agent (first time takes a few minutes)..."
    cargo build --release -p agent
    agent="$root/target/release/agent"
  else
    say "The agent program was not found."
    say "Either download a release from the project's GitHub Releases page and unpack it here,"
    say "or install Rust (https://rustup.rs) and run this script again to build it."
    exit 1
  fi
fi

# 3. adb (only needed for phones on USB)
usb=$(printf '%s' "${USB_BRIDGE:-on}" | tr '[:upper:]' '[:lower:]')
case "$usb" in 0|false|off|no) usb_off=1 ;; *) usb_off=0 ;; esac
if [ "$usb_off" -eq 0 ] && [ -z "${ADB_BIN:-}" ]; then
  if [ ! -x "$root/platform-tools/adb" ] && ! command -v adb >/dev/null 2>&1; then
    say "adb (Android platform-tools) was not found. It is needed to reach a phone over USB."
    case "$(uname -s)" in
      Darwin) pt=darwin ;;
      Linux) pt=linux ;;
      *) pt="" ;;
    esac
    answer=y
    if [ "$yes" -eq 0 ] && [ -t 0 ]; then
      printf 'Download it from Google (about 7 MB) into ./platform-tools? [Y/n] '
      read -r answer || answer=n
    fi
    case "$answer" in
      ''|y|Y|yes|Yes)
        if [ -z "$pt" ]; then
          say "no platform-tools download for this system; install adb with your package manager."
        elif ! command -v unzip >/dev/null 2>&1; then
          say "unzip is not installed; install adb with your package manager instead (brew install android-platform-tools / apt install adb)."
        else
          tmp=$(mktemp -d)
          say "downloading platform-tools..."
          if command -v curl >/dev/null 2>&1; then
            curl -fsSL -o "$tmp/pt.zip" "https://dl.google.com/android/repository/platform-tools-latest-$pt.zip"
          else
            wget -q -O "$tmp/pt.zip" "https://dl.google.com/android/repository/platform-tools-latest-$pt.zip"
          fi
          unzip -q -o "$tmp/pt.zip" -d "$root"
          rm -rf "$tmp"
          [ -x "$root/platform-tools/adb" ] || { say "the download did not contain adb"; exit 1; }
          say "adb installed at $root/platform-tools/adb"
        fi
        ;;
      *) say "continuing without adb: phones on USB will not connect until it is installed." ;;
    esac
  fi
fi

# 4. run
port=${AGENT_PORT:-8787}
url="http://localhost:$port/"
say "starting $agent"
say "web console: $url   (Ctrl+C here stops the agent)"
if [ "$no_browser" -eq 0 ]; then
  (
    sleep 2
    if command -v open >/dev/null 2>&1; then open "$url"
    elif command -v xdg-open >/dev/null 2>&1; then xdg-open "$url" >/dev/null 2>&1
    fi
  ) &
fi

# 5. desktop shortcut to the web console, once the agent answers; kept when it exists
shortcut_label() {
  loc=""
  if [ "$(uname -s)" = Darwin ]; then loc=$(defaults read -g AppleLocale 2>/dev/null || true); fi
  [ -n "$loc" ] || loc=${LC_ALL:-${LC_MESSAGES:-${LANG:-}}}
  case "$loc" in
    zh*) echo "Bageo 控制台" ;;
    ja*) echo "Bageo コンソール" ;;
    ko*) echo "Bageo 콘솔" ;;
    es*) echo "Consola de Bageo" ;;
    fr*) echo "Console Bageo" ;;
    de*) echo "Bageo-Konsole" ;;
    pt*) echo "Console do Bageo" ;;
    *) echo "Bageo Console" ;;
  esac
}
make_shortcut() {
  label=$(shortcut_label)
  # A shortcut from before the rename to Bageo ("Myna Console") opens the same
  # address: keep it and add no second one.
  old_label=$(printf '%s' "$label" | sed 's/Bageo/Myna/')
  if [ "$(uname -s)" = Darwin ]; then
    file="$HOME/Desktop/$label.webloc"
    if [ ! -d "$HOME/Desktop" ] || [ -e "$file" ] || [ -e "$HOME/Desktop/$old_label.webloc" ]; then return 0; fi
    cat > "$file" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>URL</key>
  <string>$url</string>
</dict>
</plist>
EOF
    return 0
  fi
  apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
  file="$apps/bageo-console.desktop"
  if [ -e "$file" ] || [ -e "$apps/myna-console.desktop" ]; then return 0; fi
  mkdir -p "$apps"
  cat > "$file" <<EOF
[Desktop Entry]
Type=Application
Name=$label
Comment=$url
Exec=xdg-open $url
Icon=web-browser
Terminal=false
Categories=Network;
EOF
  chmod +x "$file"
  desk=$(xdg-user-dir DESKTOP 2>/dev/null || true)
  [ -n "$desk" ] || desk="$HOME/Desktop"
  if [ -d "$desk" ] && [ "$desk" != "$HOME" ] && [ ! -e "$desk/bageo-console.desktop" ] \
    && [ ! -e "$desk/myna-console.desktop" ]; then
    cp "$file" "$desk/bageo-console.desktop"
    chmod +x "$desk/bageo-console.desktop"
    # GNOME only starts desktop files marked as trusted.
    gio set "$desk/bageo-console.desktop" metadata::trusted true >/dev/null 2>&1 || true
  fi
}
if [ "$no_shortcut" -eq 0 ]; then
  # Ask the agent where it listens: 127.0.0.1 unless AGENT_HOST names one address.
  case "${AGENT_HOST:-}" in
    ''|0.0.0.0|::) probe_host=127.0.0.1 ;;
    *:*) probe_host="[${AGENT_HOST}]" ;;
    *) probe_host=$AGENT_HOST ;;
  esac
  (
    i=0
    while [ "$i" -lt 60 ]; do
      sleep 2
      i=$((i + 1))
      if command -v curl >/dev/null 2>&1; then
        curl -fsS -o /dev/null --max-time 2 "http://$probe_host:$port/health" 2>/dev/null || continue
      elif command -v wget >/dev/null 2>&1; then
        wget -q -O /dev/null -T 2 "http://$probe_host:$port/health" 2>/dev/null || continue
      fi
      make_shortcut || true
      break
    done
  ) &
fi
exec "$agent"
