//! Side-button gestures. No OS calls: the bindings are resolved here (the file itself is read in config.rs),
//! pointer movement goes in while a button is held, and the bindings to act on come out.
//!
//! - Holding a configured side button freezes the cursor (done by the caller) and starts a gesture.
//! - Once the movement reaches `step` px (global, or per button), the gesture locks to the axis it moved furthest on (horizontal or
//!   vertical). After that, every `step` px along that axis is one step in that direction: sliding back the
//!   other way steps the opposite direction (volume up, then down).
//! - An action marked `once` fires at most once per hold.
//! - Releasing before the gesture locked to an axis is a click: the button's own back / forward, or its
//!   `click` binding. A button with only a `click` binding just clicks (no freeze, no gesture).

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    fn index(self) -> usize {
        self as usize
    }
}

/// What a step does.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// Run with /bin/sh -c (in order, off the event thread).
    Sh(String),
    /// Post a key press: virtual key code + modifier flags (CGEventFlags bits).
    Key { code: u16, flags: u64 },
    /// Post a media / system key (NX_KEYTYPE_*), so macOS shows its own volume / brightness overlay.
    Media(i64),
    /// Post a click of a mouse button (CoreGraphics number: 3 = back, 4 = forward) where the cursor is.
    Button(i64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub act: Act,
    pub once: bool,
    /// This direction's step, overriding the button's.
    pub step: Option<f64>,
}

/// The four directions of one button.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ButtonMap {
    pub dirs: [Option<Binding>; 4],
    /// What a press without a gesture does, instead of the button's own back / forward click.
    pub click: Option<Act>,
    /// This button's step, overriding the global one.
    pub step: Option<f64>,
}

impl ButtonMap {
    pub fn is_empty(&self) -> bool {
        !self.has_dirs() && self.click.is_none()
    }

    /// Whether any direction is bound. Without one, the button only clicks: no cursor freeze, no gesture.
    pub fn has_dirs(&self) -> bool {
        self.dirs.iter().any(Option::is_some)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub step: f64,
    pub back: ButtonMap,
    pub forward: ButtonMap,
}

pub const DEFAULT_STEP: f64 = 80.0;

impl Default for Config {
    fn default() -> Self {
        Config { step: DEFAULT_STEP, back: ButtonMap::default(), forward: ButtonMap::default() }
    }
}

impl Config {
    /// The px per step for a button's map.
    pub fn step_for(&self, map: &ButtonMap) -> f64 {
        map.step.unwrap_or(self.step)
    }

    /// The map for a CoreGraphics mouse button number (3 = back, 4 = forward), if it has any binding.
    pub fn button(&self, number: i64) -> Option<&ButtonMap> {
        let map = match number {
            3 => &self.back,
            4 => &self.forward,
            _ => return None,
        };
        (!map.is_empty()).then_some(map)
    }

