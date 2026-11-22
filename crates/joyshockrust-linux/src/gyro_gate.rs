use joyshockrust_core::{Button, Buttons};

#[derive(Clone, Copy, Default)]
pub enum Binding {
    #[default]
    Always,
    Hold(Button),
    Toggle(Button),
}

pub struct GyroGate {
    binding: Binding,
    previous_pressed: Option<bool>,
    toggled: bool,
}

impl GyroGate {
    pub fn new(binding: Binding) -> Self {
        Self {
            binding,
            previous_pressed: None,
            toggled: false,
        }
    }

    pub fn enabled(&mut self, buttons: Buttons) -> bool {
        match self.binding {
            Binding::Always => true,
            Binding::Hold(button) => buttons.contains(button),
            Binding::Toggle(button) => {
                let pressed = buttons.contains(button);
                if self.previous_pressed.replace(pressed) == Some(false) && pressed {
                    self.toggled = !self.toggled;
                }
                self.toggled
            }
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

    fn pressed(button: Button) -> Buttons {
        let mut buttons = Buttons::default();
        buttons.insert(button);
        buttons
    }

    #[test]
    fn configurable_hold_tracks_only_the_selected_button() {
        let mut gate = GyroGate::new(Binding::Hold(button_by_name("r1").unwrap()));
        assert!(!gate.enabled(pressed(Button::LeftShoulder)));
        assert!(gate.enabled(pressed(Button::RightShoulder)));
        assert!(!gate.enabled(Buttons::default()));
    }

    #[test]
    fn toggle_changes_once_per_press_and_reconnect_restarts_disabled() {
        let binding = Binding::Toggle(Button::RightStick);
        let up = Buttons::default();
        let down = pressed(Button::RightStick);
        let mut gate = GyroGate::new(binding);
        assert!(!gate.enabled(down)); // already held at session start
        assert!(!gate.enabled(up));
        assert!(gate.enabled(down));
        assert!(gate.enabled(down));
        assert!(gate.enabled(up));
        assert!(!gate.enabled(down));
        assert!(!GyroGate::new(binding).enabled(up));
    }
}
