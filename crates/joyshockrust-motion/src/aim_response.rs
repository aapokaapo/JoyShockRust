//! Optional response shaping applied after calibrated motion integration.

pub const DEFAULT_FLICK_AXIAL_THRESHOLD_DEG: f64 = 4.0;
pub const MAX_FLICK_AXIAL_THRESHOLD_DEG: f64 = 30.0;
pub const MAX_GYRO_RATE_DPS: f64 = 2_000.0;
pub const MAX_GYRO_ACCEL_MULTIPLIER: f64 = 10.0;

/// Angle in radians, clockwise from forward in the stick's device coordinates.
pub fn flick_stick_angle(x: f64, y: f64, axial_threshold_radians: f64) -> f64 {
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};

    let angle = x.atan2(-y);
    if axial_threshold_radians <= 0.0 {
        return angle;
    }
    let cardinal = (angle / FRAC_PI_2).round() * FRAC_PI_2;
    let offset = angle - cardinal;
    let distance = offset.abs();
    if distance <= axial_threshold_radians {
        return cardinal;
    }

    let end = (axial_threshold_radians * 3.0).min(FRAC_PI_4);
    if distance >= end {
        return angle;
    }
    // Smoothstep starts with zero slope at the snap boundary and rejoins
    // the original angle with unit slope at the end of the transition.
    let t = (distance - axial_threshold_radians) / (end - axial_threshold_radians);
    cardinal + offset * t * t * (3.0 - 2.0 * t)
}

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
    fn flick_threshold_has_a_continuous_monotonic_transition() {
        let aim = |angle: f64, threshold: f64| {
            let (x, forward) = angle.to_radians().sin_cos();
            flick_stick_angle(x, -forward, threshold.to_radians()).to_degrees()
        };
        for threshold in [4.0, 12.0, 30.0] {
            assert_eq!(aim(threshold - 0.001, threshold), 0.0);
            assert!(aim(threshold + 0.001, threshold).abs() < 0.001);
            let mut previous = 0.0;
            for step in 0..=450 {
                let angle = f64::from(step) / 10.0;
                let output = aim(angle, threshold);
                assert!(output >= previous && output <= angle + 1e-10);
                previous = output;
            }
            assert!((aim(45.0, threshold) - 45.0).abs() < 1e-10);
            assert!((aim(45.001, threshold) - aim(44.999, threshold)).abs() < 0.003);
        }
        assert!((aim(-3.0, 0.0) + 3.0).abs() < 1e-10);
    }

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
