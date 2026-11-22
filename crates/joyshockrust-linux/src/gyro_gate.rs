use std::time::{Duration, Instant};

use joyshockrust_core::{Button, Controls};

const HOLD_TIME: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, Default)]
pub enum Binding {
    #[default]
    Always,
    Hold(Button),
    Toggle(Button),
    TapHold(Button),
    AnalogTrigger(TriggerGate),
}

#[derive(Clone, Copy)]
pub struct TriggerGate {
    side: TriggerSide,
    press: f32,
    release: f32,
}

#[derive(Clone, Copy)]
enum TriggerSide {
    Left,
    Right,
}

impl TriggerGate {
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

    fn travel(self, controls: &Controls) -> f32 {
        match self.side {
            TriggerSide::Left => controls.left_trigger,
            TriggerSide::Right => controls.right_trigger,
        }
    }
}

pub struct GyroGate {
    binding: Binding,
    previous_pressed: Option<bool>,
    active: bool,
    hold_deadline: Option<Instant>,
}

impl GyroGate {
    pub fn new(binding: Binding) -> Self {
        Self {
            binding,
            previous_pressed: None,
            active: false,
            hold_deadline: None,
        }
    }

    pub fn enabled(&mut self, controls: &Controls, clock: impl FnOnce() -> Instant) -> bool {
        let buttons = controls.buttons;
        match self.binding {
            Binding::Always => true,
            Binding::Hold(button) => buttons.contains(button),
            Binding::Toggle(button) => {
                let pressed = buttons.contains(button);
                if self.previous_pressed.replace(pressed) == Some(false) && pressed {
                    self.active = !self.active;
                }
                self.active
            }
            Binding::TapHold(button) => self.tap_hold(buttons.contains(button), clock()),
            Binding::AnalogTrigger(trigger) => {
                let travel = trigger.travel(controls);
                self.active = if self.active {
                    travel > trigger.release
                } else {
                    travel >= trigger.press
                };
                self.active
            }
        }
    }

    fn tap_hold(&mut self, pressed: bool, now: Instant) -> bool {
        let previous = self.previous_pressed.replace(pressed);
        if pressed {
            if previous == Some(false) {
                self.hold_deadline = Some(now + HOLD_TIME);
            }
            self.active || self.hold_deadline.is_some_and(|deadline| now >= deadline)
        } else {
            if self
                .hold_deadline
                .take()
                .is_some_and(|deadline| now < deadline)
            {
                self.active = !self.active;
            }
            self.active
        }
    }
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
    fn configurable_hold_tracks_only_the_selected_button() {
        let mut gate = GyroGate::new(Binding::Hold(button_by_name("r1").unwrap()));
        assert!(!gate.enabled(&pressed(Button::LeftShoulder), Instant::now));
        assert!(gate.enabled(&pressed(Button::RightShoulder), Instant::now));
        assert!(!gate.enabled(&Controls::default(), Instant::now));
    }

    #[test]
    fn toggle_changes_once_per_press_and_reconnect_restarts_disabled() {
        let binding = Binding::Toggle(Button::RightStick);
        let up = Controls::default();
        let down = pressed(Button::RightStick);
        let mut gate = GyroGate::new(binding);
        assert!(!gate.enabled(&down, Instant::now)); // already held at session start
        assert!(!gate.enabled(&up, Instant::now));
        assert!(gate.enabled(&down, Instant::now));
        assert!(gate.enabled(&down, Instant::now));
        assert!(gate.enabled(&up, Instant::now));
        assert!(!gate.enabled(&down, Instant::now));
        assert!(!GyroGate::new(binding).enabled(&up, Instant::now));
    }

    #[test]
    fn tap_toggles_and_long_hold_preserves_the_latched_state() {
        let binding = Binding::TapHold(Button::RightStick);
        let mut gate = GyroGate::new(binding);
        let up = Controls::default();
        let down = pressed(Button::RightStick);
        let start = Instant::now();
        let mut update =
            |controls, ms| gate.enabled(controls, || start + Duration::from_millis(ms));

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
        let mut gate = GyroGate::new(Binding::TapHold(Button::RightStick));
        let down = pressed(Button::RightStick);
        let start = Instant::now();
        assert!(!gate.enabled(&down, || start));
        assert!(!gate.enabled(&down, || start + HOLD_TIME));
        assert!(!gate.enabled(&Controls::default(), || start + HOLD_TIME));
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
            assert_eq!(gate.enabled(&controls, Instant::now), enabled, "{travel}");
        }
        controls.right_trigger = 0.5;
        assert!(!GyroGate::new(binding).enabled(&controls, Instant::now));
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
