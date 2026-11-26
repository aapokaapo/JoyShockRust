use joyshockrust_core::Stick;

use crate::flick_stick::FlickStick;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GameCalibration(f64);

impl Default for GameCalibration {
    fn default() -> Self {
        Self(1.0)
    }
}

impl GameCalibration {
    pub fn new(real_world_calibration: f64, in_game_sens: f64) -> Result<Self, &'static str> {
        if !positive(real_world_calibration) {
            return Err("gyro.real_world_calibration must be finite and greater than zero");
        }
        if !positive(in_game_sens) {
            return Err("gyro.in_game_sens must be finite and greater than zero");
        }
        let factor = real_world_calibration / in_game_sens;
        // Keep effective pointer scales representable even at the maximum
        // gyro and Flick Stick sensitivities and a large angle per report.
        if !positive(factor) || factor > 10_000.0 {
            return Err(
                "real_world_calibration / in_game_sens must be greater than 0 and at most 10000",
            );
        }
        Ok(Self(factor))
    }

    pub fn scale(self, sensitivity: f64) -> f64 {
        sensitivity * self.0
    }
}

pub fn positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

pub fn calculate(counts: i32, in_game_sens: f64, turns: f64) -> Result<f64, &'static str> {
    if !positive(turns) {
        return Err("measured turns must be finite and greater than zero");
    }
    let real_world_calibration = (f64::from(counts) / 360.0 / turns) * in_game_sens;
    GameCalibration::new(real_world_calibration, in_game_sens)?;
    Ok(real_world_calibration)
}

/// One right-stick calibration sweep with explicit confirm-button state.
pub struct StickSweep {
    flick: FlickStick,
    measuring: bool,
    confirm_held: bool,
    counts: i64,
    dial: Dial,
}

#[derive(Default)]
pub struct Dial {
    travel: f64,
}

impl Dial {
    pub fn rotate(&mut self, degrees: Option<f64>) -> bool {
        const STEP: f64 = 15.0;
        let Some(degrees) = degrees else {
            self.travel = 0.0;
            return false;
        };
        self.travel += degrees;
        if self.travel.abs() >= STEP {
            self.travel %= STEP;
            return true;
        }
        false
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Confirmation {
    None,
    Started,
    CenterStick,
    NoMotion,
    Finished(u64),
}

impl StickSweep {
    pub fn new(scale: f64) -> Self {
        Self {
            flick: FlickStick::new(scale)
                .with_axial_threshold(0.0)
                .with_steering_sensitivity(1.0),
            measuring: false,
            confirm_held: true,
            counts: 0,
            dial: Dial::default(),
        }
    }

    pub fn advance(
        &mut self,
        stick: Stick,
        ticks: u32,
        confirm: bool,
    ) -> (i32, Confirmation, bool) {
        let pressed = confirm && !self.confirm_held;
        self.confirm_held = confirm;
        let mut detent = false;
        let delta = if self.measuring {
            self.flick.advance_with_rotation(stick, ticks, |degrees| {
                detent = self.dial.rotate(degrees);
            })
        } else {
            0
        };
        self.counts += i64::from(delta);
        let confirmation = if !pressed {
            Confirmation::None
        } else if !self.flick.is_settled(stick) {
            Confirmation::CenterStick
        } else if !self.measuring {
            self.measuring = true;
            self.flick.advance(stick, ticks);
            Confirmation::Started
        } else if self.counts == 0 {
            Confirmation::NoMotion
        } else {
            Confirmation::Finished(self.counts.unsigned_abs())
        };
        (delta, confirmation, detent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurement_recovers_the_game_factor_for_different_turn_counts() {
        let calibration = calculate(9000, 2.0, 1.25).unwrap();
        assert!((calibration - 40.0).abs() < 1e-10);
        let effective = GameCalibration::new(calibration, 2.0).unwrap();
        assert!((effective.scale(1.0) - 20.0).abs() < 1e-10);
        assert!((effective.scale(3.0) - 60.0).abs() < 1e-10);
        assert!(calculate(9000, 2.0, 0.0).is_err());
        assert!(GameCalibration::new(1.0, f64::INFINITY).is_err());
    }

    #[test]
    fn stick_measurement_sums_two_full_rotations_and_uses_whole_turns() {
        let up = Stick { x: 0.0, y: -1.0 };
        let right = Stick { x: 1.0, y: 0.0 };
        let down = Stick { x: 0.0, y: 1.0 };
        let left = Stick { x: -1.0, y: 0.0 };
        let center = Stick::default();
        let mut sweep = StickSweep::new(20.0);
        let mut ticks = 0;
        let mut advance = |stick, options| {
            ticks += 30_000;
            sweep.advance(stick, ticks, options)
        };

        assert_eq!(advance(up, true), (0, Confirmation::None, false));
        advance(center, false);
        assert_eq!(advance(center, true), (0, Confirmation::Started, false));
        assert_eq!(advance(up, true), (0, Confirmation::None, false));
        for direction in [right, down, left, up].into_iter().cycle().take(8) {
            assert_eq!(advance(direction, false), (1800, Confirmation::None, true));
        }
        assert_eq!(advance(up, true), (0, Confirmation::CenterStick, false));
        for _ in 0..3 {
            advance(center, false);
        }
        let (delta, confirmation, detent) = advance(center, true);
        assert!(!detent);
        assert_eq!(delta, 0);
        assert_eq!(confirmation, Confirmation::Finished(14_400));
        assert_eq!(calculate(14_400, 2.0, 2.0).unwrap(), 40.0);
    }

    #[test]
    fn measurement_can_make_fine_adjustments_next_to_forward() {
        let mut sweep = StickSweep::new(20.0);
        let center = Stick::default();
        sweep.advance(center, 0, false);
        assert_eq!(sweep.advance(center, 30_000, true).1, Confirmation::Started);
        sweep.advance(Stick { x: 0.0, y: -1.0 }, 60_000, false);
        let (x, forward) = 2.0_f32.to_radians().sin_cos();
        let (counts, _, _) = sweep.advance(Stick { x, y: -forward }, 90_000, false);
        assert!((39..=40).contains(&counts));
    }

    #[test]
    fn dial_uses_relative_steps_and_resets_when_the_stick_recenters() {
        let mut dial = Dial::default();
        assert!(!dial.rotate(Some(10.0)));
        assert!(!dial.rotate(Some(-2.0)));
        assert!(dial.rotate(Some(8.0)));
        assert!(!dial.rotate(Some(-14.0)));
        assert!(dial.rotate(Some(-3.0)));
        assert!(!dial.rotate(None));
        assert!(!dial.rotate(Some(-10.0)));
    }
}
