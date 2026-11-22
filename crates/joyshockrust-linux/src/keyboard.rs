use std::{
    io,
    time::{Duration, Instant},
};

use evdev::{AttributeSet, EventType, InputEvent, KeyCode, uinput::VirtualDevice};
use joyshockrust_core::{Button, Controls, DPad};
use serde::Deserialize;

use crate::gyro_gate::{self, DOUBLE_TAP_WINDOW, HOLD_TIME, TriggerGate};

mod stick;

const TAP_KEY_TIME: Duration = Duration::from_millis(30);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    bindings: Vec<RawBinding>,
    #[serde(default)]
    sticks: Vec<stick::Settings>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBinding {
    button: Option<String>,
    trigger: Option<String>,
    tap: Option<String>,
    hold: Option<String>,
    double_tap: Option<String>,
}

#[derive(PartialEq)]
pub struct Config {
    buttons: Vec<Binding>,
    sticks: Vec<stick::Binding>,
}

impl Config {
    fn keys(&self) -> impl Iterator<Item = KeyCode> {
        self.buttons
            .iter()
            .flat_map(|binding| {
                [binding.tap, binding.hold, binding.double_tap]
                    .into_iter()
                    .flatten()
            })
            .chain(self.sticks.iter().flat_map(|binding| binding.keys()))
    }
}

fn parse_key(name: Option<String>) -> Result<Option<KeyCode>, String> {
    name.as_deref().map(keyboard_key).transpose()
}

fn keyboard_key(name: &str) -> Result<KeyCode, String> {
    if !name.starts_with("KEY_") || name == "KEY_RESERVED" {
        return Err(format!("expected a Linux keyboard key, got {name}"));
    }
    name.parse()
        .map_err(|_| format!("unknown keyboard key {name}"))
}

#[derive(Clone, Copy, PartialEq)]
enum Source {
    Button(Button),
    Direction(DPad),
    Trigger(TriggerGate),
}

#[derive(Clone, Copy, PartialEq)]
struct Binding {
    source: Source,
    tap: Option<KeyCode>,
    hold: Option<KeyCode>,
    double_tap: Option<KeyCode>,
}

impl Source {
    fn parse(name: &str) -> Option<Self> {
        let direction = match name {
            "dpad-up" => DPad::Up,
            "dpad-right" => DPad::Right,
            "dpad-down" => DPad::Down,
            "dpad-left" => DPad::Left,
            _ => return gyro_gate::button_by_name(name).map(Self::Button),
        };
        Some(Self::Direction(direction))
    }

    fn pressed(self, controls: Controls, was_pressed: bool) -> bool {
        match self {
            Self::Button(button) => controls.buttons.contains(button),
            Self::Trigger(trigger) => trigger.pressed(&controls, was_pressed),
            Self::Direction(direction) => {
                controls.dpad == direction
                    || matches!(
                        (direction, controls.dpad),
                        (DPad::Up, DPad::UpLeft | DPad::UpRight)
                            | (DPad::Right, DPad::UpRight | DPad::DownRight)
                            | (DPad::Down, DPad::DownLeft | DPad::DownRight)
                            | (DPad::Left, DPad::UpLeft | DPad::DownLeft)
                    )
            }
        }
    }
}

