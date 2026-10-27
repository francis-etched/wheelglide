# wheelglide

Trackpad-style scrolling for a notched mouse wheel (built for the ProtoArc EM11 NL). Replaces LinearMouse's scrolling.

## Behaviour

- Every wheel click scrolls one fixed step (`WHEELGLIDE_STEP`, default 80 px).
- A click is *fast* if it arrived in the very next Bluetooth packet (about 7.5 ms), or one packet carried 2 clicks.
  One fast click makes a flick.
- When a flick's next packet doesn't come, the content keeps moving at the flick's own speed and loses
  `1 - WHEELGLIDE_DECAY` (default decay 0.975) of that speed every 60 Hz frame. Any click stops the glide.
- Output is posted as continuous, trackpad-style scroll events with scroll and momentum phases, so apps treat it
  like the trackpad. Scrolling with a modifier key held (Shift, Ctrl, Option, Cmd) passes through untouched.

The rule lives in `src/engine.rs` (no OS calls); `tests/replay.rs` replays recorded single-click, smooth-roll and
flick takes through it. `src/main.rs` is the macOS plumbing: IOHIDManager for raw wheel reports, a HID-level event
tap that swallows the mouse's own wheel events, and a 60 Hz timer that posts frames.

## Install / uninstall

```sh
scripts/install.sh      # build, install ~/Applications/WheelGlide.app, run now and at login; quits LinearMouse
scripts/uninstall.sh    # remove everything, reset its permissions, relaunch LinearMouse if install quit it
```

Grant WheelGlide **Accessibility** and **Input Monitoring** when asked. Run `scripts/setup-signing.sh` once first:
it makes a local signing identity (in `.local/signing/`, git-ignored), so the grants survive rebuilds. Without it the
build is ad-hoc signed and the grants have to be given again after each rebuild. Log: `~/Library/Logs/wheelglide.log` (`WHEELGLIDE_DEBUG=1` logs every click).
