#!/usr/bin/env bash
# Build wheelglide, install it as ~/Applications/WheelGlide.app, and run it now and at login (LaunchAgent),
# plus a SwiftBar menu-bar toggle when SwiftBar is set up.
# wheelglide replaces LinearMouse's scrolling, so LinearMouse is quit; uninstall.sh relaunches it if it was running.
# Idempotent. No sudo. Settings: WHEELGLIDE_STEP=80 WHEELGLIDE_DECAY=0.975 scripts/install.sh
set -euo pipefail
cd "$(dirname "$0")/.."

LABEL=com.francis.wheelglide
APP="$HOME/Applications/WheelGlide.app"
AGENT="$HOME/Library/LaunchAgents/$LABEL.plist"
STATE="$HOME/Library/Application Support/wheelglide"
LOG="$HOME/Library/Logs/wheelglide.log"
STEP="${WHEELGLIDE_STEP:-80}"
DECAY="${WHEELGLIDE_DECAY:-0.975}"
UID_NUM="$(id -u)"

say() { printf '==> %s\n' "$*"; }

say "building"
cargo build --release

say "stopping any running copy"
launchctl bootout "gui/$UID_NUM/$LABEL" 2>/dev/null || true

say "installing $APP"
mkdir -p "$APP/Contents/MacOS" "$STATE" "$(dirname "$AGENT")" "$(dirname "$LOG")"
cp target/release/wheelglide "$APP/Contents/MacOS/wheelglide"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>$LABEL</string>
  <key>CFBundleName</key><string>WheelGlide</string>
  <key>CFBundleExecutable</key><string>wheelglide</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSUIElement</key><true/>
</dict></plist>
PLIST
# Sign with the local identity (scripts/setup-signing.sh) so permissions survive rebuilds; ad-hoc otherwise.
SIGN="$PWD/.local/signing"
if [[ -f "$SIGN/identity.sha1" ]]; then
  KC="$SIGN/wheelglide-signing.keychain-db"
  security unlock-keychain -p "$(cat "$SIGN/keychain-password")" "$KC"
  # codesign looks identities up through the search list: add our keychain for the duration, then restore it.
  prev=()
  while IFS= read -r k; do k="${k#"${k%%[![:space:]]*}"}"; prev+=("${k//\"/}"); done < <(security list-keychains -d user)
  security list-keychains -d user -s "${prev[@]}" "$KC"
  trap 'security list-keychains -d user -s "${prev[@]}"' EXIT
  codesign --force --sign "$(cat "$SIGN/identity.sha1")" --keychain "$KC" --identifier "$LABEL" --timestamp=none "$APP"
  security list-keychains -d user -s "${prev[@]}"
  trap - EXIT
  say "signed with the local identity: $(codesign -d -r- "$APP" 2>&1 | sed -n 's/^designated => //p')"
else
  say "no local identity (run scripts/setup-signing.sh once): ad-hoc signing, permissions must be re-granted after each rebuild"
  codesign --force --sign - --identifier "$LABEL" "$APP"
fi

if pgrep -x LinearMouse >/dev/null; then
  say "quitting LinearMouse (uninstall.sh will relaunch it)"
  touch "$STATE/linearmouse-was-running"
  pkill -x LinearMouse || true
fi

say "writing $AGENT"
cat > "$AGENT" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>$APP/Contents/MacOS/wheelglide</string></array>
  <key>EnvironmentVariables</key><dict>
    <key>WHEELGLIDE_STEP</key><string>$STEP</string>
    <key>WHEELGLIDE_DECAY</key><string>$DECAY</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardErrorPath</key><string>$LOG</string>
</dict></plist>
PLIST

say "starting"
launchctl bootstrap "gui/$UID_NUM" "$AGENT"

# Menu-bar toggle, if SwiftBar is set up.
PLUGINS="$(defaults read com.ameba.SwiftBar PluginDirectory 2>/dev/null || true)"
if [[ -n "$PLUGINS" && -d "$PLUGINS" ]]; then
  cp scripts/swiftbar/wheelglide.10s.sh "$PLUGINS/"
  open -g "swiftbar://refreshallplugins" || true
  say "menu-bar toggle installed in SwiftBar"
fi
sleep 2
tail -n 5 "$LOG" 2>/dev/null || true

cat <<EOF

Next (first install, or after a build without the local signing identity):
  1. Allow WheelGlide in System Settings > Privacy & Security > Accessibility and > Input Monitoring
     (macOS shows prompts). It restarts on its own within ~10 s once allowed.
  2. If LinearMouse is set to launch at login, turn that off (LinearMouse > Settings > General),
     or it will come back after a restart and smooth the wheel a second time.
Log: $LOG    Stop/undo: scripts/uninstall.sh
EOF
