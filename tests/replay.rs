//! Replays recorded wheel takes (raw HID value + device timestamp per report) through the engine,
//! with 120 Hz frames in between, and checks the three agreed behaviours.

use wheelglide::engine::{Engine, Out, Params, Phase};

struct Result {
    clicks: u32,
    moved: f64,      // total |px| scrolled
    glides: u32,     // MomentumBegan count
    glide_px: f64,   // |px| scrolled during momentum frames
    phases_ok: bool, // Began..Ended / MomentumBegan..MomentumEnded nesting never broken
}

fn replay(name: &str) -> Result {
    let text = std::fs::read_to_string(format!("{}/tests/data/{name}.txt", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let reports: Vec<(i64, u64)> = text
        .lines()
        .map(|l| {
            let mut it = l.split_whitespace();
            (it.next().unwrap().parse().unwrap(), it.next().unwrap().parse().unwrap())
        })
        .collect();

    let mut engine = Engine::new(Params::default());
    let mut outs: Vec<Out> = Vec::new();
    let frame_ns = 1_000_000_000 / 120;
    let end = reports.last().unwrap().1 + 6_000_000_000;
    let mut now = reports[0].1;
    let mut next = 0;
    while now < end {
        while next < reports.len() && reports[next].1 <= now {
            let (v, t) = reports[next];
            engine.click(-(v.signum() as f64), v.unsigned_abs() as u32, t, &mut outs);
            next += 1;
        }
        engine.frame(now, frame_ns as f64 / 1e6, &mut outs);
        now += frame_ns;
    }

    let (mut touching, mut gliding, mut phases_ok) = (false, false, true);
    let (mut moved, mut glides, mut glide_px) = (0.0, 0, 0.0);
    for o in &outs {
        moved += o.dy.abs();
        match o.phase {
            Phase::Began => { phases_ok &= !touching && !gliding; touching = true; }
            Phase::Changed => phases_ok &= touching,
            Phase::Ended => { phases_ok &= touching; touching = false; }
            Phase::MomentumBegan => { phases_ok &= !touching && !gliding; gliding = true; glides += 1; glide_px += o.dy.abs(); }
            Phase::Momentum => { phases_ok &= gliding; glide_px += o.dy.abs(); }
            Phase::MomentumEnded => { phases_ok &= gliding; gliding = false; }
        }
    }
    let clicks = reports.iter().map(|r| r.0.unsigned_abs() as u32).sum();
    Result { clicks, moved, glides, glide_px, phases_ok: phases_ok && !touching && !gliding }
}

#[test]
fn single_clicks_move_exactly_one_step_each_and_never_glide() {
    let r = replay("single");
    assert!(r.phases_ok);
    assert_eq!(r.glides, 0);
    assert!((r.moved - r.clicks as f64 * Params::default().step_px).abs() < 1e-6, "moved {} for {} clicks", r.moved, r.clicks);
}

#[test]
fn smooth_rolls_are_essentially_one_step_per_click() {
    let r = replay("smooth");
    assert!(r.phases_ok);
    let steps = r.clicks as f64 * Params::default().step_px;
    // An occasional one-packet gap starts a glide that the next roll click stops almost at once.
    assert!(r.glide_px < 0.1 * steps, "glide px {} vs steps {}", r.glide_px, steps);
}

#[test]
fn flicks_glide() {
    let r = replay("flick");
    assert!(r.phases_ok);
    assert!(r.glides >= 10, "only {} glides", r.glides);
    assert!(r.glide_px > r.clicks as f64 * Params::default().step_px, "glides should carry well past the click steps");
}

#[test]
fn a_click_stops_a_glide() {
    let mut e = Engine::new(Params::default());
    let mut out = Vec::new();
    let ms = 1_000_000u64;
    e.click(1.0, 1, 0, &mut out);
    e.click(1.0, 1, 7 * ms + ms / 2, &mut out); // next packet: fast
    for i in 1..=20 { e.frame(7 * ms + ms / 2 + i * 8 * ms, 8.0, &mut out); }
    assert!(out.iter().any(|o| o.phase == Phase::MomentumBegan));
    e.click(-1.0, 1, 400 * ms, &mut out);
    assert_eq!(out.last().unwrap().phase, Phase::MomentumEnded);
}
