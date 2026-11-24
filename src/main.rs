//! wheelglide: replaces a notched mouse wheel's scrolling with trackpad-style scrolling.
//!
//! - IOHIDManager delivers the mouse's raw wheel reports (±1 / ±2 clicks, device timestamps) to the engine.
//! - An active CGEventTap at the HID level swallows that mouse's own wheel events, so apps only see ours.
//!   Shift+wheel becomes smooth horizontal scrolling (pixel deltas, no trackpad phases, so browsers don't
//!   read it as a page or sidebar swipe); scrolls with any other modifier held pass through untouched.
//! - A 60 Hz run-loop timer asks the engine for frames and posts them as continuous (trackpad-style)
//!   scroll events with scroll/momentum phases at the session level.
//!
//! Settings: ~/.config/wheelglide/config.toml (or WHEELGLIDE_CONFIG), see config.example.toml; re-read when it
//! changes, checked when a scroll starts and on each side-button press.
//! Environment: WHEELGLIDE_VENDOR / WHEELGLIDE_PRODUCT (default ProtoArc EM11 NL 0x25a7 / 0xfaa0),
//! WHEELGLIDE_DEBUG=1 to log every click and swallowed event.
//!
//! Side-button gestures (src/gesture.rs): the same tap claims the mouse's back / forward buttons when
//! the config binds them. While one is held the cursor is
//! frozen and drag deltas drive the gesture; a release that never moved a full step replays the click.

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr::{self, NonNull};
use std::sync::mpsc;
use std::time::SystemTime;

use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use objc2_foundation::NSPoint;

use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFDictionary, CGPoint, CFMachPort, CFNumber, CFRetained, CFRunLoop, CFRunLoopTimer, CFString,
    kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGAssociateMouseAndMouseCursorPosition, CGEvent, CGMouseButton, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType, CGRequestListenEventAccess, CGRequestPostEventAccess,
    CGScrollEventUnit,
};
use objc2_io_kit::{
    IOHIDAccessType, IOHIDCheckAccess, IOHIDManager, IOHIDRequestAccess, IOHIDRequestType, IOHIDValue,
    IOObjectRelease, IORegistryEntryCreateCFProperty, IORegistryEntryIDMatching, IOReturn, IOServiceGetMatchingService,
};
use wheelglide::engine::{Engine, Out, Params, Phase};
use wheelglide::config::{self, Settings};
use wheelglide::gesture::{Act, Config, Tracker};

// Scroll event fields (CGEventField raw values; 87 is undocumented: the sending service's registry ID).
const F_BUTTON_NUMBER: u32 = 3;
const F_MOUSE_DELTA_X: u32 = 4;
const F_MOUSE_DELTA_Y: u32 = 5;
const F_DELTA_AXIS1: u32 = 11;
const F_SOURCE_USER_DATA: u32 = 42;
const F_SENDER_ID: u32 = 87;
const F_IS_CONTINUOUS: u32 = 88;
const F_DELTA_AXIS2: u32 = 12;
const F_FIXED_DELTA_AXIS1: u32 = 93;
const F_FIXED_DELTA_AXIS2: u32 = 94;
const F_POINT_DELTA_AXIS1: u32 = 96;
const F_POINT_DELTA_AXIS2: u32 = 97;
const F_SCROLL_PHASE: u32 = 99;
const F_MOMENTUM_PHASE: u32 = 123;
const OUR_TAG: i64 = 0x57_4847_4c44; // "WHGLD": marks events we post

const FRAME_S: f64 = 1.0 / 60.0;

struct State {
    engine: Engine,
    vendor: i64,
    product: i64,
    natural: bool,
    horizontal: bool, // the axis of the current scroll: Shift held when it started
    debug: bool,
    hid_ok: bool,
    tap: Option<CFRetained<CFMachPort>>,
    source: CFRetained<CGEventSource>,
    timebase: (u64, u64),
    last_frame_ns: u64,
    subpixel: f64,
    outs: Vec<Out>,
    gestures: Config,
    config_path: PathBuf,
    config_mtime: Option<SystemTime>,
    config_error: Option<String>, // the last config error notified, so the same one isn't shown again
    held: Option<(i64, Tracker)>, // the side button being held for a gesture
    sh: mpsc::Sender<String>,
}

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut [u32; 2]) -> i32;
}

