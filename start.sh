#!/usr/bin/env sh
# Myna - start the agent on macOS / Linux.
#
#   ./start.sh            (macOS: or double-click start.command)
#
# What it does, in order:
#   1. reads settings from .env next to this script (optional, see .env.example)
#   2. finds the agent program (a downloaded release, or builds it with Rust)
#   3. makes sure adb (Android platform-tools) is there, offering to download it
#   4. starts the agent and opens the web console
#
# Options: --no-browser   do not open the web console
#          --yes          answer "yes" to the platform-tools download
#          --build        build from source even when a program is already there
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$root"

no_browser=0; yes=0; build=0
for arg in "$@"; do
  case "$arg" in
    --no-browser) no_browser=1 ;;
    --yes) yes=1 ;;
    --build) build=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

say() { printf '[myna] %s\n' "$*"; }

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
exec "$agent"
