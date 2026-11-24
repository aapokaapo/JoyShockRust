use std::time::Duration;

use joyshockrust_core::Stick;
use joyshockrust_input::{Timebase, TimestampUpdate};
use joyshockrust_motion::aim_response::{
    DEFAULT_FLICK_AXIAL_THRESHOLD_DEG, DEFAULT_FLICK_DURATION_MS,
    DEFAULT_FLICK_STEERING_SENSITIVITY, FlickDirection, FlickUpdate, snap_flick_angle,
};

pub struct FlickStick {
    counts_per_degree: f64,
    axial_threshold: f64,
    timebase: Timebase,
    direction: FlickDirection,
    steering_sensitivity: f64,
    flick_duration: Duration,
    remaining_degrees: f64,
    remaining_time: Duration,
    remainder: f64,
}

impl FlickStick {
    pub fn new(counts_per_degree: f64) -> Self {
        Self {
            counts_per_degree,
            axial_threshold: DEFAULT_FLICK_AXIAL_THRESHOLD_DEG.to_radians(),
            timebase: Timebase::new(150_000).expect("valid 50 ms sensor gap"),
            direction: FlickDirection::default(),
            steering_sensitivity: DEFAULT_FLICK_STEERING_SENSITIVITY,
            flick_duration: Duration::from_secs_f64(DEFAULT_FLICK_DURATION_MS / 1000.0),
            remaining_degrees: 0.0,
            remaining_time: Duration::ZERO,
            remainder: 0.0,
        }
    }

    pub fn with_timing(mut self, engage_percent: f64, duration_ms: f64) -> Self {
        self.direction = FlickDirection::new(engage_percent);
        self.flick_duration = Duration::from_secs_f64(duration_ms / 1000.0);
        self
    }

    pub fn with_axial_threshold(mut self, degrees: f64) -> Self {
        self.axial_threshold = degrees.to_radians();
        self
    }

    pub fn set_axial_threshold(&mut self, degrees: f64) {
        self.axial_threshold = degrees.to_radians();
    }

    pub fn with_steering_sensitivity(mut self, multiplier: f64) -> Self {
        self.set_steering_sensitivity(multiplier);
        self
    }

    pub fn set_steering_sensitivity(&mut self, multiplier: f64) {
        if self.steering_sensitivity != multiplier {
            self.steering_sensitivity = multiplier;
            self.remainder = 0.0;
        }
    }

    pub fn set_counts_per_degree(&mut self, scale: f64) {
        if self.counts_per_degree != scale {
            self.counts_per_degree = scale;
            self.remainder = 0.0;
        }
    }

    pub fn is_settled(&self, stick: Stick) -> bool {
        FlickDirection::is_released(stick.x, stick.y) && self.remaining_time.is_zero()
    }

    pub fn advance(&mut self, stick: Stick, ticks: u32) -> i32 {
        self.advance_with_rotation(stick, ticks, |_| {})
    }

