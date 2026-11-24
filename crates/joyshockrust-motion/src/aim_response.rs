//! Optional response shaping applied after calibrated motion integration.

use std::{
    f64::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU},
    time::Duration,
};

pub const DEFAULT_FLICK_AXIAL_THRESHOLD_DEG: f64 = 4.0;
pub const MAX_FLICK_AXIAL_THRESHOLD_DEG: f64 = 30.0;
pub const DEFAULT_FLICK_STEERING_SENSITIVITY: f64 = 0.5;
pub const MAX_GYRO_RATE_DPS: f64 = 2_000.0;
pub const MAX_GYRO_ACCEL_MULTIPLIER: f64 = 10.0;

pub const DEFAULT_FLICK_ENGAGE_PERCENT: f64 = 90.0;
pub const MIN_FLICK_ENGAGE_PERCENT: f64 = 30.0;
pub const DEFAULT_FLICK_DURATION_MS: f64 = 100.0;
pub const MIN_FLICK_DURATION_MS: f64 = 10.0;
pub const MAX_FLICK_DURATION_MS: f64 = 500.0;
pub const FLICK_RELEASE_RADIUS: f32 = 0.25;

pub struct FlickDirection {
    armed: bool,
    previous: Option<f64>,
    engage_radius_squared: f32,
}

impl Default for FlickDirection {
    fn default() -> Self {
        Self::new(DEFAULT_FLICK_ENGAGE_PERCENT)
    }
}

pub enum FlickUpdate {
    Idle,
    Started(f64),
    Steered(f64),
}

impl FlickDirection {
    pub fn new(engage_percent: f64) -> Self {
        let radius = (engage_percent / 100.0) as f32;
        Self {
            armed: false,
            previous: None,
            engage_radius_squared: radius * radius,
        }
    }

    pub fn reset(&mut self) {
        self.armed = false;
        self.previous = None;
    }

    pub fn is_released(x: f32, y: f32) -> bool {
        x * x + y * y <= FLICK_RELEASE_RADIUS * FLICK_RELEASE_RADIUS
    }

    /// Reports angles in radians, clockwise from forward.
    pub fn update(&mut self, x: f32, y: f32) -> FlickUpdate {
        if Self::is_released(x, y) {
            self.armed = true;
            self.previous = None;
        } else if self.armed
            && (self.previous.is_some() || x * x + y * y >= self.engage_radius_squared)
        {
            let current = f64::from(x).atan2(-f64::from(y));
            return match self.previous.replace(current) {
                Some(previous) => {
                    FlickUpdate::Steered((current - previous + PI).rem_euclid(TAU) - PI)
                }
                None => FlickUpdate::Started(current),
            };
        }
        FlickUpdate::Idle
    }
}

/// Applies the axial threshold to the initial angle, in radians from forward.
pub fn snap_flick_angle(angle: f64, axial_threshold_radians: f64) -> f64 {
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
    acceleration_enabled: bool,
}

impl Default for GyroResponse {
    fn default() -> Self {
        Self {
            min_speed: 0.2,
            accel_start: 30.0,
            accel_end: 180.0,
            multiplier: 1.0,
            acceleration_enabled: false,
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
            acceleration_enabled: multiplier > 1.0,
        })
    }

    pub fn with_acceleration_enabled(mut self, enabled: bool) -> Self {
        self.acceleration_enabled = enabled;
        self
    }

    pub fn values(self) -> [f64; 4] {
        [
            self.min_speed,
            self.accel_start,
            self.accel_end,
            self.multiplier,
        ]
    }

    /// Gain for the interval's integrated aim angle in degrees.
    pub fn interval_gain(self, aim_delta: [f32; 2], duration: Duration) -> f64 {
        if duration.is_zero() {
            return 0.0;
        }
        if self.min_speed == 0.0 && !self.acceleration_enabled {
            return 1.0;
        }
        let [yaw, pitch] = aim_delta.map(f64::from);
        let speed = pitch.hypot(yaw) / duration.as_secs_f64();
        self.gain_at_speed(speed)
    }

    /// Response gain at the magnitude of a projected aim speed in degrees per second.
    pub fn gain_at_speed(self, speed: f64) -> f64 {
        if speed < self.min_speed {
            return 0.0;
        }
        if !self.acceleration_enabled || self.multiplier == 1.0 {
            return 1.0;
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
            snap_flick_angle(x.atan2(forward), threshold.to_radians()).to_degrees()
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
        let second = Duration::from_secs(1);
        for (pitch, yaw, gain) in [
            (0.5, 1.0, 0.0),
            (0.0, -20.0, 1.0),
            (36.0, 48.0, 2.0),
            (0.0, 100.0, 3.0),
            (0.0, -120.0, 3.0),
        ] {
            assert_eq!(curve.interval_gain([yaw, pitch], second), gain);
        }
        let disabled = curve.with_acceleration_enabled(false);
        assert_eq!(disabled.interval_gain([0.5, 1.0], second), 0.0);
        assert_eq!(disabled.interval_gain([0.0, 120.0], second), 1.0);
        assert_eq!(disabled.values(), curve.values());
        assert_eq!(disabled.with_acceleration_enabled(true), curve);
        assert_eq!(GyroResponse::default().interval_gain([0.0; 2], second), 0.0);
        assert_eq!(
            GyroResponse::default().interval_gain([0.0, 0.25], second),
            1.0
        );
        assert_eq!(
            GyroResponse::new(0.0, 30.0, 180.0, 1.0)
                .unwrap()
                .interval_gain([0.0, 0.1], second),
            1.0
        );
        assert!(GyroResponse::new(0.0, 10.0, 10.0, 1.0).is_err());
        assert!(GyroResponse::new(0.0, 10.0, 100.0, f64::NAN).is_err());
    }
}
