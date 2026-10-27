#!/usr/bin/env bash
# SwiftBar menu-bar toggle for wheelglide: a mouse icon (filled = on); click it to turn wheelglide off / on.
# Installed into SwiftBar's plugin folder by scripts/install.sh; same on/off semantics as scripts/toggle.sh.
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

if [[ "${1:-}" == toggle ]]; then
  if running; then off; else on; fi
  exit 0
fi

if running; then img=computermouse.fill tip="wheelglide on"; else img=computermouse tip="wheelglide off"; fi
echo "| sfimage=$img tooltip=\"$tip, click to toggle\" bash=\"$0\" param1=toggle terminal=false refresh=true"
