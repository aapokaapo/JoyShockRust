use serde::Deserialize;

use joyshockrust_core::trigger::TriggerSide;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile<Bridge, Gyro, Touchpad, Keyboard, Effect: Default> {
    pub version: u32,
    pub bridge: Option<Bridge>,
    pub stick_curve: Option<f32>,
    pub stick_curve_left: Option<f32>,
    pub stick_curve_right: Option<f32>,
    #[serde(default)]
    pub remap_buttons: Vec<String>,
    #[serde(default)]
    pub layer_remaps: Vec<String>,
    pub gyro: Option<Gyro>,
    pub touchpad: Option<Touchpad>,
    pub keyboard: Option<Keyboard>,
    #[serde(default)]
    pub triggers: TriggerProfiles<Effect>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TriggerProfiles<Effect: Default> {
    pub l2: TriggerProfile<Effect>,
    pub r2: TriggerProfile<Effect>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TriggerProfile<Effect: Default> {
    pub thresholds: Option<[u8; 4]>,
    pub full_travel_percent: Option<u8>,
    pub travel_limit_percent: Option<u8>,
    pub partial_button: Option<String>,
    pub full_button: Option<String>,
    pub full_pull_exclusive: bool,
    pub keep_partial: bool,
    pub adaptive: Effect,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnalogTriggerRoute {
    #[default]
    Same,
    Other,
    Off,
}

pub fn analog_route(side: TriggerSide, exclusive: bool, full: Option<&str>) -> AnalogTriggerRoute {
    use AnalogTriggerRoute::{Off, Other, Same};
    if !exclusive {
        return Same;
    }
    match full {
        None => Same,
        Some(target) => match TriggerSide::parse(target) {
            Ok(target) if target == side => Same,
            Ok(_) => Other,
            Err(_) => Off,
        },
    }
}

pub const fn relative_trigger_stages(exclusive: bool, has_travel_limit: bool) -> bool {
    exclusive || has_travel_limit
}

pub fn trigger_travel_limit(configured: Option<u8>) -> Result<Option<u8>, &'static str> {
    match configured {
        None | Some(100) => Ok(None),
        Some(value @ 1..=99) => Ok(Some(value)),
        _ => Err("trigger travel limit must be between 1 and 100"),
    }
}

impl<Effect: Default> TriggerProfile<Effect> {
    pub fn analog_route(&self, side: TriggerSide) -> AnalogTriggerRoute {
        analog_route(side, self.full_pull_exclusive, self.full_button.as_deref())
    }

    pub fn full_travel(&self) -> Result<Option<u8>, &'static str> {
        trigger_travel_limit(self.travel_limit_percent.or(self.full_travel_percent))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gyro {
    #[serde(default)]
    pub calibrate: bool,
    pub aim_mode: Option<String>,
    pub calibration_hold: Option<CalibrationHold>,
    pub flick_stick: Option<f64>,
    pub flick_stick_side: Option<String>,
    pub flick_engage_percent: Option<f64>,
    pub flick_duration_ms: Option<f64>,
    pub flick_axial_threshold_deg: Option<f64>,
    pub flick_steering_sensitivity: Option<f64>,
    pub threshold_dps: Option<f64>,
    pub accel_start_dps: Option<f64>,
    pub accel_end_dps: Option<f64>,
    pub accel_multiplier: Option<f64>,
    pub accel_enabled: Option<bool>,
    pub sensitivity_x: Option<f32>,
    pub sensitivity_y: Option<f32>,
    pub real_world_calibration: Option<f64>,
    pub in_game_sens: Option<f64>,
    pub binding: Option<GyroBinding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationHold {
    pub button: String,
    pub hold_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(
    tag = "mode",
    content = "input",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum GyroBinding {
    Hold(String),
    DisableWhileHeld(String),
    Toggle(String),
    TapHold(String),
    DoubleTap(String),
    Chord(String),
    Trigger(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Touchpad {
    pub columns: usize,
    pub rows: usize,
    #[serde(default)]
    pub regions: Vec<RegionConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionConfig {
    pub column: usize,
    pub row: usize,
    pub touch: Option<String>,
    pub click: Option<String>,
}
