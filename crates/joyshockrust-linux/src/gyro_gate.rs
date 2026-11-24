use std::time::{Duration, Instant};

use joyshockrust_core::{Button, Controls};
use joyshockrust_input::TouchContact;

use crate::touchpad::TouchRegion;

pub(crate) const HOLD_TIME: Duration = Duration::from_millis(200);
pub(crate) const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Default, PartialEq)]
pub enum Binding {
    #[default]
    Always,
    Hold(DigitalInput),
    DisableWhileHeld(DigitalInput),
    Toggle(DigitalInput),
    TapHold(DigitalInput),
    DoubleTap(DigitalInput),
    Chord(DigitalInput, DigitalInput),
    AnalogTrigger(TriggerGate),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DigitalInput {
    Button(Button),
    Touch(TouchRegion),
}

impl From<Button> for DigitalInput {
    fn from(button: Button) -> Self {
        Self::Button(button)
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct TriggerGate {
    side: TriggerSide,
    press: f32,
    release: f32,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum TriggerSide {
    Left,
    Right,
}

impl TriggerGate {
    pub(crate) fn from_percent(side: TriggerSide, press: u8, release: u8) -> Self {
        Self {
            side,
            press: f32::from(press) / 100.0,
            release: f32::from(release) / 100.0,
        }
    }

    pub(crate) fn within_travel(mut self, percent: u8) -> Self {
        let scale = f32::from(percent) / 100.0;
        self.press *= scale;
        self.release *= scale;
        self
    }

    pub fn parse(spec: &str) -> Result<Self, &'static str> {
        let mut fields = spec.split(':');
        let side = match fields.next() {
            Some("l2") => TriggerSide::Left,
            Some("r2") => TriggerSide::Right,
            _ => return Err("expected analog trigger l2 or r2"),
        };
        let (press, release) = match (fields.next(), fields.next(), fields.next()) {
            (None, None, None) => (0.45, 0.35),
            (Some(press), Some(release), None) => (
                press
                    .parse()
                    .map_err(|_| "invalid trigger press threshold")?,
                release
                    .parse()
                    .map_err(|_| "invalid trigger release threshold")?,
            ),
            _ => return Err("expected l2 or r2 with optional :PRESS:RELEASE thresholds"),
        };
        if !(0.0..=1.0).contains(&release) || !(0.0..=1.0).contains(&press) || release >= press {
            return Err("trigger thresholds must satisfy 0 <= release < press <= 1");
        }
        Ok(Self {
            side,
            press,
            release,
        })
    }

    pub(crate) fn partial(spec: &str) -> Result<Option<(Self, Self)>, &'static str> {
        let fields = spec.split(':').collect::<Vec<_>>();
        let &[side, press, release, full_press, full_release] = fields.as_slice() else {
            return Ok(None);
        };
        let lower = Self::parse(&format!("{side}:{press}:{release}"))?;
        let upper = Self::parse(&format!("{side}:{full_press}:{full_release}"))?;
        if lower.press >= upper.release {
            return Err("partial trigger thresholds must satisfy partial press < full release");
        }
        Ok(Some((lower, upper)))
    }

    pub(crate) fn side(self) -> TriggerSide {
        self.side
    }

    pub(crate) fn pressed(self, controls: &Controls, was_pressed: bool) -> bool {
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

pub struct GyroGate {
    binding: Binding,
    previous_pressed: Option<bool>,
    active: bool,
    hold_deadline: Option<Instant>,
    second_tap_deadline: Option<Instant>,
}

impl GyroGate {
    pub fn new(binding: Binding) -> Self {
        Self {
            binding,
            previous_pressed: None,
            active: false,
            hold_deadline: None,
            second_tap_deadline: None,
        }
    }

    pub fn enabled(
        &mut self,
        controls: &Controls,
        contacts: &[Option<TouchContact>; 2],
        clock: impl FnOnce() -> Instant,
    ) -> bool {
        let pressed = |input| match input {
            DigitalInput::Button(button) => controls.buttons.contains(button),
            DigitalInput::Touch(region) => region.pressed(contacts),
        };
        match self.binding {
            Binding::Always => true,
            Binding::Hold(input) => pressed(input),
            Binding::DisableWhileHeld(input) => !pressed(input),
            Binding::Chord(first, second) => pressed(first) && pressed(second),
            Binding::Toggle(input) => {
                let pressed = pressed(input);
                if self.previous_pressed.replace(pressed) == Some(false) && pressed {
                    self.active = !self.active;
                }
                self.active
            }
            Binding::TapHold(input) => self.tap_hold(pressed(input), clock()),
            Binding::DoubleTap(input) => self.double_tap(pressed(input), clock()),
            Binding::AnalogTrigger(trigger) => {
                self.active = trigger.pressed(controls, self.active);
                self.active
            }
        }
    }

    fn tap_hold(&mut self, pressed: bool, now: Instant) -> bool {
        let deadline = self.press_deadline(pressed, now);
        if pressed {
            self.active || deadline.is_some_and(|deadline| now >= deadline)
        } else {
            if deadline.is_some_and(|deadline| now < deadline) {
                self.active = !self.active;
            }
            self.active
        }
    }

    fn double_tap(&mut self, pressed: bool, now: Instant) -> bool {
        let press_deadline = self.press_deadline(pressed, now);
        if let Some(deadline) = press_deadline.filter(|_| !pressed) {
            let pending = self.second_tap_deadline.take();
            if now < deadline {
                if pending.is_some_and(|deadline| now <= deadline) {
                    self.active = !self.active;
                } else {
                    self.second_tap_deadline = Some(now + DOUBLE_TAP_WINDOW);
                }
            }
        }
        self.active
    }

    fn press_deadline(&mut self, pressed: bool, now: Instant) -> Option<Instant> {
        let previous = self.previous_pressed.replace(pressed);
        if !pressed {
            return self.hold_deadline.take();
        }
        if previous == Some(false) {
            self.hold_deadline = Some(now + HOLD_TIME);
        }
        self.hold_deadline
    }
}

pub fn chord_by_name(spec: &str) -> Result<Binding, &'static str> {
    parse_chord(spec, |input| {
        button_by_name(input)
            .map(DigitalInput::Button)
            .ok_or("unknown gyro chord button")
    })
}

pub fn parse_chord<E: From<&'static str>>(
    spec: &str,
    resolve: impl Fn(&str) -> Result<DigitalInput, E>,
) -> Result<Binding, E> {
    let (first, second) = spec
        .split_once(',')
        .ok_or("expected two comma-separated gyro chord inputs")?;
    let first = resolve(first)?;
    let second = resolve(second)?;
    if first == second {
        return Err("select two distinct gyro chord inputs".into());
    }
    Ok(Binding::Chord(first, second))
}

pub fn button_by_name(name: &str) -> Option<Button> {
    Some(match name {
        "cross" => Button::South,
        "circle" => Button::East,
        "square" => Button::West,
        "triangle" => Button::North,
        "l1" => Button::LeftShoulder,
        "r1" => Button::RightShoulder,
        "l2" => Button::LeftTrigger,
        "r2" => Button::RightTrigger,
        "l3" => Button::LeftStick,
        "r3" => Button::RightStick,
        "create" => Button::Select,
        "options" => Button::Start,
        "ps" => Button::Home,
        "touchpad" => Button::TouchpadClick,
        "mute" => Button::MicrophoneMute,
        "fn-left" => Button::LeftFunction,
        "fn-right" => Button::RightFunction,
        "paddle-left" => Button::LeftPaddle,
        "paddle-right" => Button::RightPaddle,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pressed(button: Button) -> Controls {
        let mut controls = Controls::default();
        controls.buttons.insert(button);
        controls
    }

    #[test]
    fn chord_tracks_both_buttons_regardless_of_press_order() {
        let left = pressed(Button::LeftShoulder);
        let right = pressed(Button::RightShoulder);
        let mut both = left;
        both.buttons.insert(Button::RightShoulder);
        let binding = chord_by_name("l1,r1").unwrap();
        for (first, remaining) in [(left, right), (right, left)] {
            let mut gate = GyroGate::new(binding);
            assert!(!gate.enabled(&first, &[None; 2], Instant::now));
            assert!(gate.enabled(&both, &[None; 2], Instant::now));
            assert!(!gate.enabled(&remaining, &[None; 2], Instant::now));
            assert!(gate.enabled(&both, &[None; 2], Instant::now));
            assert!(!gate.enabled(&Controls::default(), &[None; 2], Instant::now));
        }
    }

    #[test]
    fn malformed_chords_are_rejected() {
        for spec in ["l1", "l1,l1", "l1,", ",r1", "l1,r1,r2", "l1,unknown"] {
            assert!(chord_by_name(spec).is_err(), "{spec}");
        }
    }

    #[test]
    fn configurable_hold_tracks_only_the_selected_button() {
        let mut gate = GyroGate::new(Binding::Hold(button_by_name("r1").unwrap().into()));
        assert!(!gate.enabled(&pressed(Button::LeftShoulder), &[None; 2], Instant::now));
        assert!(gate.enabled(&pressed(Button::RightShoulder), &[None; 2], Instant::now));
        assert!(!gate.enabled(&Controls::default(), &[None; 2], Instant::now));
    }

    #[test]
    fn toggle_changes_once_per_press_and_reconnect_restarts_disabled() {
        let binding = Binding::Toggle(Button::RightStick.into());
        let up = Controls::default();
        let down = pressed(Button::RightStick);
        let mut gate = GyroGate::new(binding);
        assert!(!gate.enabled(&down, &[None; 2], Instant::now)); // already held at session start
        assert!(!gate.enabled(&up, &[None; 2], Instant::now));
        assert!(gate.enabled(&down, &[None; 2], Instant::now));
        assert!(gate.enabled(&down, &[None; 2], Instant::now));
        assert!(gate.enabled(&up, &[None; 2], Instant::now));
        assert!(!gate.enabled(&down, &[None; 2], Instant::now));
        assert!(!GyroGate::new(binding).enabled(&up, &[None; 2], Instant::now));
    }

    #[test]
    fn tap_toggles_and_long_hold_preserves_the_latched_state() {
        let binding = Binding::TapHold(Button::RightStick.into());
        let mut gate = GyroGate::new(binding);
        let up = Controls::default();
        let down = pressed(Button::RightStick);
        let start = Instant::now();
        let mut update =
            |controls, ms| gate.enabled(controls, &[None; 2], || start + Duration::from_millis(ms));

        assert!(!update(&up, 0));
        assert!(!update(&down, 10));
        assert!(update(&up, 209)); // 199 ms tap toggles on
        assert!(update(&down, 250));
        assert!(update(&up, 450)); // exact cutoff, no intervening report
        assert!(update(&down, 500));
        assert!(!update(&up, 600)); // next tap toggles off
        assert!(!update(&down, 650));
        assert!(!update(&down, 849));
        assert!(update(&down, 850));
        assert!(!update(&up, 851));
    }

    #[test]
    fn button_held_at_connection_does_not_start_tap_or_hold() {
        let mut gate = GyroGate::new(Binding::TapHold(Button::RightStick.into()));
        let down = pressed(Button::RightStick);
        let start = Instant::now();
        assert!(!gate.enabled(&down, &[None; 2], || start));
        assert!(!gate.enabled(&down, &[None; 2], || start + HOLD_TIME));
        assert!(!gate.enabled(&Controls::default(), &[None; 2], || start + HOLD_TIME));
    }

    #[test]
    fn double_tap_changes_state_on_second_release_once_per_pair() {
        let mut gate = GyroGate::new(Binding::DoubleTap(Button::RightStick.into()));
        let up = Controls::default();
        let down = pressed(Button::RightStick);
        let start = Instant::now();
        for (controls, ms, enabled) in [
            (&up, 0, false),
            (&down, 10, false),
            (&up, 50, false),
            (&down, 200, false),
            (&down, 300, false),
            (&up, 350, true), // inclusive 300 ms release-to-release cutoff
            (&up, 351, true),
            (&down, 400, true),
            (&up, 450, true),
            (&down, 500, true),
            (&up, 550, false),
        ] {
            assert_eq!(
                gate.enabled(controls, &[None; 2], || start + Duration::from_millis(ms)),
                enabled,
                "{ms}"
            );
        }
    }

    #[test]
    fn long_presses_and_expired_taps_do_not_complete_a_pair() {
        let mut gate = GyroGate::new(Binding::DoubleTap(Button::RightStick.into()));
        let up = Controls::default();
        let down = pressed(Button::RightStick);
        let start = Instant::now();
        for (controls, ms) in [
            (&down, 0), // held when connecting
            (&up, 10),
            (&down, 20),
            (&up, 40),
            (&down, 330),
            (&up, 341), // 301 ms after first release, starts a fresh pair
            (&down, 400),
            (&up, 600), // 200 ms hold, cancels the pending pair
            (&down, 610),
            (&up, 620),
        ] {
            assert!(
                !gate.enabled(controls, &[None; 2], || start + Duration::from_millis(ms)),
                "{ms}"
            );
        }
    }

    #[test]
    fn analog_trigger_hysteresis_ignores_other_trigger_and_digital_button() {
        let binding = Binding::AnalogTrigger(TriggerGate::parse("r2:0.6:0.4").unwrap());
        let mut gate = GyroGate::new(binding);
        let mut controls = pressed(Button::RightTrigger);
        controls.left_trigger = 1.0;

        for (travel, enabled) in [
            (0.0, false),
            (0.59, false),
            (0.6, true),
            (0.59, true),
            (0.41, true),
            (0.4, false),
            (0.41, false),
            (1.0, true),
        ] {
            controls.right_trigger = travel;
            assert_eq!(
                gate.enabled(&controls, &[None; 2], Instant::now),
                enabled,
                "{travel}"
            );
        }
        controls.right_trigger = 0.5;
        assert!(!GyroGate::new(binding).enabled(&controls, &[None; 2], Instant::now));
    }

    #[test]
    fn trigger_specs_require_ordered_finite_thresholds() {
        for spec in [
            "l1",
            "l2:",
            "r2:0.5",
            "r2:0.5:0.5",
            "l2:0.1:0.2",
            "r2:NaN:0.1",
            "r2:1.1:0.5",
            "l2:1:-0.3",
            "l2:1:0:extra",
        ] {
            assert!(TriggerGate::parse(spec).is_err(), "{spec}");
        }
        let default = TriggerGate::parse("l2").unwrap();
        assert_eq!((default.press, default.release), (0.45, 0.35));
        assert!(TriggerGate::parse("r2:1:0").is_ok());
    }
}