impl Settings {
    pub fn resolve(self) -> Result<Config, String> {
        let mut bindings = Vec::<Binding>::new();
        for raw in self.bindings {
            let (source, name) = match (raw.button.as_deref(), raw.trigger.as_deref()) {
                (Some(name), None) => (
                    Source::parse(name)
                        .ok_or_else(|| format!("unknown keyboard binding button {name}"))?,
                    name,
                ),
                (None, Some(spec)) => (
                    Source::Trigger(TriggerGate::parse(spec).map_err(|error| {
                        format!("invalid keyboard binding trigger {spec}: {error}")
                    })?),
                    spec,
                ),
                _ => {
                    return Err("keyboard binding requires exactly one of button or trigger".into());
                }
            };
            if bindings.iter().any(|binding| binding.source == source) {
                return Err(format!("duplicate keyboard binding for {name}"));
            }
            let binding = Binding {
                source,
                tap: parse_key(raw.tap)?,
                hold: parse_key(raw.hold)?,
                double_tap: parse_key(raw.double_tap)?,
            };
            if binding.tap.is_none() && binding.hold.is_none() && binding.double_tap.is_none() {
                return Err(format!(
                    "keyboard binding for {name} needs at least one key"
                ));
            }
            bindings.push(binding);
        }
        let config = Config {
            buttons: bindings,
            sticks: stick::Settings::resolve_all(self.sticks)?,
        };
        for key in config
            .sticks
            .iter()
            .filter_map(|stick| stick.pulsed_outer_key())
        {
            if config
                .keys()
                .filter(|&mapped| mapped == key)
                .nth(1)
                .is_some()
            {
                return Err(format!(
                    "keyboard stick pulse key {key:?} must have a single binding"
                ));
            }
        }
        Ok(config)
    }
}

#[derive(Default)]
enum Press {
    #[default]
    Up,
    Down(Instant),
    Held,
}

struct Gesture {
    binding: Binding,
    press: Press,
    pending_tap: Option<Instant>,
}

impl Gesture {
    fn update(&mut self, controls: Controls, now: Instant) -> Option<KeyCode> {
        let pressed = self
            .binding
            .source
            .pressed(controls, !matches!(self.press, Press::Up));
        let mut pulse = None;
        if self.pending_tap.is_some_and(|deadline| deadline <= now) {
            self.pending_tap = None;
            pulse = self.binding.tap;
        }
        match self.press {
            Press::Up
                if pressed && self.binding.tap.is_none() && self.binding.double_tap.is_none() =>
            {
                self.press = Press::Held;
            }
            Press::Up if pressed => self.press = Press::Down(now),
            Press::Down(start) if !pressed => {
                self.press = Press::Up;
                if now.duration_since(start) < HOLD_TIME {
                    if let Some(double_tap) = self.binding.double_tap {
                        if self.pending_tap.take().is_some() {
                            pulse = Some(double_tap);
                        } else {
                            self.pending_tap = Some(now + DOUBLE_TAP_WINDOW);
                        }
                    } else {
                        pulse = self.binding.tap;
                    }
                }
            }
            Press::Down(start) if self.binding.hold.is_some() && now >= start + HOLD_TIME => {
                self.press = Press::Held;
            }
            Press::Held if !pressed => self.press = Press::Up,
            _ => {}
        }
        pulse
    }

    fn deadline(&self) -> Option<Instant> {
        let hold = match self.press {
            Press::Down(start) if self.binding.hold.is_some() => Some(start + HOLD_TIME),
            _ => None,
        };
        hold.into_iter().chain(self.pending_tap).min()
    }
}

struct State {
    gestures: Vec<Gesture>,
    sticks: Vec<stick::State>,
    pulses: Vec<(KeyCode, Instant)>,
    mapped_keys: AttributeSet<KeyCode>,
    outer_keys: AttributeSet<KeyCode>,
    pulsed_outer_keys: AttributeSet<KeyCode>,
    held: AttributeSet<KeyCode>,
    next_held: AttributeSet<KeyCode>,
    events: Vec<InputEvent>,
    controls: Controls,
}

impl State {
    fn new(config: &Config) -> Self {
        let gestures = config
            .buttons
            .iter()
            .map(|&binding| Gesture {
                binding,
                press: Press::Up,
                pending_tap: None,
            })
            .collect();
        let mapped_keys: AttributeSet<KeyCode> = config.keys().collect();
        let events = Vec::with_capacity(mapped_keys.iter().count());
        Self {
            gestures,
            sticks: config
                .sticks
                .iter()
                .copied()
                .map(stick::State::new)
                .collect(),
            // Each binding has at most two distinct pulse targets.
            pulses: Vec::with_capacity(config.buttons.len() * 2),
            mapped_keys,
            outer_keys: config
                .sticks
                .iter()
                .filter_map(|binding| binding.held_outer_key())
                .collect(),
            pulsed_outer_keys: config
                .sticks
                .iter()
                .filter_map(|binding| binding.pulsed_outer_key())
                .collect(),
            held: AttributeSet::new(),
            next_held: AttributeSet::new(),
            events,
            controls: Controls::default(),
        }
    }

