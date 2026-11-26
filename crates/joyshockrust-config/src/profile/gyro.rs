use std::{error::Error, time::Duration};

use joyshockrust_core::{Button, StickSide};
use joyshockrust_mapping::{
    gyro_gate::{self, Binding, DigitalInput, TriggerGate},
    touchpad::Grid,
};
use joyshockrust_motion::{
    AimMode,
    aim_response::{
        DEFAULT_FLICK_AXIAL_THRESHOLD_DEG, DEFAULT_FLICK_STEERING_SENSITIVITY, GyroResponse,
        MAX_FLICK_DURATION_MS, MIN_FLICK_DURATION_MS, MIN_FLICK_ENGAGE_PERCENT,
        validate_flick_axial_threshold,
    },
    game_calibration::GameCalibration,
    pointer::{FlickOptions, PointerOptions},
    sensitivity::GyroSensitivity as Sensitivity,
};

use super::{CalibrationHold, Gyro, GyroBinding};

#[derive(Clone, Copy, Default, PartialEq)]
pub struct Options {
    pub calibrate: bool,
    pub aim_mode: AimMode,
    pub calibration_hold: Option<HoldBinding>,
    pub binding: Binding,
    pub flick_stick: Option<f64>,
    pub flick_stick_side: StickSide,
    pub flick_engage_percent: Option<f64>,
    pub flick_duration_ms: Option<f64>,
    pub flick_axial_threshold_deg: Option<f64>,
    pub flick_steering_sensitivity: Option<f64>,
    pub response: GyroResponse,
    pub sensitivity: Sensitivity,
    pub game_calibration: GameCalibration,
}

#[derive(Clone, Copy, PartialEq)]
pub struct HoldBinding {
    pub button: Button,
    pub duration: Duration,
}

impl Options {
    fn gyro_scale(self) -> [f64; 2] {
        self.sensitivity
            .axes()
            .map(|scale| self.game_calibration.scale(f64::from(scale)))
    }

    pub fn flick_axial_threshold(self) -> f64 {
        self.flick_axial_threshold_deg
            .unwrap_or(DEFAULT_FLICK_AXIAL_THRESHOLD_DEG)
    }

    fn flick_steering_sensitivity(self) -> f64 {
        self.flick_steering_sensitivity
            .unwrap_or(DEFAULT_FLICK_STEERING_SENSITIVITY)
    }

    fn flick_scale(self) -> Option<f64> {
        self.flick_stick
            .map(|scale| self.game_calibration.scale(scale))
    }

    pub fn pointer_options(self) -> PointerOptions {
        PointerOptions {
            mode: self.aim_mode,
            response: self.response,
            sensitivity: self.gyro_scale(),
            flick: self.flick_scale().map(|scale| FlickOptions {
                scale,
                side: self.flick_stick_side,
                engage_percent: self.flick_engage_percent,
                duration_ms: self.flick_duration_ms,
                axial_threshold_deg: self.flick_axial_threshold(),
                steering_sensitivity: self.flick_steering_sensitivity(),
            }),
        }
    }

    pub fn can_reconfigure(self, next: Self) -> bool {
        self.calibrate == next.calibrate
            && self.calibration_hold == next.calibration_hold
            && self.binding == next.binding
    }
}

pub fn validate_flick_scale(scale: f64) -> Result<f64, &'static str> {
    if !(0.01..=100.0).contains(&scale) {
        return Err("Flick Stick sensitivity must be between 0.01 and 100 counts per degree");
    }
    Ok(scale)
}

fn resolve_calibration_hold(settings: CalibrationHold) -> Result<HoldBinding, &'static str> {
    let button = gyro_gate::button_by_name(&settings.button)
        .ok_or("gyro.calibration_hold.button must name a physical controller button")?;
    let ms = settings.hold_ms.unwrap_or(1500);
    if !(250..=10_000).contains(&ms) {
        return Err("gyro.calibration_hold.hold_ms must be between 250 and 10000");
    }
    Ok(HoldBinding {
        button,
        duration: std::time::Duration::from_millis(ms),
    })
}

