use wheelglide::gesture::{Act, Config, Tracker, parse_key};

fn parse(text: &str) -> Result<Config, String> {
    wheelglide::config::parse(text).map(|s| s.gestures)
}

const CFG: &str = r#"
[gestures]
step = 50
[back]
up = { media = "volume_up" }
down = { media = "volume_down" }
[forward]
left = { sh = "nehirctl command focus left", once = true }
right = { key = "cmd+]" }
"#;

fn run(cfg: &Config, button: i64, moves: &[(f64, f64)]) -> (Vec<Act>, bool) {
    let map = cfg.button(button).unwrap();
    let mut t = Tracker::new(cfg.step_for(map));
    let mut acts = Vec::new();
    for &(dx, dy) in moves {
        let mut out = Vec::new();
        t.moved(dx, dy, map, &mut out);
        acts.extend(out.into_iter().map(|b| b.act.clone()));
    }
    (acts, t.engaged())
}

#[test]
fn small_wobble_is_a_click() {
    let cfg = parse(CFG).unwrap();
    let (acts, engaged) = run(&cfg, 3, &[(10.0, -10.0), (-20.0, 15.0), (12.0, 20.0)]);
    assert!(acts.is_empty());
    assert!(!engaged);
}

#[test]
fn slide_repeats_and_reverses_on_the_locked_axis() {
    let cfg = parse(CFG).unwrap();
    // 160 px up (3 steps, with some sideways drift), then 110 px back down (2 steps).
    let mut moves = vec![(3.0, -10.0); 16];
    moves.extend(vec![(-2.0, 10.0); 11]);
    let (acts, engaged) = run(&cfg, 3, &moves);
    assert!(engaged);
    let up = Act::Media(0);
    let down = Act::Media(1);
    assert_eq!(acts, vec![up.clone(), up.clone(), up, down.clone(), down]);
}

#[test]
fn once_fires_once_and_unbound_directions_do_nothing() {
    let cfg = parse(CFG).unwrap();
    let (acts, engaged) = run(&cfg, 4, &[(-10.0, 0.0); 30]);
    assert!(engaged);
    assert_eq!(acts, vec![Act::Sh("nehirctl command focus left".into())]);
    // Locked horizontal: vertical movement afterwards is ignored.
    let (acts, _) = run(&cfg, 4, &[(-60.0, 0.0), (0.0, -500.0)]);
    assert_eq!(acts.len(), 1);
}

#[test]
fn unconfigured_button_passes_through() {
    let cfg = parse("[back]\nup = { media = \"mute\" }").unwrap();
    assert!(cfg.button(3).is_some());
    assert!(cfg.button(4).is_none());
    assert!(cfg.button(2).is_none());
}

#[test]
fn config_errors_are_reported() {
    assert!(parse("[back]\nup = { media = \"louder\" }").unwrap_err().contains("back.up"));
    assert!(parse("[back]\nup = { sh = \"a\", key = \"b\" }").is_err());
    assert!(parse("[back]\nupp = { sh = \"a\" }").is_err());
    assert!(parse("[gestures]\nstep = 0").is_err());
    assert!(parse("step = 50").is_err()); // top-level step is ambiguous: [scroll] or [gestures]
}

#[test]
fn keys() {
    assert_eq!(parse_key("cmd+]").unwrap(), Act::Key { code: 0x1E, flags: 1 << 20 });
    assert_eq!(parse_key("Ctrl+Left").unwrap(), Act::Key { code: 0x7B, flags: (1 << 18) | (1 << 23) });
    assert!(parse_key("hyper+x").is_err());
}

#[test]
fn shipped_example_parses() {
    let text = std::fs::read_to_string(format!("{}/config.example.toml", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let s = wheelglide::config::parse(&text).unwrap();
    assert_eq!(s.scroll.step_px, 80.0);
    assert!(s.gestures.button(3).is_some());
}

#[test]
fn per_button_step() {
    let cfg = parse("[gestures]\nstep = 80\n[back]\nup = { media = \"mute\" }\n[forward]\nstep = 200\nleft = { sh = \"x\" }").unwrap();
    assert_eq!(cfg.step_for(cfg.button(3).unwrap()), 80.0);
    assert_eq!(cfg.step_for(cfg.button(4).unwrap()), 200.0);
    let (acts, _) = run(&cfg, 4, &[(-10.0, 0.0); 19]);
    assert!(acts.is_empty());
    let (acts, _) = run(&cfg, 4, &[(-10.0, 0.0); 20]);
    assert_eq!(acts.len(), 1);
    assert!(parse("[forward]\nstep = -1\nleft = { sh = \"x\" }").unwrap_err().contains("forward.step"));
}

#[test]
fn once_is_per_direction_only() {
    assert!(parse("[forward]\nonce = true\nleft = { sh = \"l\" }").is_err());
}

#[test]
fn scroll_settings() {
    let s = wheelglide::config::parse("[scroll]\nstep = 60").unwrap();
    assert_eq!((s.scroll.step_px, s.scroll.decay), (60.0, 0.975));
    assert!(wheelglide::config::parse("[scroll]\ndecay = 1.0").is_err());
    let s = wheelglide::config::parse("").unwrap();
    assert!(s.gestures.button(3).is_none());
}

#[test]
fn click_binding() {
    let cfg = parse("[back]\nclick = { key = \"cmd+w\" }\n[forward]\nclick = { media = \"play\" }\nup = { media = \"volume_up\" }").unwrap();
    let back = cfg.button(3).unwrap();
    assert!(!back.has_dirs());
    assert_eq!(back.click, Some(Act::Key { code: 0x0D, flags: 1 << 20 }));
    let fwd = cfg.button(4).unwrap();
    assert!(fwd.has_dirs());
    assert_eq!(fwd.click, Some(Act::Media(16)));
    assert!(parse("[back]\nclick = { media = \"nope\" }").unwrap_err().contains("back.click"));
}

#[test]
fn per_direction_step_and_button_action() {
    let cfg = parse(r#"
[gestures]
step = 80
[back]
click = { media = "play" }
left = { button = "back", once = true }
right = { button = "forward", once = true }
[forward]
up = { media = "volume_up" }
down = { media = "volume_down" }
left = { sh = "l", step = 160, once = true }
right = { sh = "r", step = 160, once = true }
"#).unwrap();
    let back = cfg.button(3).unwrap();
    assert_eq!(back.click, Some(Act::Media(16)));
    // A long slide left on back is one "back" click.
    let (acts, _) = run(&cfg, 3, &[(-10.0, 0.0); 40]);
    assert_eq!(acts, vec![Act::Button(3)]);
    // Forward: volume every 80 px, Nehir needs 160 px.
    let (acts, _) = run(&cfg, 4, &[(0.0, -10.0); 24]);
    assert_eq!(acts, vec![Act::Media(0); 3]);
    let (acts, engaged) = run(&cfg, 4, &[(-10.0, 0.0); 15]);
    assert!(acts.is_empty() && !engaged);
    let (acts, _) = run(&cfg, 4, &[(-10.0, 0.0); 40]);
    assert_eq!(acts, vec![Act::Sh("l".into())]);
    assert!(parse("[back]\nleft = { button = \"middle\" }").is_err());
    assert!(parse("[back]\nleft = { button = \"back\", step = 0 }").unwrap_err().contains("back.left.step"));
}
