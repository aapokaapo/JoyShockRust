//! TOML profile schema and compilation shared by the editor and bridge.

use std::error::Error;

use joyshockrust_core::{touch_region::GridSize, trigger::TriggerSide};
use joyshockrust_mapping::{
    gamepad::{self, ButtonRemaps, OutputKind},
    gyro_gate::{self, TriggerGate},
    keyboard,
    touchpad::{self, Grid},
};
use joyshockrust_motion::stick_response::StickCurve;
use serde::Deserialize;

use crate::{
    PROFILE_VERSION,
    controller::{ControllerId, ModelSelection, Selection},
};

pub mod gyro;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileProfile {
    version: u32,
    bridge: Option<BridgeFile>,
    stick_curve: Option<f32>,
    stick_curve_left: Option<f32>,
    stick_curve_right: Option<f32>,
    #[serde(default)]
    remap_buttons: Vec<String>,
    #[serde(default)]
    layer_remaps: Vec<String>,
    gyro: Option<Gyro>,
    touchpad: Option<Touchpad>,
    keyboard: Option<keyboard::Settings>,
    #[serde(default)]
    triggers: TriggerProfiles,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TriggerProfiles {
    l2: TriggerProfile,
    r2: TriggerProfile,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TriggerProfile {
    thresholds: Option<[u8; 4]>,
    full_travel_percent: Option<u8>,
    travel_limit_percent: Option<u8>,
    partial_button: Option<String>,
    full_button: Option<String>,
    full_pull_exclusive: bool,
    keep_partial: bool,
    adaptive: AdaptiveTriggerEffect,
}

pub use joyshockrust_core::trigger::AnalogTriggerRoute;

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

impl TriggerProfile {
    fn analog_route(&self, side: TriggerSide) -> AnalogTriggerRoute {
        analog_route(side, self.full_pull_exclusive, self.full_button.as_deref())
    }

    fn full_travel(&self) -> Result<Option<u8>, &'static str> {
        trigger_travel_limit(self.travel_limit_percent.or(self.full_travel_percent))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Gyro {
    #[serde(default)]
    calibrate: bool,
    aim_mode: Option<String>,
    calibration_hold: Option<CalibrationHold>,
    flick_stick: Option<f64>,
    flick_stick_side: Option<String>,
    flick_engage_percent: Option<f64>,
    flick_duration_ms: Option<f64>,
    flick_axial_threshold_deg: Option<f64>,
    flick_steering_sensitivity: Option<f64>,
    threshold_dps: Option<f64>,
    accel_start_dps: Option<f64>,
    accel_end_dps: Option<f64>,
    accel_multiplier: Option<f64>,
    accel_enabled: Option<bool>,
    sensitivity_x: Option<f32>,
    sensitivity_y: Option<f32>,
    real_world_calibration: Option<f64>,
    in_game_sens: Option<f64>,
    binding: Option<GyroBinding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CalibrationHold {
    button: String,
    hold_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(
    tag = "mode",
    content = "input",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
enum GyroBinding {
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
struct Touchpad {
    columns: usize,
    rows: usize,
    #[serde(default)]
    regions: Vec<RegionConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegionConfig {
    column: usize,
    row: usize,
    touch: Option<String>,
    click: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub enum AdaptiveTriggerEffect {
    #[default]
    Off,
    Resistance {
        start: u8,
        strength: u8,
    },
}

impl AdaptiveTriggerEffect {
    pub fn validate(self) -> Result<Self, &'static str> {
        if let Self::Resistance { start, strength } = self {
            joyshockrust_core::trigger::validate_resistance(start, strength)?;
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BridgeOptions {
    pub gamepad: OutputKind,
    pub rumble: bool,
    pub virtual_gamepad: bool,
}

impl Default for BridgeOptions {
    fn default() -> Self {
        Self {
            gamepad: OutputKind::default(),
            rumble: false,
            virtual_gamepad: true,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BridgeFile {
    // Allow existing profiles to load while their controller settings move out of the profile.
    #[serde(rename = "transport")]
    _legacy_transport: Option<Selection>,
    #[serde(rename = "model")]
    _legacy_model: Option<ModelSelection>,
    #[serde(rename = "controller_id")]
    _legacy_controller_id: Option<ControllerId>,
    gamepad: Option<OutputKind>,
    rumble: Option<bool>,
    virtual_gamepad: Option<bool>,
}

impl BridgeFile {
    fn outputs(self) -> Option<BridgeSettings> {
        if self.gamepad.is_none() && self.rumble.is_none() && self.virtual_gamepad.is_none() {
            return None;
        }
        Some(BridgeSettings {
            gamepad: self.gamepad,
            rumble: self.rumble,
            virtual_gamepad: self.virtual_gamepad,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BridgeSettings {
    pub gamepad: Option<OutputKind>,
    pub rumble: Option<bool>,
    pub virtual_gamepad: Option<bool>,
}

impl BridgeSettings {
    pub fn startup(self) -> BridgeOptions {
        BridgeOptions {
            gamepad: self.gamepad.unwrap_or_default(),
            rumble: self.rumble.unwrap_or_default(),
            virtual_gamepad: self.virtual_gamepad.unwrap_or(true),
        }
    }
}

#[derive(Default)]
pub struct RuntimeOptions {
    pub bridge: Option<BridgeSettings>,
    pub stick_curve: Option<StickCurve>,
    pub remaps: ButtonRemaps,
    pub gyro: Option<gyro::Options>,
    pub touchpad: Option<Grid>,
    pub keyboard: Option<keyboard::Config>,
    pub grab_touchpad: bool,
    pub trigger_stages: Option<[gamepad::TriggerStages; 2]>,
    pub trigger_full_travel: [Option<u8>; 2],
    pub trigger_analog_routes: [gamepad::AnalogTriggerRoute; 2],
    pub adaptive_triggers: [AdaptiveTriggerEffect; 2],
}

pub fn parse(
    source: &str,
    keys: &impl keyboard::KeyResolver,
) -> Result<RuntimeOptions, Box<dyn Error>> {
    let profile: FileProfile = toml::from_str(source)?;
    if profile.version != PROFILE_VERSION {
        return Err(format!(
            "unsupported profile version {}, expected {PROFILE_VERSION}",
            profile.version
        )
        .into());
    }
    let stick_curve = StickCurve::from_profile(
        profile.stick_curve,
        profile.stick_curve_left,
        profile.stick_curve_right,
    )?;
    let mut remaps = ButtonRemaps::default();
    for rule in &profile.remap_buttons {
        remaps.add(rule)?;
    }
    for rule in &profile.layer_remaps {
        remaps.add_layer(rule)?;
    }
    let touchpad = profile.touchpad.map(resolve_touchpad).transpose()?;
    let gyro = profile
        .gyro
        .map(|gyro| gyro::resolve(gyro, touchpad.as_ref()))
        .transpose()?;
    let grab_touchpad = profile
        .keyboard
        .as_ref()
        .is_some_and(|keyboard| keyboard.grab_touchpad);
    let trigger_stages = [
        resolve_trigger(&profile.triggers.l2, gyro_gate::TriggerSide::Left)?,
        resolve_trigger(&profile.triggers.r2, gyro_gate::TriggerSide::Right)?,
    ];
    let adaptive_triggers = [
        profile.triggers.l2.adaptive.validate()?,
        profile.triggers.r2.adaptive.validate()?,
    ];
    let keyboard = profile
        .keyboard
        .map(|keyboard| keyboard.resolve(touchpad.as_ref(), trigger_stages, keys))
        .transpose()?;
    Ok(RuntimeOptions {
        bridge: profile.bridge.and_then(BridgeFile::outputs),
        stick_curve,
        remaps,
        gyro,
        touchpad,
        keyboard,
        grab_touchpad,
        trigger_stages: trigger_stages
            .iter()
            .any(|stage| stage.targets != [None; 2])
            .then_some(trigger_stages),
        trigger_full_travel: [
            profile.triggers.l2.full_travel()?,
            profile.triggers.r2.full_travel()?,
        ],
        trigger_analog_routes: [
            profile
                .triggers
                .l2
                .analog_route(gyro_gate::TriggerSide::Left),
            profile
                .triggers
                .r2
                .analog_route(gyro_gate::TriggerSide::Right),
        ],
        adaptive_triggers,
    })
}

fn resolve_trigger(
    settings: &TriggerProfile,
    side: gyro_gate::TriggerSide,
) -> Result<gamepad::TriggerStages, &'static str> {
    let thresholds = settings
        .thresholds
        .unwrap_or(joyshockrust_core::DEFAULT_TRIGGER_THRESHOLDS);
    joyshockrust_core::validate_trigger_thresholds(thresholds)?;
    let [partial, partial_release, full, full_release] = thresholds;
    let analog_full = settings.full_pull_exclusive
        && !matches!(
            settings.analog_route(side),
            gamepad::AnalogTriggerRoute::Off
        );
    if analog_full && settings.partial_button.is_some() {
        return Err("a partial pull button requires a non-trigger full pull target");
    }
    let travel = if relative_trigger_stages(
        settings.full_pull_exclusive,
        settings.travel_limit_percent.is_some(),
    ) {
        settings.full_travel()?.unwrap_or(100)
    } else {
        100
    };
    Ok(gamepad::TriggerStages {
        lower: TriggerGate::from_percent(side, partial, partial_release).within_travel(travel),
        upper: TriggerGate::from_percent(side, full, full_release).within_travel(travel),
        targets: if analog_full {
            [None; 2]
        } else {
            [
                settings
                    .partial_button
                    .as_deref()
                    .map(gamepad::parse_target)
                    .transpose()?,
                settings
                    .full_button
                    .as_deref()
                    .map(gamepad::parse_target)
                    .transpose()?,
            ]
        },
        keep_partial: settings.keep_partial,
        travel_percent: travel,
    })
}

fn resolve_touchpad(settings: Touchpad) -> Result<Grid, &'static str> {
    let size = GridSize::new(settings.rows, settings.columns)
        .ok_or("touchpad grid must have between 1 and 25 cells")?;
    touchpad::resolve(
        size,
        settings.regions.iter().map(|region| {
            (
                region.row,
                region.column,
                region.touch.as_deref(),
                region.click.as_deref(),
            )
        }),
    )
}