impl State {
    fn now_ns(&self) -> u64 {
        self.ticks_to_ns(unsafe { mach_absolute_time() })
    }
    fn ticks_to_ns(&self, t: u64) -> u64 {
        (t as u128 * self.timebase.0 as u128 / self.timebase.1 as u128) as u64
    }

    /// Whether `sender` (a scroll event's sending service) is the configured mouse. Asked of IOKit for each
    /// notched wheel event, so a reconnect (new registry IDs) never leaves anything stale.
    fn is_mouse(&self, sender: u64) -> bool {
        unsafe {
            let matching = IORegistryEntryIDMatching(sender);
            let svc = IOServiceGetMatchingService(0, matching.map(|m| CFRetained::cast_unchecked(m)));
            if svc == 0 {
                return false;
            }
            let ok = number_prop(svc, "VendorID") == Some(self.vendor) && number_prop(svc, "ProductID") == Some(self.product);
            IOObjectRelease(svc);
            ok
        }
    }

    fn post(&mut self, o: &Out) {
        self.subpixel += o.dy;
        let px = self.subpixel.round();
        self.subpixel -= px;
        if self.horizontal && px == 0.0 {
            return; // horizontal events carry no phases, so an empty one says nothing
        }
        if let Some(e) = scroll_event(&self.source, px, o, self.horizontal) {
            CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&e));
        }
    }

    /// Re-read the config if the file changed. Called only while no scroll or gesture is running, so new
    /// settings never change one midway.
    fn reload_config(&mut self) {
        let mtime = std::fs::metadata(&self.config_path).and_then(|m| m.modified()).ok();
        if mtime == self.config_mtime {
            return;
        }
        self.config_mtime = mtime;
        let path = self.config_path.display();
        let loaded = if mtime.is_none() {
            log(&format!("config: no {path}, using defaults (no gestures)"));
            Settings::default()
        } else {
            match std::fs::read_to_string(&self.config_path).map_err(|e| e.to_string()).and_then(|t| config::parse(&t)) {
                Ok(s) => s,
                Err(e) => {
                    log(&format!("config: {path}: {e} (keeping the previous settings)"));
                    let short = summarize(&e);
                    if self.config_error.as_deref() != Some(short.as_str()) {
                        notify("config.toml not loaded (previous settings kept)", &short);
                        self.config_error = Some(short);
                    }
                    return;
                }
            }
        };
        self.config_error = None;
        let p = loaded.scroll;
        log(&format!(
            "config: scroll step {} px, decay {}; gestures step {} px, back {}, forward {}",
            p.step_px,
            p.decay,
            loaded.gestures.step,
            if loaded.gestures.button(3).is_some() { "bound" } else { "untouched" },
            if loaded.gestures.button(4).is_some() { "bound" } else { "untouched" },
        ));
        self.engine.params = p;
        self.gestures = loaded.gestures;
    }

    fn act(&self, act: &Act) {
        if self.debug {
            log(&format!("gesture action {act:?}"));
        }
        match act {
            Act::Sh(cmd) => {
                let _ = self.sh.send(cmd.clone());
            }
            Act::Key { code, flags } => {
                for down in [true, false] {
                    if let Some(e) = CGEvent::new_keyboard_event(Some(&self.source), *code, down) {
                        CGEvent::set_flags(Some(&e), CGEventFlags(*flags));
                        CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&e));
                    }
                }
            }
            Act::Media(code) => post_media_key(*code),
            Act::Button(b) => {
                let at = CGEvent::new(None).map(|e| CGEvent::location(Some(&e))).unwrap_or(CGPoint::new(0.0, 0.0));
                self.click(*b, at);
            }
        }
    }

    /// The press that was held back: a plain back / forward click where the cursor is.
    fn click(&self, button: i64, at: CGPoint) {
        for ty in [CGEventType::OtherMouseDown, CGEventType::OtherMouseUp] {
            if let Some(e) = CGEvent::new_mouse_event(Some(&self.source), ty, at, CGMouseButton(button as u32)) {
                CGEvent::set_integer_value_field(Some(&e), CGEventField(F_BUTTON_NUMBER), button);
                CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&e));
            }
        }
    }

    fn end_hold(&mut self) {
        if self.held.take().is_some() {
            CGAssociateMouseAndMouseCursorPosition(true);
        }
    }

    /// Side-button events from the tap. Returns whether to swallow the event.
    fn button_event(&mut self, ty: CGEventType, e: &CGEvent) -> bool {
        let get = |f: u32| CGEvent::integer_value_field(Some(e), CGEventField(f));
        if get(F_SOURCE_USER_DATA) == OUR_TAG {
            return false;
        }
        let button = get(F_BUTTON_NUMBER);
        match ty {
            CGEventType::OtherMouseDown => {
                if self.held.is_some() {
                    return false;
                }
                self.reload_config();
                let Some(map) = self.gestures.button(button) else { return false };
                let step = self.gestures.step_for(map);
                if !self.is_mouse(get(F_SENDER_ID) as u64) {
                    return false;
                }
                if map.has_dirs() {
                    CGAssociateMouseAndMouseCursorPosition(false);
                }
                self.held = Some((button, Tracker::new(step)));
                if self.debug {
                    log(&format!("gesture start, button {button}"));
                }
                true
            }
            CGEventType::OtherMouseDragged => {
                let Some((held, tracker)) = self.held.as_mut() else { return false };
                if *held != button {
                    return false;
                }
                let Some(map) = self.gestures.button(button) else { return true };
                if !map.has_dirs() {
                    return false; // click-only button: the pointer moves as usual
                }
                let mut out = Vec::new();
                tracker.moved(get(F_MOUSE_DELTA_X) as f64, get(F_MOUSE_DELTA_Y) as f64, map, &mut out);
                let acts: Vec<Act> = out.into_iter().map(|b| b.act.clone()).collect();
                for a in &acts {
                    self.act(a);
                }
                true
            }
            CGEventType::OtherMouseUp => {
                let Some((held, tracker)) = self.held.as_ref() else { return false };
                if *held != button {
                    return false;
                }
                let engaged = tracker.engaged();
                self.end_hold();
                if !engaged {
                    match self.gestures.button(button).and_then(|m| m.click.clone()) {
                        Some(act) => self.act(&act),
                        None => self.click(button, CGEvent::location(Some(e))),
                    }
                }
                if self.debug {
                    log(&format!("gesture end, button {button}, {}", if engaged { "gesture" } else { "click" }));
                }
                true
            }
            _ => false,
        }
    }

    fn flush(&mut self) {
        let outs = std::mem::take(&mut self.outs);
        for o in &outs {
            self.post(o);
        }
        self.outs = outs;
        self.outs.clear();
    }
}

