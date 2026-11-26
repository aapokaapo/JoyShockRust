//! Radial stick response and unchanged square-corner mapping.

use joyshockrust_core::{Controls, Stick};

pub const DEFAULT_STICK_EXPONENT: f32 = 1.0;
pub const STICK_EXPONENT_RANGE: core::ops::RangeInclusive<f32> = 0.25..=4.0;

pub fn plot_travel(exponent: f64, percent: f64) -> f64 {
    let exponent = exponent.clamp(
        (*STICK_EXPONENT_RANGE.start()).into(),
        (*STICK_EXPONENT_RANGE.end()).into(),
    );
    100.0 * (percent / 100.0).powf(exponent)
}

#[derive(Clone, Copy)]
pub struct StickCurve([f32; 2]);

impl StickCurve {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        Self::new(value.parse().map_err(|_| "invalid stick curve exponent")?)
    }

    pub fn new(exponent: f32) -> Result<Self, &'static str> {
        Self::validate(exponent)?;
        Ok(Self([exponent; 2]))
    }

    fn validate(exponent: f32) -> Result<(), &'static str> {
        if !(STICK_EXPONENT_RANGE).contains(&exponent) {
            return Err("stick curve exponent must be between 0.25 and 4");
        }
        Ok(())
    }

    pub fn from_profile(
        shared: Option<f32>,
        left: Option<f32>,
        right: Option<f32>,
    ) -> Result<Option<Self>, &'static str> {
        [shared, left, right]
            .into_iter()
            .flatten()
            .try_for_each(Self::validate)?;
        let fallback = shared.unwrap_or(DEFAULT_STICK_EXPONENT);
        Ok((shared.is_some() || left.is_some() || right.is_some())
            .then_some(Self([left.unwrap_or(fallback), right.unwrap_or(fallback)])))
    }

    #[inline]
    pub fn apply(self, mut controls: Controls) -> Controls {
        controls.left_stick = Self::shape(controls.left_stick, self.0[0]);
        controls.right_stick = Self::shape(controls.right_stick, self.0[1]);
        controls
    }

    #[inline]
    fn shape(stick: Stick, exponent: f32) -> Stick {
        let radius_squared = stick.x * stick.x + stick.y * stick.y;
        // Keep center and full travel unchanged, including square corners.
        if exponent == DEFAULT_STICK_EXPONENT || radius_squared == 0.0 || radius_squared >= 1.0 {
            return stick;
        }
        let scale = radius_squared.powf((exponent - 1.0) * 0.5);
        Stick {
            x: stick.x * scale,
            y: stick.y * scale,
        }
    }
}
