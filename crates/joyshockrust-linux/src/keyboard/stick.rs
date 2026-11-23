use std::time::Instant;

use evdev::{AttributeSet, KeyCode};
use joyshockrust_core::{Controls, Stick};
use serde::Deserialize;

use super::{TAP_KEY_TIME, parse_key};

const DIAGONAL_RATIO: f32 = 0.414_213_56; // tan(22.5°)
const DIRECTION_BOUNDARY_DEG: f32 = 22.5;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Side {
    Left,
    Right,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Settings {
    side: Side,
    deadzone: Option<f32>,
    deadzone_release: Option<f32>,
    direction_hysteresis_deg: Option<f32>,
    up: Option<String>,
    right: Option<String>,
    down: Option<String>,
    left: Option<String>,
    outer: Option<OuterSettings>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OuterSettings {
    key: String,
    threshold: f32,
    release: Option<f32>,
    #[serde(default)]
    mode: OuterMode,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum OuterMode {
    #[default]
    Hold,
    Toggle,
    KeepSprint,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct Binding {
    side: Side,
    deadzone_squared: f32,
    deadzone_release_squared: f32,
    direction_enter_ratio: f32,
    direction_release_ratio: f32,
    directions: [Option<KeyCode>; 4],
    outer: Option<Outer>,
}

#[derive(Clone, Copy, PartialEq)]
struct Outer {
    key: KeyCode,
    mode: OuterMode,
    threshold_squared: f32,
    release_squared: f32,
}

impl Settings {
    pub(super) fn resolve_all(settings: Vec<Self>) -> Result<Vec<Binding>, String> {
        let mut bindings = Vec::<Binding>::new();
        for settings in settings {
            if bindings.iter().any(|binding| binding.side == settings.side) {
                return Err(format!(
                    "duplicate keyboard stick binding for {:?}",
                    settings.side
                ));
            }
            bindings.push(settings.resolve()?);
        }
        Ok(bindings)
    }

    fn resolve(self) -> Result<Binding, String> {
        let deadzone = self.deadzone.unwrap_or(0.2);
        if !(0.0..1.0).contains(&deadzone) {
            return Err("keyboard stick deadzone must be in [0, 1)".into());
        }
        let deadzone_release = self.deadzone_release.unwrap_or(deadzone);
        if !(0.0..=deadzone).contains(&deadzone_release) {
            return Err("keyboard stick deadzone_release must be between 0 and deadzone".into());
        }
        let margin = self.direction_hysteresis_deg.unwrap_or(0.0);
        if !(0.0..DIRECTION_BOUNDARY_DEG).contains(&margin) {
            return Err("keyboard stick direction_hysteresis_deg must be in [0, 22.5)".into());
        }
        let (direction_enter_ratio, direction_release_ratio) = if margin == 0.0 {
            (DIAGONAL_RATIO, DIAGONAL_RATIO)
        } else {
            (
                (DIRECTION_BOUNDARY_DEG + margin).to_radians().tan(),
                (DIRECTION_BOUNDARY_DEG - margin).to_radians().tan(),
            )
        };
        let directions = [
            parse_key(self.up)?,
            parse_key(self.right)?,
            parse_key(self.down)?,
            parse_key(self.left)?,
        ];
        let outer = self
            .outer
            .map(|settings| {
                let threshold = settings.threshold;
                let release = settings
                    .release
                    .unwrap_or_else(|| (threshold - 0.1).max(deadzone));
                if !(deadzone..threshold).contains(&release) || threshold > 1.0 {
                    return Err(
                        "keyboard stick outer zone requires deadzone <= release < threshold <= 1"
                            .to_owned(),
                    );
                }
                Ok(Outer {
                    key: super::keyboard_key(&settings.key)?,
                    mode: settings.mode,
                    threshold_squared: threshold * threshold,
                    release_squared: release * release,
                })
            })
            .transpose()?;
        if directions.iter().all(Option::is_none) && outer.is_none() {
            return Err("keyboard stick binding needs at least one key".into());
        }
        if outer.is_some_and(|outer| outer.mode == OuterMode::KeepSprint)
            && directions.iter().all(Option::is_none)
        {
            return Err("keyboard stick keep_sprint needs a direction key".into());
        }
        Ok(Binding {
            side: self.side,
            deadzone_squared: deadzone * deadzone,
            deadzone_release_squared: deadzone_release * deadzone_release,
            direction_enter_ratio,
            direction_release_ratio,
            directions,
            outer,
        })
    }
}

impl Binding {
    pub(super) fn keys(self) -> impl Iterator<Item = KeyCode> {
        self.directions
            .into_iter()
            .flatten()
            .chain(self.outer.map(|outer| outer.key))
    }

    pub(super) fn held_outer_key(self) -> Option<KeyCode> {
        self.outer
            .filter(|outer| outer.mode == OuterMode::Hold)
            .map(|outer| outer.key)
    }

    pub(super) fn pulsed_outer_key(self) -> Option<KeyCode> {
        self.outer
            .filter(|outer| outer.mode != OuterMode::Hold)
            .map(|outer| outer.key)
    }
}

pub(super) struct State {
    binding: Binding,
    directions_active: bool,
    active_directions: [bool; 4],
    outer_active: bool,
    sent_active: bool,
    pulse: PulsePhase,
}

#[derive(Default)]
enum PulsePhase {
    #[default]
    Ready,
    Down(Instant),
    Up(Instant),
}

impl State {
    pub(super) fn new(binding: Binding) -> Self {
        Self {
            binding,
            directions_active: false,
            active_directions: [false; 4],
            outer_active: false,
            sent_active: false,
            pulse: PulsePhase::Ready,
        }
    }

    pub(super) fn update(
        &mut self,
        controls: Controls,
        now: Instant,
        held: &mut AttributeSet<KeyCode>,
    ) -> Option<Instant> {
        let Stick { x, y } = match self.binding.side {
            Side::Left => controls.left_stick,
            Side::Right => controls.right_stick,
        };
        let radius_squared = x * x + y * y;
        let direction_threshold = if self.directions_active {
            self.binding.deadzone_release_squared
        } else {
            self.binding.deadzone_squared
        };
        self.directions_active = radius_squared > direction_threshold;
        let mut moving = false;
        if self.directions_active {
            let directions = [(-y, x.abs()), (x, y.abs()), (y, x.abs()), (-x, y.abs())];
            for (((component, perpendicular), active), key) in directions
                .into_iter()
                .zip(&mut self.active_directions)
                .zip(self.binding.directions)
            {
                let ratio = if *active {
                    self.binding.direction_release_ratio
                } else {
                    self.binding.direction_enter_ratio
                };
                *active = component > 0.0 && component >= perpendicular * ratio;
                if *active && let Some(key) = key {
                    held.insert(key);
                    moving = true;
                }
            }
        } else {
            self.active_directions = [false; 4];
        }

        let outer = self.binding.outer?;
        self.outer_active = if self.outer_active {
            radius_squared > outer.release_squared
        } else {
            radius_squared >= outer.threshold_squared
        };
        let (pressed, deadline) = match outer.mode {
            OuterMode::Hold => (self.outer_active, None),
            OuterMode::Toggle => self.update_pulse(self.outer_active, now),
            OuterMode::KeepSprint => {
                if !moving {
                    // Movement release ends sprint in the game and re-arms the next press.
                    self.sent_active = false;
                }
                self.update_pulse(moving && (self.outer_active || self.sent_active), now)
            }
        };
        if pressed {
            held.insert(outer.key);
        }
        deadline
    }

    fn update_pulse(&mut self, active: bool, now: Instant) -> (bool, Option<Instant>) {
        match self.pulse {
            PulsePhase::Down(until) if now < until => return (true, Some(until)),
            PulsePhase::Down(_) => self.pulse = PulsePhase::Up(now + TAP_KEY_TIME),
            PulsePhase::Up(until) if now >= until => self.pulse = PulsePhase::Ready,
            _ => {}
        }

        if active == self.sent_active {
            return (false, None);
        }
        if let PulsePhase::Up(until) = self.pulse {
            // Keep a visible release between successive presses.
            return (false, Some(until));
        }
        let until = now + TAP_KEY_TIME;
        self.pulse = PulsePhase::Down(until);
        self.sent_active = active;
        (true, Some(until))
    }
}