/// A continuous scroll event: `px` whole pixels, fixed-point "lines" of about dy / 10, and, when vertical,
/// trackpad phases. Horizontal events get no phases: with them, browsers treat a sideways scroll as a page or
/// sidebar swipe gesture.
/// Field order matters: setting the integer line delta makes CoreGraphics recompute the pixel and fixed-point
/// deltas from it, so it goes first and the precise values are written after it.
fn scroll_event(source: &CGEventSource, px: f64, o: &Out, horizontal: bool) -> Option<CFRetained<CGEvent>> {
    let (wheel1, wheel2) = if horizontal { (0, px as i32) } else { (px as i32, 0) };
    let ev = CGEvent::new_scroll_wheel_event2(Some(source), CGScrollEventUnit::Pixel, 2, wheel1, wheel2, 0)?;
    let e = Some(&*ev);
    let set_i = |f: u32, v: i64| CGEvent::set_integer_value_field(e, CGEventField(f), v);
    let (f_line, f_point, f_fixed) = if horizontal {
        (F_DELTA_AXIS2, F_POINT_DELTA_AXIS2, F_FIXED_DELTA_AXIS2)
    } else {
        (F_DELTA_AXIS1, F_POINT_DELTA_AXIS1, F_FIXED_DELTA_AXIS1)
    };
    set_i(F_IS_CONTINUOUS, 1);
    set_i(f_line, (o.dy / 10.0).trunc() as i64);
    set_i(f_point, px as i64);
    CGEvent::set_double_value_field(e, CGEventField(f_fixed), o.dy / 10.0);
    set_i(F_SOURCE_USER_DATA, OUR_TAG);
    if horizontal {
        return Some(ev);
    }
    let (scroll, momentum) = match o.phase {
        Phase::Began => (1, 0),
        Phase::Changed => (2, 0),
        Phase::Ended => (4, 0),
        Phase::MomentumBegan => (0, 1),
        Phase::Momentum => (0, 2),
        Phase::MomentumEnded => (0, 3),
    };
    set_i(F_SCROLL_PHASE, scroll);
    set_i(F_MOMENTUM_PHASE, momentum);
    Some(ev)
}

