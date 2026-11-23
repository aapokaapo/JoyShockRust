use std::{
    num::{NonZeroU16, NonZeroU32},
    time::{Duration, Instant},
};

use joyshockrust_input::{DUALSENSE_TOUCHPAD_HEIGHT, DUALSENSE_TOUCHPAD_WIDTH, TouchContact};
use serde::Deserialize;

use super::HOLD_TIME;

const DEFAULT_TAP_MAX_TRAVEL: u16 = 40;

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ScrollSettings {
    vertical_step: Option<NonZeroU16>,
    horizontal_step: Option<NonZeroU16>,
    #[serde(default)]
    invert_vertical: bool,
    #[serde(default)]
    invert_horizontal: bool,
    #[serde(default)]
    axis_lock: bool,
    #[serde(default = "default_fingers")]
    fingers: u8,
}

fn default_fingers() -> u8 {
    1
}

impl ScrollSettings {
    pub(super) fn validate(self) -> Result<Self, &'static str> {
        if !matches!(self.fingers, 1 | 2) {
            return Err("touch scroll fingers must be 1 or 2");
        }
        if self.vertical_step.is_none() && self.horizontal_step.is_none() {
            return Err("touch scrolling requires vertical_step or horizontal_step");
        }
        if self
            .vertical_step
            .is_some_and(|step| step.get() >= DUALSENSE_TOUCHPAD_HEIGHT)
            || self
                .horizontal_step
                .is_some_and(|step| step.get() >= DUALSENSE_TOUCHPAD_WIDTH)
        {
            return Err("touch scroll step must be smaller than the touchpad axis length");
        }
        Ok(self)
    }
}

pub(super) struct ScrollState {
    tracker: Tracker,
    remainder: [i32; 2],
    locked_axis: Option<usize>,
    settings: ScrollSettings,
}

#[derive(Clone, Copy)]
struct Position {
    ids: (u8, Option<u8>),
    // Two-finger positions store the sum so half-unit midpoint motion is preserved.
    x: i32,
    y: i32,
}

impl ScrollState {
    pub(super) fn new(settings: ScrollSettings) -> Self {
        Self {
            tracker: Tracker::default(),
            remainder: [0; 2],
            locked_axis: None,
            settings,
        }
    }

    pub(super) fn advance(&mut self, contacts: [Option<TouchContact>; 2]) -> [i32; 2] {
        let Some([dx, dy]) = self.tracker.advance(contacts, self.settings.fingers) else {
            self.remainder = [0; 2];
            self.locked_axis = None;
            return [0; 2];
        };
        let delta = [-dy, dx];
        let steps = [self.settings.vertical_step, self.settings.horizontal_step];
        let inverted = [
            self.settings.invert_vertical,
            self.settings.invert_horizontal,
        ];
        let mut ticks: [i32; 2] = std::array::from_fn(|axis| {
            let Some(step) = steps[axis] else {
                return 0;
            };
            let step = i32::from(step.get()) * i32::from(self.settings.fingers);
            let total = self.remainder[axis] + delta[axis];
            self.remainder[axis] = total % step;
            let ticks = total / step;
            if inverted[axis] { -ticks } else { ticks }
        });
        if self.settings.axis_lock && ticks != [0; 2] {
            // Lock when the swipe first emits a step. Equal magnitudes favor vertical.
            let axis = *self
                .locked_axis
                .get_or_insert_with(|| usize::from(ticks[1].abs() > ticks[0].abs()));
            ticks[1 - axis] = 0;
        }
        ticks
    }
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct PointerSettings {
    sensitivity_x: Option<f64>,
    sensitivity_y: Option<f64>,
    #[serde(default)]
    pub(super) tap_to_click: bool,
    #[serde(default)]
    pub(super) two_finger_tap_to_click: bool,
    tap_window_ms: Option<NonZeroU32>,
    tap_max_travel: Option<u16>,
}

impl PointerSettings {
    pub(super) fn validate(self) -> Result<Self, &'static str> {
        if !self
            .sensitivity()
            .into_iter()
            .all(|value| (0.0..=100.0).contains(&value))
        {
            return Err("touch pointer sensitivity must be between 0 and 100");
        }
        Ok(self)
    }

    fn sensitivity(self) -> [f64; 2] {
        [
            self.sensitivity_x.unwrap_or(0.5),
            self.sensitivity_y.unwrap_or(0.5),
        ]
    }
}

pub(super) struct PointerState {
    tracker: Tracker,
    remainder: [f64; 2],
    sensitivity: [f64; 2],
}

pub(super) struct TapState {
    phase: TapPhase,
    window: Duration,
    max_travel_squared: i64,
}

#[derive(Default)]
enum TapPhase {
    #[default]
    Idle,
    Candidate {
        start: Instant,
        contacts: TapContacts,
    },
    Blocked,
}

#[derive(Clone, Copy)]
enum TapContacts {
    One(TouchContact),
    Two(TouchContact, TouchContact),
    Remaining(TouchContact),
}

pub(super) enum TapCount {
    One,
    Two,
}

