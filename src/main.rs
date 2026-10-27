//! wheelglide: replaces a notched mouse wheel's scrolling with trackpad-style scrolling.
//!
//! - IOHIDManager delivers the mouse's raw wheel reports (±1 / ±2 clicks, device timestamps) to the engine.
//! - An active CGEventTap at the HID level swallows that mouse's own wheel events, so apps only see ours.
//!   Scrolls with a modifier key held (Shift, Ctrl, Option, Cmd) pass through untouched.
//! - A 60 Hz run-loop timer asks the engine for frames and posts them as continuous (trackpad-style)
//!   scroll events with scroll/momentum phases at the session level.
//!
//! Environment: WHEELGLIDE_STEP (px per click, default 80), WHEELGLIDE_DECAY (glide decay per 60 Hz
//! frame, default 0.975), WHEELGLIDE_VENDOR / WHEELGLIDE_PRODUCT (default ProtoArc EM11 NL 0x25a7 / 0xfaa0),
//! WHEELGLIDE_DEBUG=1 to log every click and swallowed event.

use std::ffi::c_void;
use std::ptr::{self, NonNull};

use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFDictionary, CFMachPort, CFNumber, CFRetained, CFRunLoop, CFRunLoopTimer, CFString,
    kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType, CGRequestListenEventAccess, CGRequestPostEventAccess,
    CGScrollEventUnit,
};
use objc2_io_kit::{
    IOHIDAccessType, IOHIDCheckAccess, IOHIDManager, IOHIDRequestAccess, IOHIDRequestType, IOHIDValue,
    IOObjectRelease, IORegistryEntryCreateCFProperty, IORegistryEntryIDMatching, IOReturn, IOServiceGetMatchingService,
};
use wheelglide::engine::{Engine, Out, Params, Phase};

// Scroll event fields (CGEventField raw values; 87 is undocumented: the sending service's registry ID).
const F_DELTA_AXIS1: u32 = 11;
const F_SOURCE_USER_DATA: u32 = 42;
const F_SENDER_ID: u32 = 87;
const F_IS_CONTINUOUS: u32 = 88;
const F_FIXED_DELTA_AXIS1: u32 = 93;
const F_POINT_DELTA_AXIS1: u32 = 96;
const F_SCROLL_PHASE: u32 = 99;
const F_MOMENTUM_PHASE: u32 = 123;
const OUR_TAG: i64 = 0x57_4847_4c44; // "WHGLD": marks events we post

const FRAME_S: f64 = 1.0 / 60.0;

struct State {
    engine: Engine,
    vendor: i64,
    product: i64,
    natural: bool,
    debug: bool,
    hid_ok: bool,
    tap: Option<CFRetained<CFMachPort>>,
    source: CFRetained<CGEventSource>,
    timebase: (u64, u64),
    last_frame_ns: u64,
    subpixel: f64,
    outs: Vec<Out>,
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

    /// Registry IDs of every IOHIDEventService for our vendor/product: the "sender" stamped on its events.
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
        if let Some(e) = scroll_event(&self.source, px, o) {
            CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&e));
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

/// A continuous, trackpad-style scroll event: `px` whole pixels, fixed-point "lines" of about dy / 10, and phases.
/// Field order matters: setting the integer line delta makes CoreGraphics recompute the pixel and fixed-point
/// deltas from it, so it goes first and the precise values are written after it.
fn scroll_event(source: &CGEventSource, px: f64, o: &Out) -> Option<CFRetained<CGEvent>> {
    let ev = CGEvent::new_scroll_wheel_event2(Some(source), CGScrollEventUnit::Pixel, 1, px as i32, 0, 0)?;
    let e = Some(&*ev);
    let set_i = |f: u32, v: i64| CGEvent::set_integer_value_field(e, CGEventField(f), v);
    set_i(F_IS_CONTINUOUS, 1);
    set_i(F_DELTA_AXIS1, (o.dy / 10.0).trunc() as i64);
    set_i(F_POINT_DELTA_AXIS1, px as i64);
    CGEvent::set_double_value_field(e, CGEventField(F_FIXED_DELTA_AXIS1), o.dy / 10.0);
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
    set_i(F_SOURCE_USER_DATA, OUR_TAG);
    Some(ev)
}

/// `wheelglide --selftest`: build events without posting them and print what a reader of each field would see.
fn selftest(source: &CGEventSource) {
    for (dy, phase) in [(12.0, Phase::Changed), (8.4, Phase::Changed), (-3.2, Phase::Momentum), (0.4, Phase::Changed), (40.0, Phase::Began)] {
        let o = Out { dy, phase };
        let e = scroll_event(source, dy.round(), &o).expect("event");
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

unsafe extern "C-unwind" fn tap_cb(_: CGEventTapProxy, ty: CGEventType, ev: NonNull<CGEvent>, info: *mut c_void) -> *mut CGEvent {
    let st = unsafe { &mut *(info as *mut State) };
    if ty == CGEventType::TapDisabledByTimeout || ty == CGEventType::TapDisabledByUserInput {
        if let Some(t) = &st.tap {
            CGEvent::tap_enable(t, true);
        }
        return ev.as_ptr();
    }
    let e = Some(unsafe { ev.as_ref() });
    let get = |f: u32| CGEvent::integer_value_field(e, CGEventField(f));
    // Cheap checks first: trackpad (continuous) events, ours and modifier scrolls pass without an IOKit lookup.
    let notched = get(F_IS_CONTINUOUS) == 0 && get(F_SOURCE_USER_DATA) != OUR_TAG;
    if notched && st.hid_ok && !CGEvent::flags(e).intersects(MODIFIERS) && st.is_mouse(get(F_SENDER_ID) as u64) {
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
    // A modifier is held: the tap lets the system event through, so the engine must not scroll too.
    if CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState).intersects(MODIFIERS) {
        return;
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


fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
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
    let params = Params {
        step_px: env_f64("WHEELGLIDE_STEP", Params::default().step_px),
        decay: env_f64("WHEELGLIDE_DECAY", Params::default().decay),
    };
    let source = CGEventSource::new(CGEventSourceStateID::Private).expect("event source");
    CGEventSource::set_user_data(Some(&source), OUR_TAG);
    if std::env::args().any(|a| a == "--selftest") {
        selftest(&source);
        return;
    }
    let st: &mut State = Box::leak(Box::new(State {
        engine: Engine::new(params),
        vendor: env_hex("WHEELGLIDE_VENDOR", 0x25a7),
        product: env_hex("WHEELGLIDE_PRODUCT", 0xfaa0),
        natural: natural_scrolling(),
        debug: std::env::var("WHEELGLIDE_DEBUG").is_ok_and(|v| v == "1"),
        hid_ok: false,
        tap: None,
        source,
        timebase: (tb[0] as u64, tb[1] as u64),
        last_frame_ns: 0,
        subpixel: 0.0,
        outs: Vec::new(),
    }));
    log(&format!(
        "starting: step {} px, decay {}, natural scrolling {}, device {:04x}:{:04x}",
        params.step_px, params.decay, st.natural, st.vendor, st.product
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

    // Swallow the mouse's own wheel events.
    let mask = 1u64 << CGEventType::ScrollWheel.0;
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
