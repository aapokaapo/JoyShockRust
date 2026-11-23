use std::num::NonZeroU16;

use joyshockrust_input::{DUALSENSE_TOUCHPAD_HEIGHT, DUALSENSE_TOUCHPAD_WIDTH, TouchContact};
use serde::Deserialize;

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Settings {
    vertical_step: Option<NonZeroU16>,
    horizontal_step: Option<NonZeroU16>,
}

impl Settings {
    pub(super) fn validate(self) -> Result<Self, &'static str> {
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
    previous: Option<TouchContact>,
    remainder: [i32; 2],
    settings: Settings,
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
        let mut active = contacts.into_iter().flatten();
        let continued = self.previous.and_then(|previous| {
            active
                .clone()
                .find(|contact| contact.id == previous.id)
                .map(|current| (previous, current))
        });
        let Some((previous, current)) = continued else {
            // A different contact starts a fresh swipe, even if one contact remains on the pad.
            self.previous = active.next();
            self.remainder = [0; 2];
            return [0; 2];
        };
        self.previous = Some(current);
        let delta = [
            i32::from(previous.y) - i32::from(current.y),
            i32::from(current.x) - i32::from(previous.x),
        ];
        let steps = [self.settings.vertical_step, self.settings.horizontal_step];
        std::array::from_fn(|axis| {
            let Some(step) = steps[axis] else {
                return 0;
            };
            let step = i32::from(step.get());
            let total = self.remainder[axis] + delta[axis];
            self.remainder[axis] = total % step;
            total / step
        })
    }
}