/// A config error in two lines: where (toml's first line) and what (its last line).
fn summarize(e: &str) -> String {
    let lines: Vec<&str> = e.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let s = match (lines.first(), lines.last()) {
        (Some(a), Some(b)) if a != b => format!("{a}: {b}"),
        (Some(a), _) => a.to_string(),
        _ => e.to_string(),
    };
    s.chars().take(200).collect()
}

/// A macOS notification, without waiting for it. Text goes in as an argument, never into the script.
fn notify(title: &str, body: &str) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        let out = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", "on run argv", "-e", "display notification (item 2 of argv) with title \"wheelglide\" subtitle (item 1 of argv)", "-e", "end run", &title, &body])
            .output();
        match out {
            Ok(o) if o.status.success() => {}
            Ok(o) => log(&format!("notification failed ({}): {}", o.status, String::from_utf8_lossy(&o.stderr).trim())),
            Err(e) => log(&format!("notification could not start: {e}")),
        }
    });
}

/// A media / system key (NX_KEYTYPE_*) press and release, the way the keyboard sends them, so macOS acts
/// on it and shows its own overlay.
fn post_media_key(code: i64) {
    for state in [0xa_i64, 0xb] { // key down, key up
        let ev = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
            NSEventType::SystemDefined,
            NSPoint::new(0.0, 0.0),
            NSEventModifierFlags((state as usize) << 8),
            0.0,
            0,
            None,
            8, // NX_SUBTYPE_AUX_CONTROL_BUTTONS
            ((code << 16) | (state << 8)) as isize,
            -1,
        );
        if let Some(cg) = ev.and_then(|e| e.CGEvent()) {
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&cg));
        }
    }
}

