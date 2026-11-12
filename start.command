#!/bin/sh
# Myna - double-click to start (macOS). See start.sh for what it does.
cd "$(dirname "$0")" || exit 1
exec ./start.sh "$@"
