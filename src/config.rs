//! The config file, ~/.config/wheelglide/config.toml: wheel scrolling ([scroll]) and side-button gestures
//! ([gestures], [back], [forward]). No OS calls; wheelglide re-reads the file when it changes.

use serde::Deserialize;

use crate::engine::Params;
use crate::gesture::{self, RawButton, check_positive};

#[derive(Clone, Debug, Default)]
pub struct Settings {
    pub scroll: Params,
    pub gestures: gesture::Config,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    scroll: Option<RawScroll>,
    gestures: Option<RawGestures>,
    back: Option<RawButton>,
    forward: Option<RawButton>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScroll {
    step: Option<f64>,
    decay: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGestures {
    step: Option<f64>,
}

pub fn parse(text: &str) -> Result<Settings, String> {
    let raw: Raw = toml::from_str(text).map_err(|e| e.to_string())?;
    let d = Params::default();
    let scroll = raw.scroll.map_or(d, |s| Params { step_px: s.step.unwrap_or(d.step_px), decay: s.decay.unwrap_or(d.decay) });
    check_positive(scroll.step_px, "scroll.step")?;
    if !(scroll.decay > 0.0 && scroll.decay < 1.0) {
        return Err(format!("scroll.decay must be between 0 and 1, got {}", scroll.decay));
    }
    let gestures = gesture::Config::resolve(raw.gestures.and_then(|g| g.step), raw.back, raw.forward)?;
    Ok(Settings { scroll, gestures })
}