/// Runs gesture shell commands one at a time, in order, off the event thread.
fn sh_worker() -> mpsc::Sender<String> {
    let (tx, rx) = mpsc::channel::<String>();
    let home = std::env::var("HOME").unwrap_or_default();
    let path = format!("{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin:{}", std::env::var("PATH").unwrap_or_default());
    std::thread::spawn(move || {
        for cmd in rx {
            let status = std::process::Command::new("/bin/sh")
                .args(["-c", &cmd])
                .env("PATH", &path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .status();
            match status {
                Ok(s) if !s.success() => log(&format!("gesture command failed ({s}): {cmd}")),
                Err(e) => log(&format!("gesture command could not start ({e}): {cmd}")),
                _ => {}
            }
        }
    });
    tx
}

/// `wheelglide --selftest`: build events without posting them and print what a reader of each field would see.
fn selftest(source: &CGEventSource) {
    for (dy, phase) in [(12.0, Phase::Changed), (8.4, Phase::Changed), (-3.2, Phase::Momentum), (0.4, Phase::Changed), (40.0, Phase::Began)] {
        let o = Out { dy, phase };
        let e = scroll_event(source, dy.round(), &o, false).expect("event");
        let e = Some(&*e);
        let gi = |f: u32| CGEvent::integer_value_field(e, CGEventField(f));
        let gd = |f: u32| CGEvent::double_value_field(e, CGEventField(f));
        println!(
            "dy {dy:>6}: point {:>4}  fixed {:>7.3}  line {:>3}  continuous {}  phase {} momentum {}",
            gi(F_POINT_DELTA_AXIS1),
            gd(F_FIXED_DELTA_AXIS1),
            gi(F_DELTA_AXIS1),
            gi(F_IS_CONTINUOUS),
            gi(F_SCROLL_PHASE),
            gi(F_MOMENTUM_PHASE)
        );
    }
    let e = scroll_event(source, 12.0, &Out { dy: 12.0, phase: Phase::Changed }, true).expect("event");
    let e = Some(&*e);
    let gi = |f: u32| CGEvent::integer_value_field(e, CGEventField(f));
    println!(
        "horizontal dy 12: point2 {}  fixed2 {:.3}  line2 {}  point1 {}  continuous {}  phase {} momentum {}",
        gi(F_POINT_DELTA_AXIS2),
        CGEvent::double_value_field(e, CGEventField(F_FIXED_DELTA_AXIS2)),
        gi(F_DELTA_AXIS2),
        gi(F_POINT_DELTA_AXIS1),
        gi(F_IS_CONTINUOUS),
        gi(F_SCROLL_PHASE),
        gi(F_MOMENTUM_PHASE)
    );
}

fn number_prop(svc: u32, key: &str) -> Option<i64> {
    let key = CFString::from_str(key);
    let v = unsafe { IORegistryEntryCreateCFProperty(svc, Some(&key), None, 0) }?;
    v.downcast_ref::<CFNumber>().and_then(|n| n.as_i64())
}

fn log(msg: &str) {
    eprintln!("wheelglide: {msg}");
}

const MODIFIERS: CGEventFlags = CGEventFlags::MaskShift
    .union(CGEventFlags::MaskControl)
    .union(CGEventFlags::MaskAlternate)
    .union(CGEventFlags::MaskCommand);

/// wheelglide handles the wheel with no modifier (vertical) or Shift alone (horizontal); any other modifier
/// combination passes through untouched (e.g. Nehir's Command+Shift column scrolling).
fn ours_to_handle(flags: CGEventFlags) -> bool {
    let mods = flags.intersection(MODIFIERS);
    mods.is_empty() || mods == CGEventFlags::MaskShift
}

unsafe extern "C-unwind" fn tap_cb(_: CGEventTapProxy, ty: CGEventType, ev: NonNull<CGEvent>, info: *mut c_void) -> *mut CGEvent {
    let st = unsafe { &mut *(info as *mut State) };
    if ty == CGEventType::TapDisabledByTimeout || ty == CGEventType::TapDisabledByUserInput {
        st.end_hold(); // never leave the cursor frozen
        if let Some(t) = &st.tap {
            CGEvent::tap_enable(t, true);
        }
        return ev.as_ptr();
    }
    if matches!(ty, CGEventType::OtherMouseDown | CGEventType::OtherMouseDragged | CGEventType::OtherMouseUp) {
        return if st.button_event(ty, unsafe { ev.as_ref() }) { ptr::null_mut() } else { ev.as_ptr() };
    }
    let e = Some(unsafe { ev.as_ref() });
    let get = |f: u32| CGEvent::integer_value_field(e, CGEventField(f));
    // Cheap checks first: trackpad (continuous) events, ours and modifier scrolls pass without an IOKit lookup.
    let notched = get(F_IS_CONTINUOUS) == 0 && get(F_SOURCE_USER_DATA) != OUR_TAG;
    if notched && st.hid_ok && ours_to_handle(CGEvent::flags(e)) && st.is_mouse(get(F_SENDER_ID) as u64) {
        if st.debug {
            log(&format!("swallow wheel event lines={}", get(F_DELTA_AXIS1)));
        }
        return ptr::null_mut();
    }
    ev.as_ptr()
}

unsafe extern "C-unwind" fn hid_value_cb(ctx: *mut c_void, _: IOReturn, _: *mut c_void, value: NonNull<IOHIDValue>) {
    let st = unsafe { &mut *(ctx as *mut State) };
    let value = unsafe { value.as_ref() };
    let el = value.element();
    if el.usage_page() != 0x01 || el.usage() != 0x38 {
        return;
    }
    let v = value.integer_value() as i64;
    if v == 0 {
        return;
    }
    // Another modifier is held: the tap lets the system event through, so the engine must not scroll too.
    let flags = CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState);
    if !ours_to_handle(flags) {
        return;
    }
    let horizontal = flags.intersects(CGEventFlags::MaskShift);
    if horizontal != st.horizontal {
        // Shift pressed or released between clicks: finish the current scroll on its own axis first.
        let mut outs = std::mem::take(&mut st.outs);
        st.engine.cancel(&mut outs);
        st.outs = outs;
        st.flush();
        st.subpixel = 0.0;
        st.horizontal = horizontal;
    }
    if st.engine.is_idle() {
        st.reload_config();
    }
    let t = st.ticks_to_ns(value.time_stamp());
    let dir = if st.natural { -(v.signum() as f64) } else { v.signum() as f64 };
    if st.debug {
        log(&format!("click {v:+} at {t}"));
    }
    let mut outs = std::mem::take(&mut st.outs);
    st.engine.click(dir, v.unsigned_abs() as u32, t, &mut outs);
    st.outs = outs;
    st.flush();
}



fn env_hex(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| i64::from_str_radix(v.trim_start_matches("0x"), 16).ok())
        .unwrap_or(default)
}

/// Natural scrolling is on unless the global preference explicitly says 0.
fn natural_scrolling() -> bool {
    std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "com.apple.swipescrolldirection"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() != "0")
        .unwrap_or(true)
}

