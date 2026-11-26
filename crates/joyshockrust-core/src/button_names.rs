use crate::Button;

// One list defines both directions of the profile name mapping.
macro_rules! button_names {
    ($($button:ident => $name:literal),+ $(,)?) => {
        impl Button {
            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Self::$button),)+
                    _ => None,
                }
            }

            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$button => $name,)+
                }
            }
        }
    };
}

button_names! {
    South => "cross",
    East => "circle",
    West => "square",
    North => "triangle",
    LeftShoulder => "l1",
    RightShoulder => "r1",
    LeftTrigger => "l2",
    RightTrigger => "r2",
    LeftStick => "l3",
    RightStick => "r3",
    Select => "create",
    Start => "options",
    Home => "ps",
    TouchpadClick => "touchpad",
    MicrophoneMute => "mute",
    LeftFunction => "fn-left",
    RightFunction => "fn-right",
    LeftPaddle => "paddle-left",
    RightPaddle => "paddle-right",
    DPadUp => "dpad-up",
    DPadRight => "dpad-right",
    DPadDown => "dpad-down",
    DPadLeft => "dpad-left",
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_names_round_trip() {
        let names = [
            "cross",
            "circle",
            "square",
            "triangle",
            "l1",
            "r1",
            "l2",
            "r2",
            "l3",
            "r3",
            "create",
            "options",
            "ps",
            "touchpad",
            "mute",
            "fn-left",
            "fn-right",
            "paddle-left",
            "paddle-right",
            "dpad-up",
            "dpad-right",
            "dpad-down",
            "dpad-left",
        ];
        for name in names {
            assert_eq!(Button::from_name(name).unwrap().name(), name);
        }
        assert_eq!(Button::from_name("Cross"), None);
    }
}
