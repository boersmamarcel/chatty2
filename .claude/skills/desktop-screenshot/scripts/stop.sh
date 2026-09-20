#!/usr/bin/env bash
# Stop the app and Xvfb started by start.sh, and the headless Chromes a
# browser artifact leaks when the app is killed.
#   stop.sh [--display :99]
DISPLAY_NO=":99"
[ "${1:-}" = "--display" ] && DISPLAY_NO="$2"
# Bracketed patterns so pkill never matches this script's own command line.
pkill -f "debug/[c]hatty$" || true
pkill -f "user-data-dir=/tmp/chatty-[b]rowser" || true
sleep 0.5
pkill -f "[X]vfb $DISPLAY_NO" || true
echo "stopped"
