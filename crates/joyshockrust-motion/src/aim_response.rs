//! Optional response shaping applied after calibrated motion integration.

pub const DEFAULT_FLICK_AXIAL_THRESHOLD_DEG: f64 = 8.0;
pub const MAX_FLICK_AXIAL_THRESHOLD_DEG: f64 = 30.0;
pub const MAX_GYRO_RATE_DPS: f64 = 2_000.0;
pub const MAX_GYRO_ACCEL_MULTIPLIER: f64 = 10.0;

pub fn validate_flick_axial_threshold(degrees: f64) -> Result<f64, &'static str> {
    if (0.0..=MAX_FLICK_AXIAL_THRESHOLD_DEG).contains(&degrees) {
        Ok(degrees)
    } else {
        Err("Flick Stick axial threshold must be between 0 and 30 degrees")
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GyroResponse {
    min_speed: f64,
    accel_start: f64,
    accel_end: f64,
    multiplier: f64,
}

impl Default for GyroResponse {
    fn default() -> Self {
        Self {
            min_speed: 0.0,
            accel_start: 30.0,
            accel_end: 180.0,
            multiplier: 1.0,
        }
    }
}

impl GyroResponse {
    pub fn new(
        min_speed: f64,
        accel_start: f64,
        accel_end: f64,
        multiplier: f64,
    ) -> Result<Self, &'static str> {
        if ![min_speed, accel_start, accel_end]
            .into_iter()
            .all(|speed| (0.0..=MAX_GYRO_RATE_DPS).contains(&speed))
        {
            return Err("Gyro speed thresholds must be between 0 and 2000 degrees per second");
        }
        if accel_start >= accel_end {
            return Err("Gyro acceleration end must exceed its start threshold");
        }
        if !(1.0..=MAX_GYRO_ACCEL_MULTIPLIER).contains(&multiplier) {
            return Err("Gyro acceleration multiplier must be between 1 and 10");
        }
        Ok(Self {
            min_speed,
            accel_start,
            accel_end,
            multiplier,
        })
    }

    pub fn values(self) -> [f64; 4] {
        [
            self.min_speed,
            self.accel_start,
            self.accel_end,
            self.multiplier,
        ]
    }

    pub fn gain(self, angular_velocity: [f32; 3]) -> f64 {
        if self.min_speed == 0.0 && self.multiplier == 1.0 {
            return 1.0;
        }
        let [pitch, yaw, _] = angular_velocity.map(f64::from);
        let speed = pitch.hypot(yaw);
        if speed < self.min_speed {
            return 0.0;
        }
        let amount =
            ((speed - self.accel_start) / (self.accel_end - self.accel_start)).clamp(0.0, 1.0);
        1.0 + amount * (self.multiplier - 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gyro_threshold_and_acceleration_follow_pitch_yaw_speed() {
        let curve = GyroResponse::new(3.0, 20.0, 100.0, 3.0).unwrap();
        for (pitch, yaw, gain) in [
            (0.5, 1.0, 0.0),
            (0.0, -20.0, 1.0),
            (36.0, 48.0, 2.0),
            (0.0, 100.0, 3.0),
            (0.0, -120.0, 3.0),
        ] {
            assert_eq!(curve.gain([pitch, yaw, 100.0]), gain);
        }
        assert_eq!(GyroResponse::default().gain([0.0; 3]), 1.0);
        assert!(GyroResponse::new(0.0, 10.0, 10.0, 1.0).is_err());
        assert!(GyroResponse::new(0.0, 10.0, 100.0, f64::NAN).is_err());
    }
}