    fn update(
        &mut self,
        controls: Controls,
        now: Instant,
        mut emit: impl FnMut(&[InputEvent]) -> io::Result<()>,
    ) -> io::Result<Option<Instant>> {
        self.controls = controls;
        // Reuse the bitsets. evdev's key bitset allocates on construction.
        let held = &mut self.next_held;
        for key in self.mapped_keys.iter() {
            held.remove(key);
        }
        let mut deadline = None;
        self.pulses.retain(|&(_, until)| until > now);
        for gesture in &mut self.gestures {
            if let Some(key) = gesture.update(controls, now) {
                let until = now + TAP_KEY_TIME;
                if let Some(pulse) = self.pulses.iter_mut().find(|pulse| pulse.0 == key) {
                    pulse.1 = until;
                } else {
                    self.pulses.push((key, until));
                }
            }
            if matches!(gesture.press, Press::Held)
                && let Some(key) = gesture.binding.hold
            {
                held.insert(key);
            }
            deadline = deadline.into_iter().chain(gesture.deadline()).min();
        }
        for &(key, until) in &self.pulses {
            held.insert(key);
            deadline = deadline.into_iter().chain(Some(until)).min();
        }
        for stick in &mut self.sticks {
            deadline = deadline
                .into_iter()
                .chain(stick.update(controls, now, held))
                .min();
        }
        self.events.clear();
        // Held outer modifiers wrap movement. Sprint pulses start after movement.
        for outer in [false, true] {
            for key in self
                .held
                .iter()
                .filter(|&key| !held.contains(key) && self.outer_keys.contains(key) == outer)
            {
                self.events.push(key_event(key, 0));
            }
        }
        for (outer, pulsed) in [(true, false), (false, false), (false, true)] {
            for key in held.iter().filter(|&key| {
                !self.held.contains(key)
                    && self.outer_keys.contains(key) == outer
                    && self.pulsed_outer_keys.contains(key) == pulsed
            }) {
                self.events.push(key_event(key, 1));
            }
        }
        if !self.events.is_empty() {
            emit(&self.events)?;
        }
        std::mem::swap(&mut self.held, &mut self.next_held);
        Ok(deadline)
    }
}

fn key_event(key: KeyCode, value: i32) -> InputEvent {
    InputEvent::new(EventType::KEY.0, key.0, value)
}

pub struct Keyboard {
    state: State,
    output: VirtualDevice,
}

impl Keyboard {
    pub fn open(config: Option<&Config>) -> io::Result<Option<Self>> {
        let Some(config) =
            config.filter(|config| !config.buttons.is_empty() || !config.sticks.is_empty())
        else {
            return Ok(None);
        };
        let mut keys: AttributeSet<KeyCode> = config.keys().collect();
        // Advertise the standard key range used by udev to recognize a keyboard.
        for code in KeyCode::KEY_ESC.0..=KeyCode::KEY_D.0 {
            keys.insert(KeyCode(code));
        }
        let output = VirtualDevice::builder()?
            .name("JoyShockRust Virtual Keyboard")
            .with_keys(&keys)?
            .build()?;
        Ok(Some(Self {
            state: State::new(config),
            output,
        }))
    }

    pub fn publish(&mut self, controls: Controls, now: Instant) -> io::Result<Option<Instant>> {
        self.state
            .update(controls, now, |events| self.output.emit(events))
    }

    pub fn tick(&mut self, now: Instant) -> io::Result<Option<Instant>> {
        self.publish(self.state.controls, now)
    }

