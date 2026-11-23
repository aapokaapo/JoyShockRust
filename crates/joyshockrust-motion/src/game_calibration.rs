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
