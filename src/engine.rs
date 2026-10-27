//! The scrolling rule. No OS calls: clicks and frames go in, scroll amounts with trackpad phases come out.
//!
//! - Every click scrolls one fixed step.
//! - A click is *fast* if it arrived in the very next Bluetooth packet (or one packet carried 2 clicks).
//!   One fast click makes a flick.
//! - When a flick's next packet doesn't come, the content keeps moving at the flick's own speed and
//!   loses (1 - decay) of that speed every 60 Hz frame, until it moves less than half a pixel a frame.
//! - Any click stops a glide.
//!
//! All times are device time in nanoseconds (the HID timestamp clock, which is also `mach_absolute_time`).

/// The mouse's Bluetooth report interval. A hardware fact, not a tuning knob.
pub const PACKET_NS: u64 = 7_500_000;
const FRAME_MS: f64 = 1000.0 / 60.0;

#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub step_px: f64,
    pub decay: f64,
}

impl Default for Params {
    fn default() -> Self {
        Params { step_px: 80.0, decay: 0.975 }
    }
}

/// Trackpad-style phase of an output event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Began,
    Changed,
    Ended,
    MomentumBegan,
    Momentum,
    MomentumEnded,
}

#[derive(Clone, Copy, Debug)]
pub struct Out {
    /// Pixels to scroll, signed like a scroll event's delta (positive moves content down).
    pub dy: f64,
    pub phase: Phase,
}

#[derive(Default)]
pub struct Engine {
    pub params: Params,
    last_t: Option<u64>,
    run: u32,
    flick_start: u64,
    flick_px: f64,
    dir: f64,
    pending: f64,
    touching: bool,
    glide: Option<Glide>,
}

struct Glide {
    v: f64, // px/s, signed
    started: bool,
}

impl Engine {
    pub fn new(params: Params) -> Self {
        Engine { params, ..Default::default() }
    }

    /// A wheel report: `clicks` (1 or 2) in direction `dir` (+1 / -1, already in scroll-delta sign), at device time `t`.
    pub fn click(&mut self, dir: f64, clicks: u32, t: u64, out: &mut Vec<Out>) {
        self.stop_glide(out);
        let step = self.params.step_px;
        let gap = self.last_t.map(|lt| t.saturating_sub(lt));
        let fast = gap.is_some_and(|g| 2 * g < 3 * PACKET_NS || clicks > 1);
        if fast {
            if self.run == 0 {
                self.flick_start = self.last_t.unwrap_or(t);
                self.flick_px = step;
            }
            self.run += 1;
            self.flick_px += clicks as f64 * step;
        } else {
            self.run = 0;
        }
        self.dir = dir;
        self.pending += dir * clicks as f64 * step;
        self.last_t = Some(t);
    }

    /// One display frame at device time `now`, `dt_ms` after the previous frame.
    pub fn frame(&mut self, now: u64, dt_ms: f64, out: &mut Vec<Out>) {
        // The flick's next packet didn't come: the finger let go.
        if self.run > 0 && self.last_t.is_some_and(|lt| 2 * now.saturating_sub(lt) >= 3 * PACKET_NS) {
            let ms = ((self.last_t.unwrap() - self.flick_start) as f64 / 1e6).max(PACKET_NS as f64 / 1e6);
            self.run = 0;
            self.end_touch(out);
            self.glide = Some(Glide { v: self.dir * self.flick_px / ms * 1000.0, started: false });
        }

        // Ease out the remaining click steps; flush the last fraction so each click moves exactly one step.
        let mut step = 0.0;
        if self.pending != 0.0 {
            step = self.pending * (1.0 - 0.7f64.powf(dt_ms / FRAME_MS));
            if (self.pending - step).abs() < 0.5 {
                step = self.pending;
            }
            self.pending -= step;
        }

        if let Some(g) = self.glide.as_mut() {
            let phase = if g.started { Phase::Momentum } else { Phase::MomentumBegan };
            g.started = true;
            out.push(Out { dy: g.v * dt_ms / 1000.0 + step, phase });
            g.v *= self.params.decay.powf(dt_ms / FRAME_MS);
            if g.v.abs() * FRAME_MS / 1000.0 < 0.5 {
                self.glide = None;
                out.push(Out { dy: 0.0, phase: Phase::MomentumEnded });
            }
        } else if step != 0.0 {
            let phase = if self.touching { Phase::Changed } else { Phase::Began };
            self.touching = true;
            out.push(Out { dy: step, phase });
        } else if self.run == 0 {
            self.end_touch(out);
        }
    }

    /// Drops pending steps and any glide and closes the touch, e.g. when the scroll axis changes.
    pub fn cancel(&mut self, out: &mut Vec<Out>) {
        self.stop_glide(out);
        self.pending = 0.0;
        self.run = 0;
        self.end_touch(out);
    }

    pub fn is_idle(&self) -> bool {
        self.glide.is_none() && self.pending == 0.0 && !self.touching && self.run == 0
    }

    fn stop_glide(&mut self, out: &mut Vec<Out>) {
        if let Some(g) = self.glide.take() {
            if g.started {
                out.push(Out { dy: 0.0, phase: Phase::MomentumEnded });
            }
        }
    }

    fn end_touch(&mut self, out: &mut Vec<Out>) {
        if self.touching {
            self.touching = false;
            out.push(Out { dy: 0.0, phase: Phase::Ended });
        }
    }
}