impl TapState {
    pub(super) fn new(settings: PointerSettings) -> Self {
        let travel = i64::from(settings.tap_max_travel.unwrap_or(DEFAULT_TAP_MAX_TRAVEL));
        Self {
            phase: TapPhase::Idle,
            window: settings
                .tap_window_ms
                .map_or(HOLD_TIME, |ms| Duration::from_millis(u64::from(ms.get()))),
            max_travel_squared: travel * travel,
        }
    }

    pub(super) fn advance(
        &mut self,
        contacts: [Option<TouchContact>; 2],
        cancelled: bool,
        now: Instant,
    ) -> Option<TapCount> {
        let (first, second) = match contacts {
            [None, None] => {
                return match std::mem::take(&mut self.phase) {
                    TapPhase::Candidate { start, contacts }
                        if !cancelled && now.duration_since(start) < self.window =>
                    {
                        Some(match contacts {
                            TapContacts::One(_) => TapCount::One,
                            TapContacts::Two(..) | TapContacts::Remaining(_) => TapCount::Two,
                        })
                    }
                    _ => None,
                };
            }
            [Some(contact), None] | [None, Some(contact)] if !cancelled => (contact, None),
            [Some(first), Some(second)] if !cancelled && first.id != second.id => {
                (first, Some(second))
            }
            _ => {
                self.phase = TapPhase::Blocked;
                return None;
            }
        };
        self.phase = match self.phase {
            TapPhase::Idle => TapPhase::Candidate {
                start: now,
                contacts: second.map_or(TapContacts::One(first), |second| {
                    TapContacts::Two(first, second)
                }),
            },
            TapPhase::Candidate { start, contacts } => {
                match contacts.observe(first, second, self.max_travel_squared) {
                    Some(contacts) => TapPhase::Candidate { start, contacts },
                    None => TapPhase::Blocked,
                }
            }
            TapPhase::Blocked => TapPhase::Blocked,
        };
        None
    }
}

impl TapContacts {
    fn observe(
        self,
        first: TouchContact,
        second: Option<TouchContact>,
        max_travel_squared: i64,
    ) -> Option<Self> {
        let near = |origin: TouchContact, contact: TouchContact| {
            let dx = i64::from(contact.x) - i64::from(origin.x);
            let dy = i64::from(contact.y) - i64::from(origin.y);
            contact.id == origin.id && dx * dx + dy * dy <= max_travel_squared
        };
        match (self, second) {
            (Self::One(origin) | Self::Remaining(origin), None) if near(origin, first) => {
                Some(self)
            }
            (Self::One(origin), Some(second)) if near(origin, first) => {
                Some(Self::Two(origin, second))
            }
            (Self::One(origin), Some(second)) if near(origin, second) => {
                Some(Self::Two(origin, first))
            }
            (Self::Two(a, b), Some(second))
                if (near(a, first) && near(b, second)) || (near(b, first) && near(a, second)) =>
            {
                Some(self)
            }
            (Self::Two(a, b), None) => [a, b]
                .into_iter()
                .find(|&origin| near(origin, first))
                .map(Self::Remaining),
            _ => None,
        }
    }
}

impl PointerState {
    pub(super) fn new(settings: PointerSettings) -> Self {
        Self {
            tracker: Tracker::default(),
            remainder: [0.0; 2],
            sensitivity: settings.sensitivity(),
        }
    }

    pub(super) fn advance(&mut self, mut contacts: [Option<TouchContact>; 2]) -> [i32; 2] {
        if contacts.iter().all(Option::is_some) {
            // Reserve two-finger swipes for scrolling and discard the old pointer baseline.
            contacts = [None; 2];
        }
        let Some(delta) = self.tracker.advance(contacts, 1) else {
            self.remainder = [0.0; 2];
            return [0; 2];
        };
        std::array::from_fn(|axis| {
            let total = self.remainder[axis] + f64::from(delta[axis]) * self.sensitivity[axis];
            let movement = total.trunc();
            self.remainder[axis] = total - movement;
            movement as i32
        })
    }
}

#[derive(Default)]
struct Tracker {
    previous: Option<Position>,
}

impl Tracker {
    fn advance(&mut self, contacts: [Option<TouchContact>; 2], fingers: u8) -> Option<[i32; 2]> {
        let current = self.position(contacts, fingers);
        let previous = std::mem::replace(&mut self.previous, current);
        let (previous, current) = previous.zip(current).filter(|(a, b)| a.ids == b.ids)?;
        Some([current.x - previous.x, current.y - previous.y])
    }

    fn position(&self, contacts: [Option<TouchContact>; 2], fingers: u8) -> Option<Position> {
        if fingers == 2 {
            let [Some(first), Some(second)] = contacts else {
                return None;
            };
            if first.id == second.id {
                return None;
            }
            return Some(Position {
                ids: (first.id.min(second.id), Some(first.id.max(second.id))),
                x: i32::from(first.x) + i32::from(second.x),
                y: i32::from(first.y) + i32::from(second.y),
            });
        }

        let mut active = contacts.into_iter().flatten();
        let contact = self
            .previous
            .and_then(|previous| active.clone().find(|contact| contact.id == previous.ids.0))
            .or_else(|| active.next())?;
        Some(Position {
            ids: (contact.id, None),
            x: i32::from(contact.x),
            y: i32::from(contact.y),
        })
    }
}
