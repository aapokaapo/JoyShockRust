use core::{ops::Div, str::FromStr};

use crate::Controls;

/// Pull and release points in percent, in partial then full order.
pub const DEFAULT_TRIGGER_THRESHOLDS: [u8; 4] = [45, 35, 95, 85];

pub fn validate_trigger_thresholds(
    [partial, partial_release, full, full_release]: [u8; 4],
) -> Result<(), &'static str> {
    if partial_release < partial && partial < full_release && full_release < full && full <= 100 {
        Ok(())
    } else {
        Err(
            "Trigger thresholds must satisfy partial release < partial pull < full release < full pull ≤ 100%.",
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TriggerGate {
    side: TriggerSide,
    press: f32,
    release: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerSide {
    Left,
    Right,
}

impl TriggerGate {
    pub fn from_percent(side: TriggerSide, press: u8, release: u8) -> Self {
        Self {
            side,
            press: f32::from(press) / 100.0,
            release: f32::from(release) / 100.0,
        }
    }

    pub fn within_travel(mut self, percent: u8) -> Self {
        let scale = f32::from(percent) / 100.0;
        self.press *= scale;
        self.release *= scale;
        self
    }

    pub fn parse(spec: &str) -> Result<Self, &'static str> {
        let (side, press, release) = parse_analog_trigger(spec)?;
        Ok(Self {
            side,
            press,
            release,
        })
    }

    pub fn partial(spec: &str) -> Result<Option<(Self, Self)>, &'static str> {
        let mut fields = spec.split(':');
        let (Some(side), Some(press), Some(release), Some(full_press), Some(full_release), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return Ok(None);
        };
        let side = TriggerSide::parse(side)?;
        let (press, release) = parse_pair(press, release)?;
        let lower = Self {
            side,
            press,
            release,
        };
        let (press, release) = parse_pair(full_press, full_release)?;
        let upper = Self {
            side,
            press,
            release,
        };
        if lower.press >= upper.release {
            return Err("partial trigger thresholds must satisfy partial press < full release");
        }
        Ok(Some((lower, upper)))
    }

    pub fn side(self) -> TriggerSide {
        self.side
    }

    #[inline]
    pub fn pressed(self, controls: &Controls, was_pressed: bool) -> bool {
        let travel = match self.side {
            TriggerSide::Left => controls.left_trigger,
            TriggerSide::Right => controls.right_trigger,
        };
        if was_pressed {
            travel > self.release
        } else {
            travel >= self.press
        }
    }
}

impl TriggerSide {
    pub fn parse(side: &str) -> Result<Self, &'static str> {
        match side {
            "l2" => Ok(Self::Left),
            "r2" => Ok(Self::Right),
            _ => Err("expected analog trigger l2 or r2"),
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Left => "l2",
            Self::Right => "r2",
        }
    }
}

/// Parse the bare or explicit single-stage form in the caller's numeric precision.
pub fn parse_analog_trigger<T>(spec: &str) -> Result<(TriggerSide, T, T), &'static str>
where
    T: FromStr + From<u8> + PartialOrd + Div<Output = T>,
{
    let mut fields = spec.split(':');
    let side = TriggerSide::parse(fields.next().unwrap_or_default())?;
    let (press, release) = match (fields.next(), fields.next(), fields.next()) {
        (None, None, None) => (
            T::from(DEFAULT_TRIGGER_THRESHOLDS[0]) / T::from(100),
            T::from(DEFAULT_TRIGGER_THRESHOLDS[1]) / T::from(100),
        ),
        (Some(press), Some(release), None) => parse_pair(press, release)?,
        _ => return Err("expected l2 or r2 with optional :PRESS:RELEASE thresholds"),
    };
    Ok((side, press, release))
}

fn parse_pair<T: FromStr + From<u8> + PartialOrd>(
    press: &str,
    release: &str,
) -> Result<(T, T), &'static str> {
    let press = press
        .parse()
        .map_err(|_| "invalid trigger press threshold")?;
    let release = release
        .parse()
        .map_err(|_| "invalid trigger release threshold")?;
    if T::from(0) <= release && release < press && press <= T::from(1) {
        Ok((press, release))
    } else {
        Err("trigger thresholds must satisfy 0 <= release < press <= 1")
    }
}

pub fn validate_resistance(start: u8, strength: u8) -> Result<(), &'static str> {
    if start > 9 || !(1..=8).contains(&strength) {
        Err("adaptive trigger resistance requires start 0–9 and strength 1–8")
    } else {
        Ok(())
    }
}