    pub fn release(&mut self) -> io::Result<()> {
        for key in self.state.held.iter() {
            self.output.emit(&[key_event(key, 0)])?;
        }
        self.state.held = AttributeSet::new();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn config(bindings: &str) -> Config {
        crate::profile::parse(&format!("version = 1\n{bindings}"))
            .unwrap()
            .keyboard
            .unwrap()
    }

    #[test]
    fn tap_hold_and_double_tap_emit_distinct_keys() {
        let config = config(
            r#"
            [[keyboard.bindings]]
            button = "cross"
            tap = "KEY_SPACE"
            hold = "KEY_LEFTSHIFT"
            double_tap = "KEY_R"
        "#,
        );
        let mut state = State::new(&config);
        let start = Instant::now();
        let mut events = Vec::with_capacity(8);
        let allocations = allocation_counter::measure(|| {
            for (ms, pressed, next_deadline) in [
                (0, true, Some(200)),
                (60, false, Some(360)),
                (359, false, Some(360)),
                (360, false, Some(390)), // Single tap after the double-tap window.
                (390, false, None),
                (500, true, Some(700)),
                (550, false, Some(850)),
                (590, true, Some(790)),
                (640, false, Some(670)), // Second short release.
                (670, false, None),
                (800, true, Some(1000)),
                (1000, true, None), // Idle timer, no new input state.
                (1500, false, None),
            ] {
                let mut controls = Controls::default();
                if pressed {
                    controls.buttons.insert(Button::South);
                }
                let deadline = state
                    .update(controls, start + Duration::from_millis(ms), |batch| {
                        events.extend(batch.iter().map(|event| (event.code(), event.value())));
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(
                    deadline,
                    next_deadline.map(|ms| start + Duration::from_millis(ms))
                );
            }
        });
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
        assert_eq!(
            events,
            [
                (KeyCode::KEY_SPACE.0, 1),
                (KeyCode::KEY_SPACE.0, 0),
                (KeyCode::KEY_R.0, 1),
                (KeyCode::KEY_R.0, 0),
                (KeyCode::KEY_LEFTSHIFT.0, 1),
                (KeyCode::KEY_LEFTSHIFT.0, 0),
            ]
        );
    }

    #[test]
    fn hold_only_dpad_bindings_are_immediate_and_share_key_state() {
        let config = config(
            r#"
            [[keyboard.bindings]]
            button = "dpad-up"
            hold = "KEY_W"
            [[keyboard.bindings]]
            button = "dpad-right"
            hold = "KEY_D"
            [[keyboard.bindings]]
            button = "r1"
            hold = "KEY_W"
        "#,
        );
        let mut state = State::new(&config);
        let start = Instant::now();
        let mut events = Vec::new();
        let mut controls = Controls {
            dpad: DPad::UpRight,
            ..Controls::default()
        };
        for (ms, direction, shoulder) in [
            (0, DPad::UpRight, false),
            (1, DPad::Right, true),
            (2, DPad::Neutral, true),
            (3, DPad::Neutral, false),
        ] {
            controls.dpad = direction;
            if shoulder {
                controls.buttons.insert(Button::RightShoulder);
            } else {
                controls.buttons.remove(Button::RightShoulder);
            }
            assert!(
                state
                    .update(controls, start + Duration::from_millis(ms), |batch| {
                        events.extend(batch.iter().map(|event| (event.code(), event.value())));
                        Ok(())
                    })
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(
            events,
            [
                (KeyCode::KEY_W.0, 1),
                (KeyCode::KEY_D.0, 1),
                (KeyCode::KEY_D.0, 0),
                (KeyCode::KEY_W.0, 0)
            ]
        );
    }

    #[test]
    fn analog_stick_walks_runs_and_releases_keys_in_order() {
        let config = config(
            r#"
            [[keyboard.sticks]]
            side = "left"
            deadzone = 0.2
            up = "KEY_W"
            right = "KEY_D"
            down = "KEY_S"
            left = "KEY_A"
            outer = { key = "KEY_LEFTSHIFT", threshold = 0.8, release = 0.7 }
        "#,
        );
        let mut state = State::new(&config);
        let now = Instant::now();
        let mut events = Vec::with_capacity(2);
        let w = KeyCode::KEY_W.0;
        let d = KeyCode::KEY_D.0;
        let shift = KeyCode::KEY_LEFTSHIFT.0;
        let frames = [
            (0.0, -0.19, &[][..]),
            (0.0, -0.40, &[(w, 1)][..]),
            (0.30, -0.30, &[(d, 1)][..]),
            (0.01, -1.0, &[(d, 0), (shift, 1)][..]),
            (0.0, -0.75, &[][..]),
            (0.0, -0.69, &[(shift, 0)][..]),
            (0.0, 0.0, &[(w, 0)][..]),
            (0.0, -1.0, &[(shift, 1), (w, 1)][..]),
            (0.0, 0.0, &[(w, 0), (shift, 0)][..]),
        ];
        let allocations = allocation_counter::measure(|| {
            for (x, y, expected) in frames {
                events.clear();
                let controls = Controls {
                    left_stick: joyshockrust_core::Stick { x, y },
                    ..Controls::default()
                };
                let deadline = state
                    .update(controls, now, |batch| {
                        events.extend(batch.iter().map(|event| (event.code(), event.value())));
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(deadline, None);
                assert_eq!(events, expected);
            }
        });
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }

    #[test]
    fn example_stick_mapping_holds_both_keys_on_every_diagonal() {
        let profile =
            crate::profile::parse(include_str!("../../../profiles/gyro-keyboard.toml")).unwrap();
        let mut state = State::new(profile.keyboard.as_ref().unwrap());
        let now = Instant::now();
        let directions = [
            KeyCode::KEY_W,
            KeyCode::KEY_A,
            KeyCode::KEY_S,
            KeyCode::KEY_D,
        ];
        for (x, y, running, expected) in [
            (-0.3, -0.3, false, [true, true, false, false]),
            (-0.8, -0.8, true, [true, true, false, false]),
            (0.3, -0.3, false, [true, false, false, true]),
            (-0.3, 0.3, false, [false, true, true, false]),
            (0.3, 0.3, false, [false, false, true, true]),
            (0.0, 0.0, false, [false, false, false, false]),
        ] {
            let controls = Controls {
                left_stick: joyshockrust_core::Stick { x, y },
                ..Controls::default()
            };
            state.update(controls, now, |_| Ok(())).unwrap();
            assert_eq!(directions.map(|key| state.held.contains(key)), expected);
            assert_eq!(state.held.contains(KeyCode::KEY_LEFTSHIFT), running);
        }
    }

    #[test]
    fn toggle_outer_zone_pulses_shift_while_preserving_diagonal_movement() {
        let profile = include_str!("../../../profiles/gyro-keyboard.toml")
            .replace("hold = \"KEY_LEFTSHIFT\"", "hold = \"KEY_LEFTCTRL\"")
            .replace("threshold = 0.8", "mode = \"toggle\", threshold = 0.8");
        let config = crate::profile::parse(&profile).unwrap().keyboard.unwrap();
        let mut state = State::new(&config);
        let start = Instant::now();
        let mut events = Vec::with_capacity(3);
        let w = KeyCode::KEY_W.0;
        let a = KeyCode::KEY_A.0;
        let shift = KeyCode::KEY_LEFTSHIFT.0;
        let frames = [
            (0, -0.3, &[(w, 1), (a, 1)][..], None),
            (10, -0.8, &[(shift, 1)][..], Some(40)),
            (40, -0.8, &[(shift, 0)][..], None),
            (80, -0.52, &[][..], None), // Between the two thresholds.
            (90, -0.3, &[(shift, 1)][..], Some(120)),
            (95, -0.8, &[][..], Some(120)), // Re-enter during the exit pulse.
            (120, -0.8, &[(shift, 0)][..], Some(150)),
            (150, -0.8, &[(shift, 1)][..], Some(180)),
            (180, -0.8, &[(shift, 0)][..], None),
            (210, 0.0, &[(w, 0), (a, 0), (shift, 1)][..], Some(240)),
            (240, 0.0, &[(shift, 0)][..], None),
        ];
        let allocations = allocation_counter::measure(|| {
            for (ms, axis, expected, next) in frames {
                events.clear();
                let controls = Controls {
                    left_stick: joyshockrust_core::Stick { x: axis, y: axis },
                    ..Controls::default()
                };
                let deadline = state
                    .update(controls, start + Duration::from_millis(ms), |batch| {
                        events.extend(batch.iter().map(|event| (event.code(), event.value())));
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(events, expected, "{ms} ms");
                assert_eq!(deadline, next.map(|ms| start + Duration::from_millis(ms)));
            }
        });
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }

    #[test]
    fn keep_sprint_pulses_once_per_continuous_movement() {
        let config = config(
            r#"
            [[keyboard.sticks]]
            side = "left"
            up = "KEY_W"
            left = "KEY_A"
            right = "KEY_D"
            outer = { key = "KEY_LEFTSHIFT", mode = "keep_sprint", threshold = 0.8 }
            "#,
        );
        let mut state = State::new(&config);
        let start = Instant::now();
        let w = KeyCode::KEY_W.0;
        let a = KeyCode::KEY_A.0;
        let d = KeyCode::KEY_D.0;
        let shift = KeyCode::KEY_LEFTSHIFT.0;
        let frames = [
            (0, -0.8, -0.8, &[(w, 1), (a, 1), (shift, 1)][..], Some(30)),
            (30, -0.8, -0.8, &[(shift, 0)][..], None),
            (60, -0.3, -0.3, &[][..], None), // Slowing the stick keeps sprint active.
            (70, 0.4, 0.0, &[(w, 0), (a, 0), (d, 1)][..], None),
            (80, 1.0, 0.0, &[][..], None), // Re-entering the zone while moving.
            (90, 0.0, 0.0, &[(d, 0)][..], None), // Center needs no second press.
            (100, 0.0, -0.4, &[(w, 1)][..], None),
            (110, 0.0, -1.0, &[(shift, 1)][..], Some(140)),
            (115, 0.0, 0.0, &[(w, 0)][..], Some(140)),
            (120, 0.0, -1.0, &[(w, 1)][..], Some(140)),
            (140, 0.0, -1.0, &[(shift, 0)][..], Some(170)),
            (170, 0.0, -1.0, &[(shift, 1)][..], Some(200)),
            (200, 0.0, -1.0, &[(shift, 0)][..], None),
            (210, 0.0, 0.0, &[(w, 0)][..], None),
        ];
        let allocations = allocation_counter::measure(|| {
            for (ms, x, y, expected, next) in frames {
                let controls = Controls {
                    left_stick: joyshockrust_core::Stick { x, y },
                    ..Controls::default()
                };
                let mut batches = 0;
                let deadline = state
                    .update(controls, start + Duration::from_millis(ms), |batch| {
                        batches += 1;
                        assert!(
                            batch
                                .iter()
                                .map(|event| (event.code(), event.value()))
                                .eq(expected.iter().copied()),
                            "{ms} ms: {batch:?}"
                        );
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(batches, usize::from(!expected.is_empty()));
                assert_eq!(deadline, next.map(|ms| start + Duration::from_millis(ms)));
            }
        });
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }

    #[test]
    fn right_stick_and_button_can_share_an_outer_key() {
        let config = config(
            r#"
            [[keyboard.sticks]]
            side = "right"
            up = "KEY_W"
            outer = { key = "KEY_LEFTSHIFT", threshold = 0.5 }
            [[keyboard.bindings]]
            button = "r1"
            hold = "KEY_LEFTSHIFT"
        "#,
        );
        let now = Instant::now();
        let mut state = State::new(&config);
        let mut controls = Controls::default();
        let mut events = Vec::new();
        let w = KeyCode::KEY_W.0;
        let shift = KeyCode::KEY_LEFTSHIFT.0;
        controls.left_stick.y = -1.0;
        for (y, r1, expected) in [
            (0.0, false, &[][..]),
            (-0.7, false, &[(shift, 1), (w, 1)][..]),
            (-0.3, true, &[][..]),
            (0.0, true, &[(w, 0)][..]),
            (0.0, false, &[(shift, 0)][..]),
        ] {
            controls.right_stick.y = y;
            if r1 {
                controls.buttons.insert(Button::RightShoulder);
            } else {
                controls.buttons.remove(Button::RightShoulder);
            }
            events.clear();
            state
                .update(controls, now, |batch| {
                    events.extend(batch.iter().map(|event| (event.code(), event.value())));
                    Ok(())
                })
                .unwrap();
            assert_eq!(events, expected);
        }
    }

    #[test]
    fn keyboard_sticks_validate_zone_limits_and_duplicate_sides() {
        let binding = "[[keyboard.sticks]]\nside = 'right'\nup = 'KEY_W'\nouter = { key = 'KEY_LEFTSHIFT', threshold = 0.8, release = 0.7 }\n";
        let valid = config(binding);
        assert_eq!(valid.sticks.len(), 1);
        for invalid in [
            binding.replace("'right'", "'middle'"),
            binding.replace("threshold = 0.8", "threshold = 1.1"),
            binding.replace("release = 0.7", "release = 0.1"),
            binding.replace("release = 0.7", "release = 0.8"),
            binding.replace("threshold = 0.8", "mode = 'tap', threshold = 0.8"),
            binding
                .replace("up = 'KEY_W'\n", "")
                .replace("threshold = 0.8", "mode = 'keep_sprint', threshold = 0.8"),
            format!(
                "{}\n[[keyboard.bindings]]\nbutton = 'r1'\nhold = 'KEY_LEFTSHIFT'\n",
                binding.replace("threshold = 0.8", "mode = 'toggle', threshold = 0.8")
            ),
            format!("{binding}deadzone = nan\n"),
            format!("{binding}{binding}"),
        ] {
            assert!(
                crate::profile::parse(&format!("version = 1\n{invalid}")).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn keyboard_profiles_reject_unknown_keys_and_duplicate_inputs() {
        let binding = "[[keyboard.bindings]]\nbutton = 'cross'\nhold = 'KEY_A'\n";
        for invalid in [
            binding.replace("KEY_A", "BTN_SOUTH"),
            binding.replace("KEY_A", "KEY_DOES_NOT_EXIST"),
            binding.replace("cross", "unknown"),
            format!("{binding}{binding}"),
        ] {
            assert!(crate::profile::parse(&format!("version = 1\n{invalid}")).is_err());
        }
        let profile = crate::profile::parse(&format!(
            "version = 1\n[bridge]\nvirtual_gamepad = false\n{binding}"
        ))
        .unwrap();
        assert!(!profile.bridge.unwrap().virtual_gamepad);
    }

    #[test]
    fn analog_trigger_bindings_keep_independent_thresholds_and_ignore_the_button_bit() {
        let config = config(
            r#"
            [[keyboard.bindings]]
            trigger = "l2:0.35:0.25"
            hold = "KEY_Q"
            [[keyboard.bindings]]
            trigger = "l2:0.8:0.65"
            hold = "KEY_E"
            [[keyboard.bindings]]
            button = "l2"
            hold = "KEY_F"
            "#,
        );
        let mut state = State::new(&config);
        let now = Instant::now();
        let mut controls = Controls {
            right_trigger: 1.0,
            ..Controls::default()
        };
        controls.buttons.insert(Button::LeftTrigger);
        for (travel, expected) in [
            (0.25, [false, false]),
            (0.35, [true, false]),
            (0.8, [true, true]),
            (0.7, [true, true]),
            (0.65, [true, false]),
            (0.3, [true, false]),
            (0.25, [false, false]),
        ] {
            controls.left_trigger = travel;
            assert_eq!(state.update(controls, now, |_| Ok(())).unwrap(), None);
            assert_eq!(
                [KeyCode::KEY_Q, KeyCode::KEY_E].map(|key| state.held.contains(key)),
                expected
            );
            assert!(state.held.contains(KeyCode::KEY_F));
        }
    }

    #[test]
    fn keyboard_profiles_validate_analog_sources() {
        let binding = "[[keyboard.bindings]]\ntrigger = 'r2'\ntap = 'KEY_R'\n";
        for invalid in [
            format!("{binding}button = 'r2'\n"),
            binding.replace("trigger = 'r2'\n", ""),
            binding.replace("r2", "r2:0.6:0.7"),
            binding.replace("r2", "l1"),
            format!("{binding}{}", binding.replace("'r2'", "'r2:0.45:0.35'")),
        ] {
            assert!(crate::profile::parse(&format!("version = 1\n{invalid}")).is_err());
        }
    }
}
