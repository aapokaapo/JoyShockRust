use std::{
    f64::consts::{PI, TAU},
    time::Duration,
};

use joyshockrust_core::Stick;
use joyshockrust_input::{Timebase, TimestampUpdate};

const FLICK_DURATION: Duration = Duration::from_millis(100);
const ENGAGE_RADIUS: f32 = 0.90;
const RELEASE_RADIUS: f32 = 0.75;

pub struct FlickStick {
    counts_per_degree: f64,
    timebase: Timebase,
    armed: bool,
    angle: Option<f64>,
    remaining_degrees: f64,
    remaining_time: Duration,
    remainder: f64,
}

impl FlickStick {
    pub fn new(counts_per_degree: f64) -> Self {
        Self {
            counts_per_degree,
            timebase: Timebase::new(150_000).expect("valid 50 ms sensor gap"),
            armed: false,
            angle: None,
            remaining_degrees: 0.0,
            remaining_time: Duration::ZERO,
            remainder: 0.0,
        }
    }

    pub fn set_counts_per_degree(&mut self, scale: f64) {
        if self.counts_per_degree != scale {
            self.counts_per_degree = scale;
            self.remainder = 0.0;
        }
    }

    pub fn is_settled(&self, stick: Stick) -> bool {
        stick.x * stick.x + stick.y * stick.y <= RELEASE_RADIUS * RELEASE_RADIUS
            && self.remaining_time.is_zero()
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
                self.armed = false;
                self.angle = None;
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

        let radius_squared = stick.x * stick.x + stick.y * stick.y;
        if radius_squared <= RELEASE_RADIUS * RELEASE_RADIUS {
            rotation(None);
            self.armed = true;
            self.angle = None;
        } else if self.armed
            && (self.angle.is_some() || radius_squared >= ENGAGE_RADIUS * ENGAGE_RADIUS)
        {
            let current = f64::from(stick.x).atan2(-f64::from(stick.y));
            if let Some(previous) = self.angle.replace(current) {
                let turn = (current - previous + PI).rem_euclid(TAU) - PI;
                let degrees_rotated = turn.to_degrees();
                rotation(Some(degrees_rotated));
                degrees += degrees_rotated;
            } else {
                rotation(None);
                self.remaining_degrees += current.to_degrees();
                self.remaining_time = FLICK_DURATION;
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
    fn steering_uses_shortest_arc_across_rear_and_gap_clears_pending_motion() {
        let mut flick = FlickStick::new(1.0);
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
