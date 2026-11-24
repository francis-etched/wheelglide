# wheelglide

Trackpad-style scrolling for a notched mouse wheel (built for the ProtoArc EM11 NL). Replaces LinearMouse's scrolling.

## Behaviour

- Every wheel click scrolls one fixed step (`[scroll] step`, default 80 px).
- A click is *fast* if it arrived in the very next Bluetooth packet (about 7.5 ms), or one packet carried 2 clicks.
  One fast click makes a flick.
- When a flick's next packet doesn't come, the content keeps moving at the flick's own speed and loses
  `1 - decay` (`[scroll] decay`, default 0.975) of that speed every 60 Hz frame. Any click stops the glide.
- Output is posted as continuous, trackpad-style scroll events with scroll and momentum phases, so apps treat it
  like the trackpad.
- Shift+wheel scrolls horizontally by the same rule, as plain pixel scrolling without trackpad phases (with phases,
  browsers read a sideways scroll as a back/forward or sidebar swipe; as raw notched scrolls, Zen's sidebar switches
  spaces). Scrolling with any other modifier held (Ctrl, Option, Cmd, or combinations) passes through untouched.

## Side-button gestures

Hold back or forward and slide, like a Logitech gesture button. Bindings are the `[gestures]`, `[back]` and
`[forward]` tables of the config file (below):

- While a bound button is held the cursor stays put. Once you have moved `[gestures] step` px (default 80; a `[back]` / `[forward]` table can set its own `step`), the gesture locks to
  that axis, and every further `step` px along it is one more step; sliding back steps the opposite way (volume up,
  then down). `once = true` on a direction limits it to one firing per hold.
- Releasing before a full step is a click: normal back / forward, or the button's `click = { ... }` binding (same
  actions as directions). A button with only `click` bound just clicks: no freeze, no gesture. A button with no
  bindings is left alone entirely.
- Actions: `sh` (a shell command, run in order off the event thread, e.g. `nehirctl ...`), `key` (`"cmd+shift+left"`),
  or `media` (`volume_up`, `volume_down`, `mute`, `brightness_up`, `brightness_down`, `play`, `next`, `previous`,
  posted as real media keys so macOS shows its overlay), or `button` (`"back"` / `"forward"`, a real mouse click).
  `step` can also be set per direction, e.g. fine volume and coarse window switching on one button.


The logic is in `src/gesture.rs` (no OS calls, tested in `tests/gesture.rs`).

The rule lives in `src/engine.rs` (no OS calls); `tests/replay.rs` replays recorded single-click, smooth-roll and
flick takes through it. `src/main.rs` is the macOS plumbing: IOHIDManager for raw wheel reports, a HID-level event
tap that swallows the mouse's own wheel events, and a 60 Hz timer that posts frames.

## Config

Everything lives in `~/.config/wheelglide/config.toml` (see `config.example.toml`; install writes it if absent).
It is re-read when it changes, checked when a scroll starts and on each side-button press, so edits apply without
a restart. An error keeps the previous settings, is logged, and shows one macOS notification (the same error isn't
shown again until it changes or the file loads cleanly).

## Install / uninstall

```sh
scripts/install.sh      # build, install ~/Applications/WheelGlide.app, run now and at login; quits LinearMouse
scripts/uninstall.sh    # remove everything, reset its permissions, relaunch LinearMouse if install quit it
scripts/toggle.sh       # turn it off / on (off lasts until turned on again or the next login)
```

With SwiftBar set up, install also adds a menu-bar mouse icon (filled = on); click it to toggle.

Grant WheelGlide **Accessibility** and **Input Monitoring** when asked. Run `scripts/setup-signing.sh` once first:
it makes a local signing identity (in `.local/signing/`, git-ignored), so the grants survive rebuilds. Without it the
build is ad-hoc signed and the grants have to be given again after each rebuild. Log: `~/Library/Logs/wheelglide.log` (`WHEELGLIDE_DEBUG=1` logs every click).
