use std::{io, time::Instant};

use evdev::{
    AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode, uinput::VirtualDevice,
};
use joyshockrust_config::linux_keys::is_mouse_button;
use joyshockrust_input::InputReport;
use joyshockrust_mapping::keyboard::{Event, Input, KeyKind, RelativeAxis};

use joyshockrust_mapping::keyboard::Config;

use crate::status::StatusSocket;

struct State {
    engine: joyshockrust_mapping::keyboard::State,
    encoded: Vec<InputEvent>,
}

impl State {
    fn new(config: &Config) -> Self {
        let engine = joyshockrust_mapping::keyboard::State::new(config);
        Self {
            encoded: Vec::with_capacity(engine.event_capacity()),
            engine,
        }
    }

    fn update(
        &mut self,
        input: Input,
        now: Instant,
        mut emit: impl FnMut(&[InputEvent]) -> io::Result<()>,
    ) -> io::Result<Option<Instant>> {
        self.engine.update(input, now, |events| {
            encode_batch(&mut self.encoded, events);
            emit(&self.encoded)
        })
    }

    fn release(&mut self, mut emit: impl FnMut(&[InputEvent]) -> io::Result<()>) -> io::Result<()> {
        self.engine.release(|events| {
            encode_batch(&mut self.encoded, events);
            emit(&self.encoded)
        })
    }

    #[cfg(test)]
    fn held_keys(&self) -> impl Iterator<Item = KeyCode> + '_ {
        self.engine.held_keys().map(KeyCode)
    }

    #[cfg(test)]
    fn is_held(&self, key: KeyCode) -> bool {
        self.held_keys().any(|held| held == key)
    }
}

fn encode_batch(batch: &mut Vec<InputEvent>, events: &[Event]) {
    batch.clear();
    batch.extend(events.iter().copied().map(|event| match event {
        Event::Key { id, pressed } => InputEvent::new(EventType::KEY.0, id, i32::from(pressed)),
        Event::Relative { axis, delta } => {
            let code = match axis {
                RelativeAxis::X => RelativeAxisCode::REL_X,
                RelativeAxis::Y => RelativeAxisCode::REL_Y,
                RelativeAxis::Wheel => RelativeAxisCode::REL_WHEEL,
                RelativeAxis::HorizontalWheel => RelativeAxisCode::REL_HWHEEL,
            };
            InputEvent::new(EventType::RELATIVE.0, code.0, delta)
        }
    }));
}

pub struct Keyboard<'a> {
    state: State,
    outputs: Outputs<'a>,
}

struct Outputs<'a> {
    keyboard: Option<VirtualDevice>,
    mouse: Option<VirtualDevice>,
    keys: Vec<InputEvent>,
    buttons: Vec<InputEvent>,
    status: Option<&'a StatusSocket>,
}

impl Outputs<'_> {
    fn emit(&mut self, events: &[InputEvent]) -> io::Result<()> {
        let status = self.status;
        let send = |device: &mut VirtualDevice, target, events: &[InputEvent]| {
            device.emit(events)?;
            if let Some(status) = status {
                status.observe_mapping_outputs(target == Target::Keyboard, target == Target::Mouse);
            }
            Ok(())
        };
        match (&mut self.keyboard, &mut self.mouse) {
            (Some(keyboard), Some(mouse)) => emit_mixed_batches(
                events,
                &mut self.keys,
                &mut self.buttons,
                |target, events| match target {
                    Target::Keyboard => send(keyboard, target, events),
                    Target::Mouse => send(mouse, target, events),
                },
            ),
            (Some(keyboard), None) => send(keyboard, Target::Keyboard, events),
            (None, Some(mouse)) => send(mouse, Target::Mouse, events),
            (None, None) => unreachable!("mapping has an output device"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Keyboard,
    Mouse,
}

fn emit_mixed_batches(
    events: &[InputEvent],
    keys: &mut Vec<InputEvent>,
    buttons: &mut Vec<InputEvent>,
    mut send: impl FnMut(Target, &[InputEvent]) -> io::Result<()>,
) -> io::Result<()> {
    keys.clear();
    buttons.clear();
    for &event in events {
        if event.event_type() == EventType::RELATIVE || is_mouse_button(KeyCode(event.code())) {
            buttons.push(event);
        } else {
            keys.push(event);
        }
    }
    if keys.is_empty() {
        return send(Target::Mouse, buttons);
    }
    // State emits releases before presses. Keep mouse releases under the old
    // modifiers and send mouse presses and wheel steps after the keyboard changes.
    let (releases, presses) = buttons.split_at(buttons.partition_point(|event| event.value() == 0));
    if !releases.is_empty() {
        send(Target::Mouse, releases)?;
    }
    send(Target::Keyboard, keys)?;
    if !presses.is_empty() {
        send(Target::Mouse, presses)?;
    }
    Ok(())
}

impl<'a> Keyboard<'a> {
    pub fn open(
        config: Option<&Config>,
        status: Option<&'a StatusSocket>,
    ) -> io::Result<Option<Self>> {
        let Some(config) = config.filter(|config| !config.is_empty()) else {
            return Ok(None);
        };
        let mut keys = AttributeSet::<KeyCode>::new();
        let mut buttons = AttributeSet::<KeyCode>::new();
        for key in config.keys() {
            if key.kind == KeyKind::MouseButton {
                buttons.insert(KeyCode(key.id));
            } else {
                keys.insert(KeyCode(key.id));
            }
        }
        let key_count = keys.iter().count();
        let button_count = buttons.iter().count();
        let scroll = config.has_scroll();
        let keyboard = if key_count > 0 {
            // Advertise the standard key range used by udev to recognize a keyboard.
            for code in KeyCode::KEY_ESC.0..=KeyCode::KEY_D.0 {
                keys.insert(KeyCode(code));
            }
            Some(
                VirtualDevice::builder()?
                    .name("JoyShockRust Virtual Keyboard")
                    .with_keys(&keys)?
                    .build()?,
            )
        } else {
            None
        };
        let mouse = (button_count > 0 || scroll || config.has_pointer())
            .then(|| {
                crate::gyro_mouse::open_output_with_buttons(
                    "JoyShockRust Button Mouse",
                    buttons,
                    scroll,
                )
            })
            .transpose()?;
        Ok(Some(Self {
            state: State::new(config),
            outputs: Outputs {
                keyboard,
                mouse,
                keys: Vec::with_capacity(key_count),
                buttons: Vec::with_capacity(button_count + config.relative_event_capacity()),
                status,
            },
        }))
    }

    pub fn publish(&mut self, report: InputReport, now: Instant) -> io::Result<Option<Instant>> {
        self.state
            .update(report.into(), now, |events| self.outputs.emit(events))
    }

    pub fn tick(&mut self, now: Instant) -> io::Result<Option<Instant>> {
        self.state
            .update(self.state.engine.last_input(), now, |events| {
                self.outputs.emit(events)
            })
    }

    pub fn release(&mut self) -> io::Result<()> {
        self.state.release(|events| self.outputs.emit(events))
    }
}

#[cfg(test)]
mod tests;
