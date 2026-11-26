//! Transport-independent controller state.

#![no_std]

mod button_names;
pub mod touch_region;
pub mod trigger;

pub use trigger::{DEFAULT_TRIGGER_THRESHOLDS, validate_trigger_thresholds};

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
    DPadUp,
    DPadRight,
    DPadDown,
    DPadLeft,
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

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub const fn symmetric_difference(self, other: Self) -> Self {
        Self(self.0 ^ other.0)
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

impl DPad {
    const UP: Buttons = Buttons(1 << Button::DPadUp as u32);
    const RIGHT: Buttons = Buttons(1 << Button::DPadRight as u32);
    const DOWN: Buttons = Buttons(1 << Button::DPadDown as u32);
    const LEFT: Buttons = Buttons(1 << Button::DPadLeft as u32);
    pub const BUTTONS: Buttons = Self::UP
        .union(Self::RIGHT)
        .union(Self::DOWN)
        .union(Self::LEFT);

    /// The cardinal inputs represented by this hat position.
    pub const fn buttons(self) -> Buttons {
        match self {
            Self::Neutral => Buttons(0),
            Self::Up => Self::UP,
            Self::UpRight => Self::UP.union(Self::RIGHT),
            Self::Right => Self::RIGHT,
            Self::DownRight => Self::DOWN.union(Self::RIGHT),
            Self::Down => Self::DOWN,
            Self::DownLeft => Self::DOWN.union(Self::LEFT),
            Self::Left => Self::LEFT,
            Self::UpLeft => Self::UP.union(Self::LEFT),
        }
    }

    /// Combine mapped cardinal outputs. Opposites cancel on their axis.
    pub fn from_buttons(buttons: Buttons) -> Self {
        let x = i8::from(buttons.contains(Button::DPadRight))
            - i8::from(buttons.contains(Button::DPadLeft));
        let y = i8::from(buttons.contains(Button::DPadDown))
            - i8::from(buttons.contains(Button::DPadUp));
        match (x, y) {
            (0, -1) => Self::Up,
            (1, -1) => Self::UpRight,
            (1, 0) => Self::Right,
            (1, 1) => Self::DownRight,
            (0, 1) => Self::Down,
            (-1, 1) => Self::DownLeft,
            (-1, 0) => Self::Left,
            (-1, -1) => Self::UpLeft,
            _ => Self::Neutral,
        }
    }
}

/// Centered axes in `[-1.0, 1.0]`. Positive X is right, positive Y is down.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Stick {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum StickSide {
    Left,
    #[default]
    Right,
}

impl StickSide {
    pub fn select<T>(self, left: T, right: T) -> T {
        match self {
            Self::Left => left,
            Self::Right => right,
        }
    }
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