fn main() {
    let mut tb = [0u32; 2];
    unsafe { mach_timebase_info(&mut tb) };
    let source = CGEventSource::new(CGEventSourceStateID::Private).expect("event source");
    CGEventSource::set_user_data(Some(&source), OUR_TAG);
    if std::env::args().any(|a| a == "--selftest") {
        selftest(&source);
        return;
    }
    let st: &mut State = Box::leak(Box::new(State {
        engine: Engine::new(Params::default()),
        vendor: env_hex("WHEELGLIDE_VENDOR", 0x25a7),
        product: env_hex("WHEELGLIDE_PRODUCT", 0xfaa0),
        natural: natural_scrolling(),
        horizontal: false,
        debug: std::env::var("WHEELGLIDE_DEBUG").is_ok_and(|v| v == "1"),
        hid_ok: false,
        tap: None,
        source,
        timebase: (tb[0] as u64, tb[1] as u64),
        last_frame_ns: 0,
        subpixel: 0.0,
        outs: Vec::new(),
        gestures: Config::default(),
        config_path: std::env::var_os("WHEELGLIDE_CONFIG").map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/wheelglide/config.toml")
        }),
        config_mtime: None,
        config_error: None,
        held: None,
        sh: sh_worker(),
    }));
    CGAssociateMouseAndMouseCursorPosition(true); // in case a previous run died mid-gesture
    st.reload_config();
    log(&format!(
        "starting: natural scrolling {}, device {:04x}:{:04x}",
        st.natural, st.vendor, st.product
    ));
    let ctx = st as *mut State as *mut c_void;

    // Permissions: Accessibility (active tap + posting) and Input Monitoring (raw HID).
    let post_ok = CGRequestPostEventAccess();
    let listen_ok = CGRequestListenEventAccess();
    let hid_access = IOHIDCheckAccess(IOHIDRequestType::ListenEvent);
    if hid_access != IOHIDAccessType::Granted {
        IOHIDRequestAccess(IOHIDRequestType::ListenEvent);
    }
    log(&format!("permissions: accessibility={post_ok} input-monitoring={listen_ok} hid={hid_access:?}"));

    let rl = CFRunLoop::current().expect("run loop");

    // Raw wheel reports from the mouse.
    let mgr = IOHIDManager::new(None, 0);
    let (vid, pid) = (CFNumber::new_i64(st.vendor), CFNumber::new_i64(st.product));
    let (kv, kp) = (CFString::from_str("VendorID"), CFString::from_str("ProductID"));
    let matching = CFDictionary::<CFString, CFNumber>::from_slices(&[&*kv, &*kp], &[&*vid, &*pid]);
    unsafe {
        mgr.set_device_matching(Some(matching.as_opaque()));
        mgr.register_input_value_callback(Some(hid_value_cb), ctx);
        mgr.schedule_with_run_loop(&rl, kCFRunLoopDefaultMode.unwrap());
    }
    let opened = mgr.open(0);
    st.hid_ok = opened == 0;
    log(&format!("HID manager open: {opened:#x}"));

    // Swallow the mouse's own wheel events, and its side buttons when gestures bind them.
    let mask = [CGEventType::ScrollWheel, CGEventType::OtherMouseDown, CGEventType::OtherMouseDragged, CGEventType::OtherMouseUp]
        .iter()
        .fold(0u64, |m, t| m | 1 << t.0);
    let Some(tap) = (unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(tap_cb),
            ctx,
        )
    }) else {
        log("could not create the event tap: grant Accessibility to wheelglide, then it restarts");
        std::process::exit(1);
    };
    let src = CFMachPort::new_run_loop_source(None, Some(&tap), 0).expect("tap source");
    rl.add_source(Some(&src), unsafe { kCFRunLoopCommonModes });
    st.tap = Some(tap);

    // Frames.
    st.last_frame_ns = st.now_ns();
    let frame = block2::RcBlock::new(move |_: *mut CFRunLoopTimer| {
        let st = unsafe { &mut *(ctx as *mut State) };
        let now = st.now_ns();
        let dt_ms = ((now - st.last_frame_ns) as f64 / 1e6).min(50.0);
        st.last_frame_ns = now;
        if st.engine.is_idle() {
            return;
        }
        let mut outs = std::mem::take(&mut st.outs);
        st.engine.frame(now, dt_ms, &mut outs);
        st.outs = outs;
        st.flush();
    });
    let timer = unsafe { CFRunLoopTimer::with_handler(None, CFAbsoluteTimeGetCurrent(), FRAME_S, 0, 0, Some(&frame)) }
        .expect("frame timer");
    rl.add_timer(Some(&timer), unsafe { kCFRunLoopCommonModes });

    log("running");
    CFRunLoop::run();
}
