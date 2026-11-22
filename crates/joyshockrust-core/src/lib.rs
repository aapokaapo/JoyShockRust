//! Transport-independent controller state.

#![no_std]

/// Face buttons are named by their position on the controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    West,
    South,
    East,
    North,
    LeftShoulder,
    RightShoulder,
    LeftTrigger,
    RightTrigger,
    Select,
    Start,
    LeftStick,
    RightStick,
    Home,
    TouchpadClick,
    MicrophoneMute,
    LeftFunction,
    RightFunction,
    LeftPaddle,
    RightPaddle,
}

/// The digital buttons held in a report. Trigger buttons are independent of
/// their corresponding analog axes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Buttons(u32);

impl Buttons {
    pub fn insert(&mut self, button: Button) {
        self.0 |= 1 << button as u32;
    }

    pub fn remove(&mut self, button: Button) {
        self.0 &= !(1 << button as u32);
    }

    pub const fn contains(self, button: Button) -> bool {
        self.0 & (1 << button as u32) != 0
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum DPad {
    #[default]
    Neutral,
    Up,
    UpRight,
    Right,
    DownRight,
    Down,
    DownLeft,
    Left,
    UpLeft,
}

/// Centered axes in `[-1.0, 1.0]`. Positive X is right, positive Y is down.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Stick {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Controls {
    pub buttons: Buttons,
    pub dpad: DPad,
    pub left_stick: Stick,
    pub right_stick: Stick,
    /// Analog trigger travel in `[0.0, 1.0]`.
    pub left_trigger: f32,
    /// Analog trigger travel in `[0.0, 1.0]`.
    pub right_trigger: f32,
}