    pub fn advance_with_rotation(
        &mut self,
        stick: Stick,
        ticks: u32,
        mut rotation: impl FnMut(Option<f64>),
    ) -> i32 {
        let delta = match self.timebase.observe(ticks) {
            TimestampUpdate::FirstSample => Duration::ZERO,
            TimestampUpdate::Advanced { delta, .. } => delta,
            TimestampUpdate::Gap { .. } => {
                rotation(None);
                self.direction.reset();
                self.remaining_degrees = 0.0;
                self.remaining_time = Duration::ZERO;
                self.remainder = 0.0;
                Duration::ZERO
            }
            TimestampUpdate::Repeated { .. } | TimestampUpdate::OutOfOrder { .. } => return 0,
        };

        let mut degrees = 0.0;
        if !self.remaining_time.is_zero() {
            let elapsed = delta.min(self.remaining_time);
            degrees = self.remaining_degrees
                * (elapsed.as_secs_f64() / self.remaining_time.as_secs_f64());
            self.remaining_degrees -= degrees;
            self.remaining_time -= elapsed;
        }

        match self.direction.update(stick.x, stick.y) {
            FlickUpdate::Idle => rotation(None),
            FlickUpdate::Started(angle) => {
                rotation(None);
                self.remaining_degrees +=
                    snap_flick_angle(angle, self.axial_threshold).to_degrees();
                self.remaining_time = self.flick_duration;
            }
            FlickUpdate::Steered(turn) => {
                let degrees_rotated = turn.to_degrees();
                rotation(Some(degrees_rotated));
                degrees += degrees_rotated * self.steering_sensitivity;
            }
        }

        let total = self.remainder + degrees * self.counts_per_degree;
        let counts = total as i32;
        self.remainder = total - f64::from(counts);
        counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engagement_and_duration_control_when_a_flick_starts_and_finishes() {
        let mut flick = FlickStick::new(2.0).with_timing(70.0, 40.0);
        flick.advance(Stick::default(), 0);
        assert_eq!(flick.advance(Stick { x: 0.69, y: 0.0 }, 30_000), 0);
        assert_eq!(flick.advance(Stick { x: 0.74, y: 0.0 }, 60_000), 0);
        assert_eq!(flick.advance(Stick::default(), 120_000), 90);
        assert_eq!(flick.advance(Stick::default(), 180_000), 90);
        assert_eq!(flick.advance(Stick::default(), 210_000), 0);
    }

    #[test]
    fn right_flick_spreads_a_quarter_turn_and_finishes_after_centering() {
        let mut flick = FlickStick::new(4.0);
        let right = Stick { x: 1.0, y: 0.0 };
        assert_eq!(flick.advance(Stick::default(), 0), 0);
        assert_eq!(flick.advance(right, 30_000), 0);
        assert_eq!(flick.advance(right, 180_000), 180);
        assert_eq!(flick.advance(Stick::default(), 330_000), 180);
        assert_eq!(flick.advance(Stick::default(), 360_000), 0);
    }

    #[test]
    fn threshold_only_changes_initial_flick_and_steering_uses_relative_motion() {
        let mut flick = FlickStick::new(10.0)
            .with_axial_threshold(10.0)
            .with_steering_sensitivity(0.5);
        let at = |degrees: f32| {
            let (x, forward) = degrees.to_radians().sin_cos();
            Stick { x, y: -forward }
        };
        let center = Stick::default();
        assert_eq!(flick.advance(center, 0), 0);
        assert_eq!(flick.advance(at(4.0), 30_000), 0);
        assert_eq!(flick.advance(at(4.0), 60_000), 0); // unchanged input does not undo the snap
        let partial = at(6.0);
        let partial = Stick {
            x: partial.x * 0.6,
            y: partial.y * 0.6,
        };
        assert!((9..=10).contains(&flick.advance(partial, 90_000)));
        assert!((-30..=-29).contains(&flick.advance(at(0.0), 120_000)));
        assert_eq!(flick.advance(center, 150_000), 0);
        assert_eq!(flick.advance(at(90.0), 180_000), 0);
        let flick_counts = flick.advance(at(90.0), 330_000) + flick.advance(at(90.0), 480_000);
        assert!((899..=900).contains(&flick_counts)); // full initial sensitivity
    }

    #[test]
    fn steering_uses_shortest_arc_across_rear_and_gap_clears_pending_motion() {
        let mut flick = FlickStick::new(1.0)
            .with_axial_threshold(0.0)
            .with_steering_sensitivity(1.0);
        let rear_right = Stick { x: 0.1, y: 1.0 };
        let rear_left = Stick { x: -0.1, y: 1.0 };
        assert_eq!(flick.advance(rear_right, 0), 0); // wait for center on startup
        assert_eq!(flick.advance(Stick::default(), 30_000), 0);
        assert_eq!(flick.advance(rear_right, 60_000), 0);
        assert_eq!(flick.advance(rear_left, 60_000), 0); // replayed timestamp
        let movement = flick.advance(rear_left, 63_000);
        assert!((12..=14).contains(&movement), "{movement}");
        assert_eq!(flick.advance(rear_left, 300_000), 0);
        assert_eq!(flick.advance(rear_right, 330_000), 0);
    }
}
