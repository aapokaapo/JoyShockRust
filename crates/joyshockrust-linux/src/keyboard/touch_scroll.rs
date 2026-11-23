use std::num::NonZeroU16;

use joyshockrust_input::{DUALSENSE_TOUCHPAD_HEIGHT, DUALSENSE_TOUCHPAD_WIDTH, TouchContact};
use serde::Deserialize;

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Settings {
    vertical_step: Option<NonZeroU16>,
    horizontal_step: Option<NonZeroU16>,
    #[serde(default = "default_fingers")]
    fingers: u8,
}

fn default_fingers() -> u8 {
    1
}

impl Settings {
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

pub(super) struct State {
    previous: Option<Position>,
    remainder: [i32; 2],
    settings: Settings,
}

#[derive(Clone, Copy)]
struct Position {
    ids: (u8, Option<u8>),
    // Two-finger positions store the sum so half-unit midpoint motion is preserved.
    x: i32,
    y: i32,
}

impl State {
    pub(super) fn new(settings: Settings) -> Self {
        Self {
            previous: None,
            remainder: [0; 2],
            settings,
        }
    }

    pub(super) fn advance(&mut self, contacts: [Option<TouchContact>; 2]) -> [i32; 2] {
        let current = self.position(contacts);
        let previous = std::mem::replace(&mut self.previous, current);
        let Some((previous, current)) = previous.zip(current).filter(|(a, b)| a.ids == b.ids)
        else {
            // Reset the swipe when the tracked contact or pair changes.
            self.remainder = [0; 2];
            return [0; 2];
        };
        let delta = [previous.y - current.y, current.x - previous.x];
        let steps = [self.settings.vertical_step, self.settings.horizontal_step];
        std::array::from_fn(|axis| {
            let Some(step) = steps[axis] else {
                return 0;
            };
            let step = i32::from(step.get()) * i32::from(self.settings.fingers);
            let total = self.remainder[axis] + delta[axis];
            self.remainder[axis] = total % step;
            total / step
        })
    }

    fn position(&self, contacts: [Option<TouchContact>; 2]) -> Option<Position> {
        if self.settings.fingers == 2 {
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