    pub(crate) fn resolve(step: Option<f64>, back: Option<RawButton>, forward: Option<RawButton>) -> Result<Config, String> {
        let step = step.unwrap_or(DEFAULT_STEP);
        check_positive(step, "gestures.step")?;
        Ok(Config {
            step,
            back: back.map(|b| b.resolve("back")).transpose()?.unwrap_or_default(),
            forward: forward.map(|b| b.resolve("forward")).transpose()?.unwrap_or_default(),
        })
    }
}

pub(crate) fn check_positive(v: f64, name: &str) -> Result<(), String> {
    if v > 0.0 { Ok(()) } else { Err(format!("{name} must be > 0, got {v}")) }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawButton {
    step: Option<f64>,
    click: Option<RawAction>,
    left: Option<RawAction>,
    right: Option<RawAction>,
    up: Option<RawAction>,
    down: Option<RawAction>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAction {
    sh: Option<String>,
    key: Option<String>,
    media: Option<String>,
    button: Option<String>,
    #[serde(default)]
    once: bool,
    step: Option<f64>,
}

impl RawButton {
    fn resolve(self, name: &str) -> Result<ButtonMap, String> {
        let mut map = ButtonMap { step: self.step, ..Default::default() };
        if let Some(step) = self.step {
            check_positive(step, &format!("{name}.step"))?;
        }
        if let Some(raw) = self.click {
            map.click = Some(raw.act().map_err(|e| format!("{name}.click: {e}"))?);
        }
        for (dir, raw) in [(Dir::Left, self.left), (Dir::Right, self.right), (Dir::Up, self.up), (Dir::Down, self.down)] {
            if let Some(raw) = raw {
                let once = raw.once;
                let dname = format!("{name}.{}", format!("{dir:?}").to_lowercase());
                let step = raw.step;
                if let Some(step) = step {
                    check_positive(step, &format!("{dname}.step"))?;
                }
                let act = raw.act().map_err(|e| format!("{name}.{}: {e}", format!("{dir:?}").to_lowercase()))?;
                map.dirs[dir.index()] = Some(Binding { act, once, step });
            }
        }
        Ok(map)
    }
}

impl RawAction {
    fn act(self) -> Result<Act, String> {
        match (self.sh, self.key, self.media, self.button) {
            (Some(s), None, None, None) => Ok(Act::Sh(s)),
            (None, Some(k), None, None) => parse_key(&k),
            (None, None, Some(m), None) => media_code(&m).map(Act::Media).ok_or_else(|| {
                format!("unknown media key {m:?} (known: {})", MEDIA.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", "))
            }),
            (None, None, None, Some(b)) => match b.as_str() {
                "back" => Ok(Act::Button(3)),
                "forward" => Ok(Act::Button(4)),
                _ => Err(format!("unknown button {b:?} (known: back, forward)")),
            },
            _ => Err("set exactly one of sh, key, media, button".into()),
        }
    }
}

const MEDIA: &[(&str, i64)] = &[
    ("volume_up", 0),
    ("volume_down", 1),
    ("brightness_up", 2),
    ("brightness_down", 3),
    ("mute", 7),
    ("play", 16),
    ("next", 17),
    ("previous", 18),
];

fn media_code(name: &str) -> Option<i64> {
    MEDIA.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

// CGEventFlags bits.
const SHIFT: u64 = 1 << 17;
const CTRL: u64 = 1 << 18;
const OPT: u64 = 1 << 19;
const CMD: u64 = 1 << 20;
const FN: u64 = 1 << 23;

/// "cmd+shift+left", "ctrl+1", "f11": modifiers then one key, joined by '+'.
pub fn parse_key(spec: &str) -> Result<Act, String> {
    let parts: Vec<String> = spec.split('+').map(|p| p.trim().to_lowercase()).collect();
    let (key, mods) = parts.split_last().ok_or("empty key")?;
    let mut flags = 0;
    for m in mods {
        flags |= match m.as_str() {
            "cmd" | "command" => CMD,
            "shift" => SHIFT,
            "ctrl" | "control" => CTRL,
            "opt" | "option" | "alt" => OPT,
            "fn" => FN,
            _ => return Err(format!("unknown modifier {m:?} in {spec:?}")),
        };
    }
    let code = keycode(key).ok_or_else(|| format!("unknown key {key:?} in {spec:?}"))?;
    // Arrow and function keys carry the Fn flag when typed on a real keyboard; some apps check for it.
    if matches!(code, 0x7B..=0x7E) {
        flags |= FN;
    }
    Ok(Act::Key { code, flags })
}

/// ANSI virtual key codes (Carbon kVK_*).
fn keycode(k: &str) -> Option<u16> {
    let table: BTreeMap<&str, u16> = [
        ("a", 0x00), ("s", 0x01), ("d", 0x02), ("f", 0x03), ("h", 0x04), ("g", 0x05), ("z", 0x06), ("x", 0x07),
        ("c", 0x08), ("v", 0x09), ("b", 0x0B), ("q", 0x0C), ("w", 0x0D), ("e", 0x0E), ("r", 0x0F), ("y", 0x10),
        ("t", 0x11), ("1", 0x12), ("2", 0x13), ("3", 0x14), ("4", 0x15), ("6", 0x16), ("5", 0x17), ("=", 0x18),
        ("9", 0x19), ("7", 0x1A), ("-", 0x1B), ("8", 0x1C), ("0", 0x1D), ("]", 0x1E), ("o", 0x1F), ("u", 0x20),
        ("[", 0x21), ("i", 0x22), ("p", 0x23), ("return", 0x24), ("enter", 0x24), ("l", 0x25), ("j", 0x26),
        ("'", 0x27), ("k", 0x28), (";", 0x29), ("\\", 0x2A), (",", 0x2B), ("/", 0x2C), ("n", 0x2D), ("m", 0x2E),
        (".", 0x2F), ("tab", 0x30), ("space", 0x31), ("`", 0x32), ("delete", 0x33), ("backspace", 0x33),
        ("escape", 0x35), ("esc", 0x35), ("f5", 0x60), ("f6", 0x61), ("f7", 0x62), ("f3", 0x63), ("f8", 0x64),
        ("f9", 0x65), ("f11", 0x67), ("f10", 0x6D), ("f12", 0x6F), ("home", 0x73), ("pageup", 0x74),
        ("forwarddelete", 0x75), ("f4", 0x76), ("end", 0x77), ("f2", 0x78), ("pagedown", 0x79), ("f1", 0x7A),
        ("left", 0x7B), ("right", 0x7C), ("down", 0x7D), ("up", 0x7E),
    ]
    .into_iter()
    .collect();
    table.get(k).copied()
}

/// One hold of one button.
#[derive(Debug)]
pub struct Tracker {
    step: f64,
    x: f64,
    y: f64,
    horizontal: Option<bool>, // the locked axis, once movement reached a step
    once_done: [bool; 4],
}

impl Tracker {
    pub fn new(step: f64) -> Self {
        Tracker { step, x: 0.0, y: 0.0, horizontal: None, once_done: [false; 4] }
    }

    /// Whether the gesture locked to an axis. If not, releasing the button is a plain click.
    pub fn engaged(&self) -> bool {
        self.horizontal.is_some()
    }

    /// Pointer movement in screen pixels (y grows downward). Pushes the bindings to act on, in order.
    pub fn moved<'a>(&mut self, dx: f64, dy: f64, map: &'a ButtonMap, out: &mut Vec<&'a Binding>) {
        self.x += dx;
        self.y += dy;
        let dir_of = |horizontal: bool, v: f64| match (horizontal, v > 0.0) {
            (true, true) => Dir::Right,
            (true, false) => Dir::Left,
            (false, true) => Dir::Down,
            (false, false) => Dir::Up,
        };
        let step_of = |dir: Dir| map.dirs[dir.index()].as_ref().and_then(|b| b.step).unwrap_or(self.step);
        let horizontal = match self.horizontal {
            Some(h) => h,
            None => {
                let h = self.x.abs() >= self.y.abs();
                let v = if h { self.x } else { self.y };
                if v.abs() < step_of(dir_of(h, v)) {
                    return;
                }
                self.horizontal = Some(h);
                h
            }
        };
        loop {
            let acc = if horizontal { self.x } else { self.y };
            let dir = dir_of(horizontal, acc);
            let step = step_of(dir);
            if acc.abs() < step {
                break;
            }
            let rest = acc - acc.signum() * step;
            if horizontal { self.x = rest } else { self.y = rest }
            if let Some(b) = &map.dirs[dir.index()] {
                if b.once && self.once_done[dir.index()] {
                    continue;
                }
                self.once_done[dir.index()] = true;
                out.push(b);
            }
        }
    }
}
