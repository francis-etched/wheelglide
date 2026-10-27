#!/usr/bin/env bash
# Remove wheelglide completely: LaunchAgent, app, menu-bar toggle, permissions, state, log. Relaunches LinearMouse if install quit it.
# Idempotent. No sudo.
set -euo pipefail

LABEL=com.francis.wheelglide
APP="$HOME/Applications/WheelGlide.app"
AGENT="$HOME/Library/LaunchAgents/$LABEL.plist"
STATE="$HOME/Library/Application Support/wheelglide"
LOG="$HOME/Library/Logs/wheelglide.log"

say() { printf '==> %s\n' "$*"; }

launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null && say "stopped $LABEL" || true
rm -f "$AGENT"
rm -rf "$APP"
PLUGINS="$(defaults read com.ameba.SwiftBar PluginDirectory 2>/dev/null || true)"
if [[ -n "$PLUGINS" && -e "$PLUGINS/wheelglide.10s.sh" ]]; then
  rm -f "$PLUGINS/wheelglide.10s.sh"
  open -g "swiftbar://refreshallplugins" || true
fi
tccutil reset Accessibility "$LABEL" >/dev/null 2>&1 || true
tccutil reset ListenEvent "$LABEL" >/dev/null 2>&1 || true
say "removed app, LaunchAgent and permissions"

if [[ -e "$STATE/linearmouse-was-running" ]]; then
  say "relaunching LinearMouse"
  open -b com.lujjjh.LinearMouse || true
fi
rm -rf "$STATE" "$LOG"
say "done"