pub(super) fn resolve(settings: Gyro, touchpad: Option<&Grid>) -> Result<Options, Box<dyn Error>> {
    let flick_stick = settings.flick_stick.map(validate_flick_scale).transpose()?;
    if settings
        .flick_engage_percent
        .is_some_and(|value| !(MIN_FLICK_ENGAGE_PERCENT..=100.0).contains(&value))
    {
        return Err("Flick Stick engagement must be between 30 and 100 percent".into());
    }
    if settings
        .flick_duration_ms
        .is_some_and(|value| !(MIN_FLICK_DURATION_MS..=MAX_FLICK_DURATION_MS).contains(&value))
    {
        return Err("Flick Stick duration must be between 10 and 500 ms".into());
    }
    if settings
        .flick_steering_sensitivity
        .is_some_and(|value| !(0.0..=1.0).contains(&value))
    {
        return Err("Flick Stick steering sensitivity must be between 0 and 1".into());
    }
    let resolve_input = |name: &str| -> Result<DigitalInput, String> {
        if let Some(spec) = name.strip_prefix("touch:") {
            let region = touchpad
                .ok_or("gyro touch binding requires a [touchpad] grid")?
                .parse_region(spec)?;
            return Ok(DigitalInput::Touch(region));
        }
        gyro_gate::button_by_name(name)
            .map(DigitalInput::Button)
            .ok_or_else(|| format!("unknown gyro binding button: {name}"))
    };
    let binding = match settings.binding {
        None => Binding::Always,
        Some(GyroBinding::Hold(name)) => Binding::Hold(resolve_input(&name)?),
        Some(GyroBinding::DisableWhileHeld(name)) => {
            Binding::DisableWhileHeld(resolve_input(&name)?)
        }
        Some(GyroBinding::Toggle(name)) => Binding::Toggle(resolve_input(&name)?),
        Some(GyroBinding::TapHold(name)) => Binding::TapHold(resolve_input(&name)?),
        Some(GyroBinding::DoubleTap(name)) => Binding::DoubleTap(resolve_input(&name)?),
        Some(GyroBinding::Chord(spec)) => gyro_gate::parse_chord(&spec, resolve_input)?,
        Some(GyroBinding::Trigger(spec)) => Binding::AnalogTrigger(TriggerGate::parse(&spec)?),
    };
    let [threshold, start, end, multiplier] = GyroResponse::default().values();
    let mut response = GyroResponse::new(
        settings.threshold_dps.unwrap_or(threshold),
        settings.accel_start_dps.unwrap_or(start),
        settings.accel_end_dps.unwrap_or(end),
        settings.accel_multiplier.unwrap_or(multiplier),
    )?;
    if let Some(enabled) = settings.accel_enabled {
        response = response.with_acceleration_enabled(enabled);
    }
    Ok(Options {
        calibrate: settings.calibrate,
        aim_mode: settings
            .aim_mode
            .as_deref()
            .map(str::parse)
            .transpose()?
            .unwrap_or_default(),
        calibration_hold: settings
            .calibration_hold
            .map(resolve_calibration_hold)
            .transpose()?,
        binding,
        flick_stick,
        flick_engage_percent: settings.flick_engage_percent,
        flick_duration_ms: settings.flick_duration_ms,
        flick_stick_side: match settings.flick_stick_side.as_deref() {
            Some("left") => joyshockrust_core::StickSide::Left,
            None | Some("right") => joyshockrust_core::StickSide::Right,
            Some(_) => return Err("gyro.flick_stick_side must be left or right".into()),
        },
        flick_axial_threshold_deg: settings
            .flick_axial_threshold_deg
            .map(validate_flick_axial_threshold)
            .transpose()?,
        flick_steering_sensitivity: settings.flick_steering_sensitivity,
        response,
        game_calibration: GameCalibration::new(
            settings.real_world_calibration.unwrap_or(1.0),
            settings.in_game_sens.unwrap_or(1.0),
        )?,
        sensitivity: Sensitivity::from_profile(settings.sensitivity_x, settings.sensitivity_y)?,
    })
}
