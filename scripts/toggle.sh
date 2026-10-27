#!/usr/bin/env bash
# Turn wheelglide on or off: scripts/toggle.sh [toggle|on|off|status] (default toggle).
# Off lasts until it's turned on again or until the next login, when the LaunchAgent starts it.
set -euo pipefail

LABEL=com.francis.wheelglide
U="gui/$(id -u)"
AGENT="$HOME/Library/LaunchAgents/$LABEL.plist"

BIN="$HOME/Applications/WheelGlide.app/Contents/MacOS/wheelglide"

# Go by the process, not just the LaunchAgent: opening WheelGlide.app directly (Finder, Spotlight, `open`)
# starts a copy outside it.
running() { pgrep -qf "^$BIN\$"; }
loaded() { launchctl print "$U/$LABEL" >/dev/null 2>&1; }
off() {
  ! loaded || launchctl bootout "$U/$LABEL"
  pkill -f "^$BIN\$" || true
}
on() {
  loaded && running && return
  off
  launchctl bootstrap "$U" "$AGENT"
}

case "${1:-toggle}" in
  toggle) if running; then off; else on; fi ;;
  on) on ;;
  off) off ;;
  status) ;;
  *) echo "usage: $0 [toggle|on|off|status]" >&2; exit 2 ;;
esac

if running; then echo "wheelglide: on"; else echo "wheelglide: off"; fi
