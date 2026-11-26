use std::time::Instant;

use joyshockrust_core::{Controls, Stick};

use super::{Key, KeyResolver, KeySet, TAP_KEY_TIME};

const DIAGONAL_RATIO: f32 = 0.414_213_56; // tan(22.5°)
const DIRECTION_BOUNDARY_DEG: f32 = 22.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "profile", derive(serde::Deserialize))]
#[cfg_attr(feature = "profile", serde(rename_all = "lowercase"))]
enum Side {
    Left,
    Right,
}

#[cfg_attr(feature = "profile", derive(serde::Deserialize))]
#[cfg_attr(feature = "profile", serde(deny_unknown_fields))]
pub(super) struct Settings {
    side: Side,
    deadzone: Option<f32>,
    deadzone_release: Option<f32>,
    direction_hysteresis_deg: Option<f32>,
    up: Option<String>,
    right: Option<String>,
    down: Option<String>,
    left: Option<String>,
    outer: Option<ZoneSettings>,
    inner: Option<ZoneSettings>,
}

#[cfg_attr(feature = "profile", derive(serde::Deserialize))]
#[cfg_attr(feature = "profile", serde(deny_unknown_fields))]
struct ZoneSettings {
    key: String,
    threshold: f32,
    release: Option<f32>,
    #[cfg_attr(feature = "profile", serde(default))]
    mode: ZoneMode,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "profile", derive(serde::Deserialize))]
#[cfg_attr(feature = "profile", serde(rename_all = "snake_case"))]
enum ZoneMode {
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
    directions: [Option<Key>; 4],
    zones: [Option<Zone>; 2],
}

#[derive(Clone, Copy, PartialEq)]
struct Zone {
    kind: ZoneKind,
    key: Key,
    mode: ZoneMode,
    threshold_squared: f32,
    release_squared: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum ZoneKind {
    Outer,
    Inner,
}

impl ZoneSettings {
    fn resolve(
        self,
        kind: ZoneKind,
        deadzone: f32,
        keys: &impl KeyResolver,
    ) -> Result<Zone, String> {
        let threshold = self.threshold;
        let release = match kind {
            ZoneKind::Outer => {
                let release = self
                    .release
                    .unwrap_or_else(|| (threshold - 0.1).max(deadzone));
                if !(deadzone..threshold).contains(&release) || threshold > 1.0 {
                    return Err(
                        "keyboard stick outer zone requires deadzone <= release < threshold <= 1"
                            .into(),
                    );
                }
                release
            }
            ZoneKind::Inner => {
                let release = self.release.unwrap_or((threshold + 0.1).min(1.0));
                if !(deadzone < threshold && threshold < release && release <= 1.0) {
                    return Err(
                        "keyboard stick inner zone requires deadzone < threshold < release <= 1"
                            .into(),
                    );
                }
                release
            }
        };
        Ok(Zone {
            kind,
            key: keys.keyboard(&self.key)?,
            mode: self.mode,
            threshold_squared: threshold * threshold,
            release_squared: release * release,
        })
    }
}

impl Settings {
    pub(super) fn resolve_all(
        settings: Vec<Self>,
        keys: &impl KeyResolver,
    ) -> Result<Vec<Binding>, String> {
        let mut bindings = Vec::<Binding>::new();
        for settings in settings {
            if bindings.iter().any(|binding| binding.side == settings.side) {
                return Err(format!(
                    "duplicate keyboard stick binding for {:?}",
                    settings.side
                ));
            }
            bindings.push(settings.resolve(keys)?);
        }
        Ok(bindings)
    }

    fn resolve(self, keys: &impl KeyResolver) -> Result<Binding, String> {
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
        let parse_key =
            |name: Option<String>| name.as_deref().map(|name| keys.keyboard(name)).transpose();
        let directions = [
            parse_key(self.up)?,
            parse_key(self.right)?,
            parse_key(self.down)?,
            parse_key(self.left)?,
        ];
        let zones = [
            self.outer
                .map(|settings| settings.resolve(ZoneKind::Outer, deadzone, keys))
                .transpose()?,
            self.inner
                .map(|settings| settings.resolve(ZoneKind::Inner, deadzone, keys))
                .transpose()?,
        ];
        if directions.iter().all(Option::is_none) && zones.iter().all(Option::is_none) {
            return Err("keyboard stick binding needs at least one key".into());
        }
        if zones
            .iter()
            .flatten()
            .any(|zone| zone.mode == ZoneMode::KeepSprint)
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
            zones,
        })
    }
}

impl Binding {
    pub(super) fn keys(self) -> impl Iterator<Item = Key> {
        self.directions
            .into_iter()
            .flatten()
            .chain(self.zones.into_iter().flatten().map(|zone| zone.key))
    }

    pub(super) fn held_zone_keys(self) -> impl Iterator<Item = Key> {
        self.zones
            .into_iter()
            .flatten()
            .filter(|zone| zone.mode == ZoneMode::Hold)
            .map(|zone| zone.key)
    }

    pub(super) fn pulsed_zone_keys(self) -> impl Iterator<Item = Key> {
        self.zones
            .into_iter()
            .flatten()
            .filter(|zone| zone.mode != ZoneMode::Hold)
            .map(|zone| zone.key)
    }
}

pub(super) struct State {
    binding: Binding,
    directions_active: bool,
    active_directions: [bool; 4],
    zones: [ZoneState; 2],
}

#[derive(Default)]
struct ZoneState {
    active: bool,
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
            zones: std::array::from_fn(|_| ZoneState::default()),
        }
    }

    pub(super) fn update(
        &mut self,
        controls: Controls,
        now: Instant,
        held: &mut KeySet,
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
                    held.insert(key.id);
                    moving = true;
                }
            }
        } else {
            self.active_directions = [false; 4];
        }

        let mut deadline = None;
        for (state, zone) in self.zones.iter_mut().zip(self.binding.zones) {
            let Some(zone) = zone else { continue };
            let active = match (zone.kind, state.active) {
                (ZoneKind::Outer, false) => radius_squared >= zone.threshold_squared,
                (ZoneKind::Outer, true) => radius_squared > zone.release_squared,
                (ZoneKind::Inner, false) => {
                    self.directions_active && radius_squared <= zone.threshold_squared
                }
                (ZoneKind::Inner, true) => {
                    self.directions_active && radius_squared < zone.release_squared
                }
            };
            let (pressed, next_deadline) = state.update(zone.mode, active, moving, now);
            if pressed {
                held.insert(zone.key.id);
            }
            deadline = deadline.into_iter().chain(next_deadline).min();
        }
        deadline
    }
}

impl ZoneState {
    fn update(
        &mut self,
        mode: ZoneMode,
        active: bool,
        moving: bool,
        now: Instant,
    ) -> (bool, Option<Instant>) {
        self.active = active;
        match mode {
            ZoneMode::Hold => (active, None),
            ZoneMode::Toggle => self.update_pulse(active, now),
            ZoneMode::KeepSprint => {
                if !moving {
                    // Releasing movement re-arms the next press.
                    self.sent_active = false;
                }
                self.update_pulse(moving && (active || self.sent_active), now)
            }
        }
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
