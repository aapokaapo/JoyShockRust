use std::{
    io,
    num::NonZeroU32,
    time::{Duration, Instant},
};

use evdev::{
    AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode, uinput::VirtualDevice,
};
use joyshockrust_core::{Button, Controls, DPad};
use joyshockrust_input::{InputReport, TouchContact};
use serde::Deserialize;

use crate::gyro_gate::{self, DOUBLE_TAP_WINDOW, HOLD_TIME, TriggerGate};
use crate::status::StatusSocket;
use crate::touchpad::{Grid, TouchRegion};

mod stick;
mod touch_motion;

const TAP_KEY_TIME: Duration = Duration::from_millis(30);
const SCROLL_REPEAT_TIME: Duration = Duration::from_millis(100);
const WHEEL_AXES: [RelativeAxisCode; 2] =
    [RelativeAxisCode::REL_WHEEL, RelativeAxisCode::REL_HWHEEL];
const POINTER_AXES: [RelativeAxisCode; 2] = [RelativeAxisCode::REL_X, RelativeAxisCode::REL_Y];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub grab_touchpad: bool,
    #[serde(default)]
    bindings: Vec<RawBinding>,
    #[serde(default)]
    sticks: Vec<stick::Settings>,
    touch_scroll: Option<touch_motion::ScrollSettings>,
    touch_pointer: Option<touch_motion::PointerSettings>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBinding {
    button: Option<String>,
    trigger: Option<String>,
    chord: Option<Vec<String>>,
    tap: Option<RawOutput>,
    hold: Option<RawOutput>,
    double_tap: Option<RawOutput>,
    hold_ms: Option<NonZeroU32>,
    double_tap_ms: Option<NonZeroU32>,
    scroll_repeat_ms: Option<NonZeroU32>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawOutput {
    Single(String),
    Combination(Vec<String>),
    Scroll(ScrollOutput),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScrollOutput {
    scroll: ScrollDirection,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

impl ScrollDirection {
    fn accumulate(self, scroll: &mut [i32; WHEEL_AXES.len()]) {
        let (axis, step) = match self {
            Self::Up => (0, 1),
            Self::Down => (0, -1),
            Self::Left => (1, -1),
            Self::Right => (1, 1),
        };
        scroll[axis] = scroll[axis].saturating_add(step);
    }
}

#[derive(Clone, PartialEq)]
enum Action {
    Keys(Box<[KeyCode]>),
    Scroll(ScrollDirection),
}

impl Action {
    fn keys(&self) -> &[KeyCode] {
        match self {
            Self::Keys(keys) => keys,
            Self::Scroll(_) => &[],
        }
    }
}

#[derive(PartialEq)]
pub struct Config {
    buttons: Vec<Binding>,
    sticks: Vec<stick::Binding>,
    touch_scroll: Option<touch_motion::ScrollSettings>,
    touch_pointer: Option<touch_motion::PointerSettings>,
}

impl Config {
    fn keys(&self) -> impl Iterator<Item = KeyCode> {
        self.buttons
            .iter()
            .flat_map(Binding::actions)
            .flat_map(|action| action.keys().iter().copied())
            .chain(self.sticks.iter().flat_map(|binding| binding.keys()))
            .chain(self.has_touch_tap().then_some(KeyCode::BTN_LEFT))
    }

    fn has_touch_tap(&self) -> bool {
        self.touch_pointer
            .is_some_and(|settings| settings.tap_to_click)
    }

    fn has_scroll(&self) -> bool {
        self.touch_scroll.is_some()
            || self
                .buttons
                .iter()
                .flat_map(Binding::actions)
                .any(|action| matches!(action, Action::Scroll(_)))
    }

    fn relative_event_capacity(&self) -> usize {
        usize::from(self.has_scroll()) * WHEEL_AXES.len()
            + usize::from(self.touch_pointer.is_some()) * POINTER_AXES.len()
    }
}

fn parse_key(name: Option<String>) -> Result<Option<KeyCode>, String> {
    name.as_deref().map(keyboard_key).transpose()
}

fn keyboard_key(name: &str) -> Result<KeyCode, String> {
    if !name.starts_with("KEY_") || name == "KEY_RESERVED" {
        return Err(format!("expected a Linux keyboard key, got {name}"));
    }
    name.parse()
        .map_err(|_| format!("unknown keyboard key {name}"))
}

fn is_mouse_button(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::BTN_LEFT
            | KeyCode::BTN_RIGHT
            | KeyCode::BTN_MIDDLE
            | KeyCode::BTN_SIDE
            | KeyCode::BTN_EXTRA
    )
}

fn gesture_key(name: &str) -> Result<KeyCode, String> {
    match name.parse::<KeyCode>() {
        Ok(button) if is_mouse_button(button) => Ok(button),
        _ => keyboard_key(name),
    }
}

fn is_modifier(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::KEY_LEFTCTRL
            | KeyCode::KEY_RIGHTCTRL
            | KeyCode::KEY_LEFTSHIFT
            | KeyCode::KEY_RIGHTSHIFT
            | KeyCode::KEY_LEFTALT
            | KeyCode::KEY_RIGHTALT
            | KeyCode::KEY_LEFTMETA
            | KeyCode::KEY_RIGHTMETA
    )
}

impl RawOutput {
    fn resolve(self) -> Result<Action, String> {
        match self {
            Self::Single(name) => Ok(Action::Keys(Box::new([gesture_key(&name)?]))),
            Self::Scroll(output) => Ok(Action::Scroll(output.scroll)),
            Self::Combination(names) => {
                if names.is_empty() {
                    return Err("gesture combination needs at least one output".into());
                }
                let mut keys = names
                    .iter()
                    .map(|name| gesture_key(name))
                    .collect::<Result<Vec<_>, _>>()?;
                keys.sort_unstable_by_key(|key| key.0);
                if keys.windows(2).any(|pair| pair[0] == pair[1]) {
                    return Err("gesture combination outputs must be distinct".into());
                }
                Ok(Action::Keys(keys.into_boxed_slice()))
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Source {
    Button(Button),
    Direction(DPad),
    Trigger(TriggerGate),
    Touch(TouchRegion),
    TouchClick(TouchRegion),
}

#[derive(Clone, PartialEq)]
struct Binding {
    source: BindingSource,
    tap: Option<Action>,
    hold: Option<Action>,
    double_tap: Option<Action>,
    hold_time: Duration,
    double_tap_window: Duration,
    scroll_repeat_time: Duration,
}

impl Binding {
    fn actions(&self) -> impl Iterator<Item = &Action> {
        [&self.tap, &self.hold, &self.double_tap]
            .into_iter()
            .flatten()
    }
}

#[derive(Clone, Copy)]
enum BindingSource {
    Single(Source),
    Chord(Source, Source),
}

impl PartialEq for BindingSource {
    fn eq(&self, other: &Self) -> bool {
        match (*self, *other) {
            (Self::Single(a), Self::Single(b)) => a == b,
            (Self::Chord(a, b), Self::Chord(c, d)) => (a == c && b == d) || (a == d && b == c),
            _ => false,
        }
    }
}

impl Source {
    fn trigger(spec: &str) -> Result<Self, String> {
        TriggerGate::parse(spec)
            .map(Self::Trigger)
            .map_err(|error| format!("invalid keyboard binding trigger {spec}: {error}"))
    }

    fn chord_member(name: &str, grid: Option<&Grid>) -> Result<Self, String> {
        if let Some(spec) = name.strip_prefix("trigger:") {
            Self::trigger(spec)
        } else {
            Self::parse(name, grid)
        }
    }

    fn parse(name: &str, grid: Option<&Grid>) -> Result<Self, String> {
        if let Some((kind, spec)) = name.split_once(':') {
            let source = match kind {
                "touch" => Self::Touch,
                "click" => Self::TouchClick,
                _ => return Err(format!("unknown keyboard binding button {name}")),
            };
            let region = grid
                .ok_or("keyboard touchpad binding requires a [touchpad] grid")?
                .parse_region(spec)?;
            return Ok(source(region));
        }
        let direction = match name {
            "dpad-up" => DPad::Up,
            "dpad-right" => DPad::Right,
            "dpad-down" => DPad::Down,
            "dpad-left" => DPad::Left,
            _ => {
                return gyro_gate::button_by_name(name)
                    .map(Self::Button)
                    .ok_or_else(|| format!("unknown keyboard binding button {name}"));
            }
        };
        Ok(Self::Direction(direction))
    }

    fn pressed(self, input: &Input, click: Option<TouchContact>, was_pressed: bool) -> bool {
        let controls = &input.controls;
        match self {
            Self::Button(button) => controls.buttons.contains(button),
            Self::Trigger(trigger) => trigger.pressed(controls, was_pressed),
            Self::Touch(region) => region.pressed(&input.contacts),
            Self::TouchClick(region) => click.is_some_and(|contact| region.contains(contact)),
            Self::Direction(direction) => {
                controls.dpad == direction
                    || matches!(
                        (direction, controls.dpad),
                        (DPad::Up, DPad::UpLeft | DPad::UpRight)
                            | (DPad::Right, DPad::UpRight | DPad::DownRight)
                            | (DPad::Down, DPad::DownLeft | DPad::DownRight)
                            | (DPad::Left, DPad::UpLeft | DPad::DownLeft)
                    )
            }
        }
    }
}

impl Settings {
    pub fn resolve(self, grid: Option<&Grid>) -> Result<Config, String> {
        let mut bindings = Vec::<Binding>::new();
        for raw in self.bindings {
            let (source, name) = match (raw.button, raw.trigger, raw.chord) {
                (Some(name), None, None) => {
                    (BindingSource::Single(Source::parse(&name, grid)?), name)
                }
                (None, Some(spec), None) => (BindingSource::Single(Source::trigger(&spec)?), spec),
                (None, None, Some(members)) => {
                    let [first, second] = members.as_slice() else {
                        return Err("keyboard chord requires exactly two inputs".into());
                    };
                    let a = Source::chord_member(first, grid)?;
                    let b = Source::chord_member(second, grid)?;
                    if a == b {
                        return Err("keyboard chord requires two distinct inputs".into());
                    }
                    (
                        BindingSource::Chord(a, b),
                        format!("chord [{first}, {second}]"),
                    )
                }
                _ => {
                    return Err(
                        "keyboard binding requires exactly one of button, trigger or chord".into(),
                    );
                }
            };
            if bindings.iter().any(|binding| binding.source == source) {
                return Err(format!("duplicate keyboard binding for {name}"));
            }
            let binding = Binding {
                source,
                tap: raw.tap.map(RawOutput::resolve).transpose()?,
                hold: raw.hold.map(RawOutput::resolve).transpose()?,
                double_tap: raw.double_tap.map(RawOutput::resolve).transpose()?,
                hold_time: raw
                    .hold_ms
                    .map_or(HOLD_TIME, |ms| Duration::from_millis(ms.get().into())),
                double_tap_window: raw.double_tap_ms.map_or(DOUBLE_TAP_WINDOW, |ms| {
                    Duration::from_millis(ms.get().into())
                }),
                scroll_repeat_time: raw.scroll_repeat_ms.map_or(SCROLL_REPEAT_TIME, |ms| {
                    Duration::from_millis(ms.get().into())
                }),
            };
            if binding.actions().next().is_none() {
                return Err(format!(
                    "keyboard binding for {name} needs at least one output"
                ));
            }
            if raw.scroll_repeat_ms.is_some() && !matches!(binding.hold, Some(Action::Scroll(_))) {
                return Err(format!(
                    "keyboard binding for {name}: scroll_repeat_ms requires a scroll hold action"
                ));
            }
            bindings.push(binding);
        }
        let config = Config {
            buttons: bindings,
            sticks: stick::Settings::resolve_all(self.sticks)?,
            touch_scroll: self
                .touch_scroll
                .map(touch_motion::ScrollSettings::validate)
                .transpose()?,
            touch_pointer: self
                .touch_pointer
                .map(touch_motion::PointerSettings::validate)
                .transpose()?,
        };
        for key in config
            .sticks
            .iter()
            .filter_map(|stick| stick.pulsed_outer_key())
        {
            if config
                .keys()
                .filter(|&mapped| mapped == key)
                .nth(1)
                .is_some()
            {
                return Err(format!(
                    "keyboard stick pulse key {key:?} must have a single binding"
                ));
            }
        }
        Ok(config)
    }
}

#[derive(Default)]
enum Press {
    #[default]
    Up,
    Down(Instant),
    Held,
}

struct Gesture {
    binding: Binding,
    source_pressed: [bool; 2],
    press: Press,
    pending_tap: Option<Instant>,
    next_scroll: Option<Instant>,
}

impl Gesture {
    fn update(
        &mut self,
        input: &Input,
        click: Option<TouchContact>,
        now: Instant,
    ) -> Option<&Action> {
        let pressed = match self.binding.source {
            BindingSource::Single(source) => {
                self.source_pressed[0] = source.pressed(input, click, self.source_pressed[0]);
                self.source_pressed[0]
            }
            BindingSource::Chord(first, second) => {
                // Update both inputs even while the chord is inactive. A trigger
                // may have crossed its engagement threshold before the other input.
                self.source_pressed = [
                    first.pressed(input, click, self.source_pressed[0]),
                    second.pressed(input, click, self.source_pressed[1]),
                ];
                self.source_pressed[0] && self.source_pressed[1]
            }
        };
        let mut pulse = None;
        if self.pending_tap.is_some_and(|deadline| deadline <= now) {
            self.pending_tap = None;
            pulse = self.binding.tap.as_ref();
        }
        match self.press {
            Press::Up
                if pressed && self.binding.tap.is_none() && self.binding.double_tap.is_none() =>
            {
                self.press = Press::Held;
            }
            Press::Up if pressed => self.press = Press::Down(now),
            Press::Down(start) if !pressed => {
                self.press = Press::Up;
                if now.duration_since(start) < self.binding.hold_time {
                    if let Some(double_tap) = &self.binding.double_tap {
                        if self.pending_tap.take().is_some() {
                            pulse = Some(double_tap);
                        } else {
                            self.pending_tap = Some(now + self.binding.double_tap_window);
                        }
                    } else {
                        pulse = self.binding.tap.as_ref();
                    }
                }
            }
            Press::Down(start)
                if self.binding.hold.is_some() && now >= start + self.binding.hold_time =>
            {
                self.press = Press::Held;
            }
            Press::Held if !pressed => self.press = Press::Up,
            _ => {}
        }
        pulse
    }

    fn deadline(&self) -> Option<Instant> {
        let hold = match self.press {
            Press::Down(start) if self.binding.hold.is_some() => {
                Some(start + self.binding.hold_time)
            }
            _ => None,
        };
        hold.into_iter()
            .chain(self.pending_tap)
            .chain(self.next_scroll)
            .min()
    }

    fn held_action(&mut self, now: Instant) -> Option<&Action> {
        if !matches!(self.press, Press::Held) {
            self.next_scroll = None;
            return None;
        }
        let action = self.binding.hold.as_ref()?;
        if matches!(action, Action::Scroll(_)) {
            if self.next_scroll.is_some_and(|deadline| deadline > now) {
                return None;
            }
            // Resume from the actual wake time after a late poll, without a burst of old steps.
            self.next_scroll = Some(now + self.binding.scroll_repeat_time);
        }
        Some(action)
    }
}

#[derive(Clone, Copy, Default)]
struct Input {
    controls: Controls,
    contacts: [Option<TouchContact>; 2],
}

impl From<InputReport> for Input {
    fn from(report: InputReport) -> Self {
        Self {
            controls: report.controls,
            contacts: report.touch_contacts,
        }
    }
}

struct State {
    gestures: Vec<Gesture>,
    sticks: Vec<stick::State>,
    touch_scroll: Option<touch_motion::ScrollState>,
    touch_pointer: Option<touch_motion::PointerState>,
    touch_tap: Option<touch_motion::TapState>,
    pulses: Vec<(KeyCode, Instant)>,
    mapped_keys: AttributeSet<KeyCode>,
    outer_keys: AttributeSet<KeyCode>,
    pulsed_outer_keys: AttributeSet<KeyCode>,
    held: AttributeSet<KeyCode>,
    next_held: AttributeSet<KeyCode>,
    events: Vec<InputEvent>,
    input: Input,
    click: Option<TouchContact>,
}

impl State {
    fn new(config: &Config) -> Self {
        let gestures = config
            .buttons
            .iter()
            .cloned()
            .map(|binding| Gesture {
                binding,
                source_pressed: [false; 2],
                press: Press::Up,
                pending_tap: None,
                next_scroll: None,
            })
            .collect();
        let mapped_keys: AttributeSet<KeyCode> = config.keys().collect();
        let key_count = mapped_keys.iter().count();
        Self {
            gestures,
            sticks: config
                .sticks
                .iter()
                .copied()
                .map(stick::State::new)
                .collect(),
            touch_scroll: config.touch_scroll.map(touch_motion::ScrollState::new),
            touch_pointer: config.touch_pointer.map(touch_motion::PointerState::new),
            touch_tap: config.has_touch_tap().then(touch_motion::TapState::default),
            // At most one pulse deadline per mapped key.
            pulses: Vec::with_capacity(key_count),
            mapped_keys,
            outer_keys: config
                .sticks
                .iter()
                .filter_map(|binding| binding.held_outer_key())
                .collect(),
            pulsed_outer_keys: config
                .sticks
                .iter()
                .filter_map(|binding| binding.pulsed_outer_key())
                .collect(),
            held: AttributeSet::new(),
            next_held: AttributeSet::new(),
            events: Vec::with_capacity(key_count + config.relative_event_capacity()),
            input: Input::default(),
            click: None,
        }
    }

    fn update(
        &mut self,
        input: Input,
        now: Instant,
        mut emit: impl FnMut(&[InputEvent]) -> io::Result<()>,
    ) -> io::Result<Option<Instant>> {
        if !input.controls.buttons.contains(Button::TouchpadClick) {
            self.click = None;
        } else if !self.input.controls.buttons.contains(Button::TouchpadClick) {
            // Match gamepad mapping: latch the first active contact at the click edge.
            self.click = input.contacts.into_iter().flatten().next();
        }
        self.input = input;
        // Reuse the bitsets. evdev's key bitset allocates on construction.
        let held = &mut self.next_held;
        for key in self.mapped_keys.iter() {
            held.remove(key);
        }
        let mut deadline = None;
        let mut scroll = self
            .touch_scroll
            .as_mut()
            .map_or([0; WHEEL_AXES.len()], |scroll| {
                scroll.advance(input.contacts)
            });
        self.pulses.retain(|&(_, until)| until > now);
        if self.touch_tap.as_mut().is_some_and(|tap| {
            tap.advance(
                input.contacts,
                input.controls.buttons.contains(Button::TouchpadClick),
                now,
            )
        }) {
            schedule_pulse(&mut self.pulses, KeyCode::BTN_LEFT, now + TAP_KEY_TIME);
        }
        for gesture in &mut self.gestures {
            match gesture.update(&input, self.click, now) {
                Some(Action::Keys(keys)) => {
                    let until = now + TAP_KEY_TIME;
                    for &key in keys {
                        schedule_pulse(&mut self.pulses, key, until);
                    }
                }
                Some(Action::Scroll(direction)) => direction.accumulate(&mut scroll),
                None => {}
            }
            match gesture.held_action(now) {
                Some(Action::Keys(keys)) => {
                    for &key in keys {
                        held.insert(key);
                    }
                }
                Some(Action::Scroll(direction)) => direction.accumulate(&mut scroll),
                None => {}
            }
            deadline = deadline.into_iter().chain(gesture.deadline()).min();
        }
        for &(key, until) in &self.pulses {
            held.insert(key);
            deadline = deadline.into_iter().chain(Some(until)).min();
        }
        for stick in &mut self.sticks {
            deadline = deadline
                .into_iter()
                .chain(stick.update(input.controls, now, held))
                .min();
        }
        self.events.clear();
        self.events.extend(
            self.held
                .iter()
                .filter(|&key| !held.contains(key))
                .map(|key| key_event(key, 0)),
        );
        sort_releases(&mut self.events, &self.outer_keys);
        let releases = self.events.len();
        self.events.extend(
            held.iter()
                .filter(|&key| !self.held.contains(key))
                .map(|key| key_event(key, 1)),
        );
        // Shortcut modifiers precede other keys. Sprint pulses follow movement.
        self.events[releases..].sort_unstable_by_key(|event| {
            let key = KeyCode(event.code());
            let order = if self.outer_keys.contains(key) {
                0
            } else if self.pulsed_outer_keys.contains(key) {
                3
            } else if is_modifier(key) {
                1
            } else {
                2
            };
            (order, key.0)
        });
        let movement = self
            .touch_pointer
            .as_mut()
            .map_or([0; POINTER_AXES.len()], |pointer| {
                pointer.advance(input.contacts)
            });
        for (axis, delta) in WHEEL_AXES
            .into_iter()
            .zip(scroll)
            .chain(POINTER_AXES.into_iter().zip(movement))
        {
            if delta != 0 {
                self.events
                    .push(InputEvent::new(EventType::RELATIVE.0, axis.0, delta));
            }
        }
        if !self.events.is_empty() {
            emit(&self.events)?;
        }
        std::mem::swap(&mut self.held, &mut self.next_held);
        Ok(deadline)
    }
}

fn key_event(key: KeyCode, value: i32) -> InputEvent {
    InputEvent::new(EventType::KEY.0, key.0, value)
}

fn schedule_pulse(pulses: &mut Vec<(KeyCode, Instant)>, key: KeyCode, until: Instant) {
    if let Some(pulse) = pulses.iter_mut().find(|pulse| pulse.0 == key) {
        pulse.1 = until;
    } else {
        pulses.push((key, until));
    }
}

fn sort_releases(events: &mut [InputEvent], outer_keys: &AttributeSet<KeyCode>) {
    events.sort_unstable_by_key(|event| {
        let key = KeyCode(event.code());
        (outer_keys.contains(key), is_modifier(key), key.0)
    });
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
        let Some(config) = config.filter(|config| {
            !config.buttons.is_empty()
                || !config.sticks.is_empty()
                || config.touch_scroll.is_some()
                || config.touch_pointer.is_some()
        }) else {
            return Ok(None);
        };
        let mut keys = AttributeSet::<KeyCode>::new();
        let mut buttons = AttributeSet::<KeyCode>::new();
        for key in config.keys() {
            if is_mouse_button(key) {
                buttons.insert(key);
            } else {
                keys.insert(key);
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
        let mouse = (button_count > 0 || scroll || config.touch_pointer.is_some())
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
            .update(self.state.input, now, |events| self.outputs.emit(events))
    }

    pub fn release(&mut self) -> io::Result<()> {
        let events = &mut self.state.events;
        events.clear();
        events.extend(self.state.held.iter().map(|key| key_event(key, 0)));
        sort_releases(events, &self.state.outer_keys);
        if !events.is_empty() {
            self.outputs.emit(events)?;
        }
        self.state.held = AttributeSet::new();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn config(bindings: &str) -> Config {
        crate::profile::parse(&format!("version = 1\n{bindings}"))
            .unwrap()
            .keyboard
            .unwrap()
    }

    fn input(controls: Controls) -> Input {
        Input {
            controls,
            ..Input::default()
        }
    }

    #[test]
    #[ignore = "requires writable /dev/uinput and readable virtual evdev nodes"]
    fn uinput_keyboard_and_mouse_gestures() {
        use crate::live_uinput_test::open_evdev;
        use joyshockrust_input::{DeviceModel, Transport, parse_report};

        let config = config(
            r#"
            [[keyboard.bindings]]
            button = "l1"
            hold = "KEY_F13"
            [[keyboard.bindings]]
            button = "cross"
            tap = "BTN_LEFT"
            double_tap = "BTN_RIGHT"
            double_tap_ms = 100
        "#,
        );
        let mut output = Keyboard::open(Some(&config), None).unwrap().unwrap();
        let keys = open_evdev(
            &output
                .outputs
                .keyboard
                .as_mut()
                .unwrap()
                .get_syspath()
                .unwrap(),
        );
        let mouse = open_evdev(
            &output
                .outputs
                .mouse
                .as_mut()
                .unwrap()
                .get_syspath()
                .unwrap(),
        );
        assert!(!keys.supported_keys().unwrap().contains(KeyCode::BTN_LEFT));
        assert!(!mouse.supported_keys().unwrap().contains(KeyCode::KEY_F13));

        let mut frame = [0; 64];
        frame[0] = 1;
        frame[1..=4].fill(128);
        frame[9] = 1; // L1 remains held for the entire sequence.
        frame[33] = 0x80;
        frame[37] = 0x80;
        let mut report = |cross| {
            frame[8] = if cross { 0x28 } else { 8 };
            parse_report(DeviceModel::DualSenseEdge, Transport::Usb, &frame, None).unwrap()
        };
        let start = Instant::now();
        assert_eq!(output.publish(report(true), start).unwrap(), None);
        assert!(keys.get_key_state().unwrap().contains(KeyCode::KEY_F13));
        assert!(!mouse.get_key_state().unwrap().contains(KeyCode::BTN_LEFT));

        let released = start + Duration::from_millis(10);
        let click = released + Duration::from_millis(100);
        assert_eq!(
            output.publish(report(false), released).unwrap(),
            Some(click)
        );
        assert_eq!(
            output.tick(click - Duration::from_millis(1)).unwrap(),
            Some(click)
        );
        assert!(!mouse.get_key_state().unwrap().contains(KeyCode::BTN_LEFT));

        let click_up = click + TAP_KEY_TIME;
        assert_eq!(output.tick(click).unwrap(), Some(click_up));
        let mouse_state = mouse.get_key_state().unwrap();
        assert!(mouse_state.contains(KeyCode::BTN_LEFT));
        assert!(!mouse_state.contains(KeyCode::BTN_RIGHT));
        assert_eq!(output.tick(click_up).unwrap(), None);
        assert!(!mouse.get_key_state().unwrap().contains(KeyCode::BTN_LEFT));
        assert!(keys.get_key_state().unwrap().contains(KeyCode::KEY_F13));

        output.release().unwrap();
        assert!(!keys.get_key_state().unwrap().contains(KeyCode::KEY_F13));
    }

    #[test]
    fn tap_hold_and_double_tap_emit_distinct_keys() {
        let config = config(
            r#"
            [[keyboard.bindings]]
            button = "cross"
            tap = "KEY_SPACE"
            hold = "KEY_LEFTSHIFT"
            double_tap = "KEY_R"
        "#,
        );
        let mut state = State::new(&config);
        let start = Instant::now();
        let mut events = Vec::with_capacity(8);
        let allocations = allocation_counter::measure(|| {
            for (ms, pressed, next_deadline) in [
                (0, true, Some(200)),
                (60, false, Some(360)),
                (359, false, Some(360)),
                (360, false, Some(390)), // Single tap after the double-tap window.
                (390, false, None),
                (500, true, Some(700)),
                (550, false, Some(850)),
                (590, true, Some(790)),
                (640, false, Some(670)), // Second short release.
                (670, false, None),
                (800, true, Some(1000)),
                (1000, true, None), // Idle timer, no new input state.
                (1500, false, None),
            ] {
                let mut controls = Controls::default();
                if pressed {
                    controls.buttons.insert(Button::South);
                }
                let deadline = state
                    .update(
                        input(controls),
                        start + Duration::from_millis(ms),
                        |batch| {
                            events.extend(batch.iter().map(|event| (event.code(), event.value())));
                            Ok(())
                        },
                    )
                    .unwrap();
                assert_eq!(
                    deadline,
                    next_deadline.map(|ms| start + Duration::from_millis(ms))
                );
            }
        });
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
        assert_eq!(
            events,
            [
                (KeyCode::KEY_SPACE.0, 1),
                (KeyCode::KEY_SPACE.0, 0),
                (KeyCode::KEY_R.0, 1),
                (KeyCode::KEY_R.0, 0),
                (KeyCode::KEY_LEFTSHIFT.0, 1),
                (KeyCode::KEY_LEFTSHIFT.0, 0),
            ]
        );
    }

    #[test]
    fn hold_only_dpad_bindings_are_immediate_and_share_key_state() {
        let config = config(
            r#"
            [[keyboard.bindings]]
            button = "dpad-up"
            hold = "KEY_W"
            [[keyboard.bindings]]
            button = "dpad-right"
            hold = "KEY_D"
            [[keyboard.bindings]]
            button = "r1"
            hold = "KEY_W"
        "#,
        );
        let mut state = State::new(&config);
        let start = Instant::now();
        let mut events = Vec::new();
        let mut controls = Controls {
            dpad: DPad::UpRight,
            ..Controls::default()
        };
        for (ms, direction, shoulder) in [
            (0, DPad::UpRight, false),
            (1, DPad::Right, true),
            (2, DPad::Neutral, true),
            (3, DPad::Neutral, false),
        ] {
            controls.dpad = direction;
            if shoulder {
                controls.buttons.insert(Button::RightShoulder);
            } else {
                controls.buttons.remove(Button::RightShoulder);
            }
            assert!(
                state
                    .update(
                        input(controls),
                        start + Duration::from_millis(ms),
                        |batch| {
                            events.extend(batch.iter().map(|event| (event.code(), event.value())));
                            Ok(())
                        }
                    )
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(
            events,
            [
                (KeyCode::KEY_W.0, 1),
                (KeyCode::KEY_D.0, 1),
                (KeyCode::KEY_D.0, 0),
                (KeyCode::KEY_W.0, 0)
            ]
        );
    }

    #[test]
    fn analog_stick_walks_runs_and_releases_keys_in_order() {
        let config = config(
            r#"
            [[keyboard.sticks]]
            side = "left"
            deadzone = 0.2
            up = "KEY_W"
            right = "KEY_D"
            down = "KEY_S"
            left = "KEY_A"
            outer = { key = "KEY_LEFTSHIFT", threshold = 0.8, release = 0.7 }
        "#,
        );
        let mut state = State::new(&config);
        let now = Instant::now();
        let mut events = Vec::with_capacity(2);
        let w = KeyCode::KEY_W.0;
        let d = KeyCode::KEY_D.0;
        let shift = KeyCode::KEY_LEFTSHIFT.0;
        let frames = [
            (0.0, -0.19, &[][..]),
            (0.0, -0.40, &[(w, 1)][..]),
            (0.30, -0.30, &[(d, 1)][..]),
            (0.01, -1.0, &[(d, 0), (shift, 1)][..]),
            (0.0, -0.75, &[][..]),
            (0.0, -0.69, &[(shift, 0)][..]),
            (0.0, 0.0, &[(w, 0)][..]),
            (0.0, -1.0, &[(shift, 1), (w, 1)][..]),
            (0.0, 0.0, &[(w, 0), (shift, 0)][..]),
        ];
        let allocations = allocation_counter::measure(|| {
            for (x, y, expected) in frames {
                events.clear();
                let controls = Controls {
                    left_stick: joyshockrust_core::Stick { x, y },
                    ..Controls::default()
                };
                let deadline = state
                    .update(input(controls), now, |batch| {
                        events.extend(batch.iter().map(|event| (event.code(), event.value())));
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(deadline, None);
                assert_eq!(events, expected);
            }
        });
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }

    #[test]
    fn example_stick_mapping_holds_both_keys_on_every_diagonal() {
        let profile =
            crate::profile::parse(include_str!("../../../profiles/gyro-keyboard.toml")).unwrap();
        let mut state = State::new(profile.keyboard.as_ref().unwrap());
        let now = Instant::now();
        let directions = [
            KeyCode::KEY_W,
            KeyCode::KEY_A,
            KeyCode::KEY_S,
            KeyCode::KEY_D,
        ];
        for (x, y, running, expected) in [
            (-0.3, -0.3, false, [true, true, false, false]),
            (-0.8, -0.8, true, [true, true, false, false]),
            (0.3, -0.3, false, [true, false, false, true]),
            (-0.3, 0.3, false, [false, true, true, false]),
            (0.3, 0.3, false, [false, false, true, true]),
            (0.0, 0.0, false, [false, false, false, false]),
        ] {
            let controls = Controls {
                left_stick: joyshockrust_core::Stick { x, y },
                ..Controls::default()
            };
            state.update(input(controls), now, |_| Ok(())).unwrap();
            assert_eq!(directions.map(|key| state.held.contains(key)), expected);
            assert_eq!(state.held.contains(KeyCode::KEY_LEFTSHIFT), running);
        }
    }

    #[test]
    fn direction_hysteresis_uses_separate_diagonal_boundaries_and_resets_at_center() {
        let config = config(
            r#"
            [[keyboard.sticks]]
            side = "left"
            up = "KEY_W"
            left = "KEY_A"
            down = "KEY_S"
            right = "KEY_D"
            direction_hysteresis_deg = 5.0
        "#,
        );
        let mut state = State::new(&config);
        let now = Instant::now();
        let w = KeyCode::KEY_W;
        let a = KeyCode::KEY_A;
        let s = KeyCode::KEY_S;
        let d = KeyCode::KEY_D;
        for (x, y, expected) in [
            (-0.33, -0.8, &[w][..]),
            (-0.5, -0.8, &[w, a][..]),
            (-0.33, -0.8, &[w, a][..]),
            (0.33, -0.8, &[w][..]), // A releases and D still needs to engage.
            (0.5, -0.8, &[w, d][..]),
            (0.33, -0.8, &[w, d][..]),
            (0.24, -0.8, &[w][..]),
            (0.8, -0.5, &[w, d][..]),
            (0.8, -0.33, &[w, d][..]),
            (0.8, -0.24, &[d][..]),
            (0.8, 0.5, &[s, d][..]),
            (0.0, 0.0, &[][..]),
            (0.8, 0.33, &[d][..]), // The earlier S hold cannot survive centering.
        ] {
            let controls = Controls {
                left_stick: joyshockrust_core::Stick { x, y },
                ..Controls::default()
            };
            assert_eq!(
                state.update(input(controls), now, |_| Ok(())).unwrap(),
                None
            );
            assert!(
                state.held.iter().eq(expected.iter().copied()),
                "({x}, {y}): expected {expected:?}"
            );
        }
    }

    #[test]
    fn toggle_outer_zone_pulses_shift_while_preserving_diagonal_movement() {
        let profile = include_str!("../../../profiles/gyro-keyboard.toml")
            .replace("hold = \"KEY_LEFTSHIFT\"", "hold = \"KEY_LEFTCTRL\"")
            .replace("threshold = 0.8", "mode = \"toggle\", threshold = 0.8");
        let config = crate::profile::parse(&profile).unwrap().keyboard.unwrap();
        let mut state = State::new(&config);
        let start = Instant::now();
        let mut events = Vec::with_capacity(3);
        let w = KeyCode::KEY_W.0;
        let a = KeyCode::KEY_A.0;
        let shift = KeyCode::KEY_LEFTSHIFT.0;
        let frames = [
            (0, -0.3, &[(w, 1), (a, 1)][..], None),
            (10, -0.8, &[(shift, 1)][..], Some(40)),
            (40, -0.8, &[(shift, 0)][..], None),
            (80, -0.52, &[][..], None), // Between the two thresholds.
            (90, -0.3, &[(shift, 1)][..], Some(120)),
            (95, -0.8, &[][..], Some(120)), // Re-enter during the exit pulse.
            (120, -0.8, &[(shift, 0)][..], Some(150)),
            (150, -0.8, &[(shift, 1)][..], Some(180)),
            (180, -0.8, &[(shift, 0)][..], None),
            (210, 0.0, &[(w, 0), (a, 0), (shift, 1)][..], Some(240)),
            (240, 0.0, &[(shift, 0)][..], None),
        ];
        let allocations = allocation_counter::measure(|| {
            for (ms, axis, expected, next) in frames {
                events.clear();
                let controls = Controls {
                    left_stick: joyshockrust_core::Stick { x: axis, y: axis },
                    ..Controls::default()
                };
                let deadline = state
                    .update(
                        input(controls),
                        start + Duration::from_millis(ms),
                        |batch| {
                            events.extend(batch.iter().map(|event| (event.code(), event.value())));
                            Ok(())
                        },
                    )
                    .unwrap();
                assert_eq!(events, expected, "{ms} ms");
                assert_eq!(deadline, next.map(|ms| start + Duration::from_millis(ms)));
            }
        });
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }

    #[test]
    fn keep_sprint_pulses_once_per_continuous_movement() {
        let config = config(
            r#"
            [[keyboard.sticks]]
            side = "left"
            deadzone = 0.3
            deadzone_release = 0.2
            up = "KEY_W"
            left = "KEY_A"
            right = "KEY_D"
            outer = { key = "KEY_LEFTSHIFT", mode = "keep_sprint", threshold = 0.8 }
            "#,
        );
        let mut state = State::new(&config);
        let start = Instant::now();
        let w = KeyCode::KEY_W.0;
        let a = KeyCode::KEY_A.0;
        let d = KeyCode::KEY_D.0;
        let shift = KeyCode::KEY_LEFTSHIFT.0;
        let frames = [
            (0, -0.18, -0.18, &[][..], None), // Below the activation radius.
            (1, -0.8, -0.8, &[(w, 1), (a, 1), (shift, 1)][..], Some(31)),
            (31, -0.8, -0.8, &[(shift, 0)][..], None),
            (60, -0.18, -0.18, &[][..], None), // Both direction keys survive between thresholds.
            (70, 0.4, 0.0, &[(w, 0), (a, 0), (d, 1)][..], None),
            (80, 1.0, 0.0, &[][..], None), // Re-entering the zone while moving.
            (90, 0.2, 0.0, &[(d, 0)][..], None), // The release radius ends movement.
            (95, 0.25, 0.0, &[][..], None), // Noise within the band keeps movement released.
            (100, 0.0, -0.4, &[(w, 1)][..], None),
            (110, 0.0, -1.0, &[(shift, 1)][..], Some(140)),
            (115, 0.0, 0.0, &[(w, 0)][..], Some(140)),
            (120, 0.0, -1.0, &[(w, 1)][..], Some(140)),
            (140, 0.0, -1.0, &[(shift, 0)][..], Some(170)),
            (170, 0.0, -1.0, &[(shift, 1)][..], Some(200)),
            (200, 0.0, -1.0, &[(shift, 0)][..], None),
            (210, 0.0, 0.0, &[(w, 0)][..], None),
        ];
        let allocations = allocation_counter::measure(|| {
            for (ms, x, y, expected, next) in frames {
                let controls = Controls {
                    left_stick: joyshockrust_core::Stick { x, y },
                    ..Controls::default()
                };
                let mut batches = 0;
                let deadline = state
                    .update(
                        input(controls),
                        start + Duration::from_millis(ms),
                        |batch| {
                            batches += 1;
                            assert!(
                                batch
                                    .iter()
                                    .map(|event| (event.code(), event.value()))
                                    .eq(expected.iter().copied()),
                                "{ms} ms: {batch:?}"
                            );
                            Ok(())
                        },
                    )
                    .unwrap();
                assert_eq!(batches, usize::from(!expected.is_empty()));
                assert_eq!(deadline, next.map(|ms| start + Duration::from_millis(ms)));
            }
        });
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }

    #[test]
    fn right_stick_and_button_can_share_an_outer_key() {
        let config = config(
            r#"
            [[keyboard.sticks]]
            side = "right"
            up = "KEY_W"
            outer = { key = "KEY_LEFTSHIFT", threshold = 0.5 }
            [[keyboard.bindings]]
            button = "r1"
            hold = "KEY_LEFTSHIFT"
        "#,
        );
        let now = Instant::now();
        let mut state = State::new(&config);
        let mut controls = Controls::default();
        let mut events = Vec::new();
        let w = KeyCode::KEY_W.0;
        let shift = KeyCode::KEY_LEFTSHIFT.0;
        controls.left_stick.y = -1.0;
        for (y, r1, expected) in [
            (0.0, false, &[][..]),
            (-0.7, false, &[(shift, 1), (w, 1)][..]),
            (-0.3, true, &[][..]),
            (0.0, true, &[(w, 0)][..]),
            (0.0, false, &[(shift, 0)][..]),
        ] {
            controls.right_stick.y = y;
            if r1 {
                controls.buttons.insert(Button::RightShoulder);
            } else {
                controls.buttons.remove(Button::RightShoulder);
            }
            events.clear();
            state
                .update(input(controls), now, |batch| {
                    events.extend(batch.iter().map(|event| (event.code(), event.value())));
                    Ok(())
                })
                .unwrap();
            assert_eq!(events, expected);
        }
    }

    #[test]
    fn keyboard_sticks_validate_zone_limits_and_duplicate_sides() {
        let binding = "[[keyboard.sticks]]\nside = 'right'\nup = 'KEY_W'\nouter = { key = 'KEY_LEFTSHIFT', threshold = 0.8, release = 0.7 }\n";
        let valid = config(binding);
        assert_eq!(valid.sticks.len(), 1);
        for invalid in [
            binding.replace("'right'", "'middle'"),
            binding.replace("threshold = 0.8", "threshold = 1.1"),
            binding.replace("release = 0.7", "release = 0.1"),
            binding.replace("release = 0.7", "release = 0.8"),
            binding.replace("threshold = 0.8", "mode = 'tap', threshold = 0.8"),
            binding
                .replace("up = 'KEY_W'\n", "")
                .replace("threshold = 0.8", "mode = 'keep_sprint', threshold = 0.8"),
            format!(
                "{}\n[[keyboard.bindings]]\nbutton = 'r1'\nhold = 'KEY_LEFTSHIFT'\n",
                binding.replace("threshold = 0.8", "mode = 'toggle', threshold = 0.8")
            ),
            format!("{binding}deadzone = nan\n"),
            format!("{binding}deadzone_release = 0.3\n"),
            format!("{binding}deadzone_release = -0.1\n"),
            format!("{binding}deadzone_release = nan\n"),
            format!("{binding}direction_hysteresis_deg = -0.1\n"),
            format!("{binding}direction_hysteresis_deg = 22.5\n"),
            format!("{binding}direction_hysteresis_deg = nan\n"),
            format!("{binding}{binding}"),
        ] {
            assert!(
                crate::profile::parse(&format!("version = 1\n{invalid}")).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn keyboard_profiles_reject_unknown_keys_and_duplicate_inputs() {
        let binding = "[[keyboard.bindings]]\nbutton = 'cross'\nhold = 'KEY_A'\n";
        for invalid in [
            binding.replace("KEY_A", "BTN_SOUTH"),
            binding.replace("KEY_A", "KEY_DOES_NOT_EXIST"),
            binding.replace("cross", "unknown"),
            format!("{binding}{binding}"),
        ] {
            assert!(crate::profile::parse(&format!("version = 1\n{invalid}")).is_err());
        }
        let profile = crate::profile::parse(&format!(
            "version = 1\n[bridge]\nvirtual_gamepad = false\n{binding}"
        ))
        .unwrap();
        assert!(!profile.bridge.unwrap().virtual_gamepad);
    }

    #[test]
    fn analog_trigger_bindings_keep_independent_thresholds_and_ignore_the_button_bit() {
        let config = config(
            r#"
            [[keyboard.bindings]]
            trigger = "l2:0.35:0.25"
            hold = "KEY_Q"
            [[keyboard.bindings]]
            trigger = "l2:0.8:0.65"
            hold = "KEY_E"
            [[keyboard.bindings]]
            button = "l2"
            hold = "KEY_F"
            "#,
        );
        let mut state = State::new(&config);
        let now = Instant::now();
        let mut controls = Controls {
            right_trigger: 1.0,
            ..Controls::default()
        };
        controls.buttons.insert(Button::LeftTrigger);
        for (travel, expected) in [
            (0.25, [false, false]),
            (0.35, [true, false]),
            (0.8, [true, true]),
            (0.7, [true, true]),
            (0.65, [true, false]),
            (0.3, [true, false]),
            (0.25, [false, false]),
        ] {
            controls.left_trigger = travel;
            assert_eq!(
                state.update(input(controls), now, |_| Ok(())).unwrap(),
                None
            );
            assert_eq!(
                [KeyCode::KEY_Q, KeyCode::KEY_E].map(|key| state.held.contains(key)),
                expected
            );
            assert!(state.held.contains(KeyCode::KEY_F));
        }
    }

    #[test]
    fn keyboard_profiles_validate_analog_sources() {
        let binding = "[[keyboard.bindings]]\ntrigger = 'r2'\ntap = 'KEY_R'\n";
        for invalid in [
            format!("{binding}button = 'r2'\n"),
            binding.replace("trigger = 'r2'\n", ""),
            binding.replace("r2", "r2:0.6:0.7"),
            binding.replace("r2", "l1"),
            format!("{binding}{}", binding.replace("'r2'", "'r2:0.45:0.35'")),
        ] {
            assert!(crate::profile::parse(&format!("version = 1\n{invalid}")).is_err());
        }
    }

    #[test]
    fn mouse_button_gestures_share_the_keyboard_deadline_path() {
        let config = config(
            r#"
            [[keyboard.bindings]]
            trigger = "r2:0.6:0.4"
            hold = "BTN_LEFT"
            [[keyboard.bindings]]
            button = "cross"
            tap = "BTN_RIGHT"
            [[keyboard.bindings]]
            button = "l1"
            hold = "KEY_W"
            "#,
        );
        let mut state = State::new(&config);
        let start = Instant::now();
        let left = KeyCode::BTN_LEFT.0;
        let right = KeyCode::BTN_RIGHT.0;
        let w = KeyCode::KEY_W.0;
        let mut controls = Controls::default();
        controls.buttons.insert(Button::LeftShoulder);
        for (ms, trigger, cross, events, deadline) in [
            (0, 0.6, true, &[(w, 1), (left, 1)][..], None),
            (20, 0.5, false, &[(right, 1)][..], Some(50)),
            (30, 0.4, false, &[(left, 0)][..], Some(50)),
            (50, 0.4, false, &[(right, 0)][..], None),
        ] {
            controls.right_trigger = trigger;
            if cross {
                controls.buttons.insert(Button::South);
            } else {
                controls.buttons.remove(Button::South);
            }
            let mut actual = Vec::new();
            let next = state
                .update(
                    input(controls),
                    start + Duration::from_millis(ms),
                    |batch| {
                        actual.extend(batch.iter().map(|event| (event.code(), event.value())));
                        Ok(())
                    },
                )
                .unwrap();
            assert_eq!(actual, events);
            assert_eq!(next, deadline.map(|ms| start + Duration::from_millis(ms)));
        }
    }

    #[test]
    fn touchpad_gestures_track_contacts_and_latch_the_initial_click_region() {
        use joyshockrust_input::{DeviceModel, Transport, parse_report};

        let config = config(
            r#"
            [touchpad]
            rows = 1
            columns = 2
            [[keyboard.bindings]]
            button = "touch:1:1"
            hold = "KEY_Q"
            [[keyboard.bindings]]
            button = "click:1:1"
            hold = "KEY_E"
            [[keyboard.bindings]]
            button = "click:1:2"
            hold = "BTN_LEFT"
        "#,
        );
        let mut state = State::new(&config);
        let mut frame = [0; 64];
        frame[0] = 1;
        frame[1..=4].fill(128);
        frame[8] = 8;
        let inactive = [0x80, 0, 0, 0];
        let left = [1, 0xbf, 0x03, 0]; // x = 959
        let right = [2, 0xc0, 0x03, 0]; // x = 960
        let now = Instant::now();
        for (first, second, click, expected) in [
            (inactive, inactive, true, [false, false, false]),
            (left, inactive, true, [true, false, false]),
            (left, inactive, false, [true, false, false]),
            (right, left, true, [true, false, true]),
            (left, inactive, true, [true, false, true]),
            (inactive, inactive, true, [false, false, true]),
            (inactive, inactive, false, [false, false, false]),
            (inactive, left, true, [true, true, false]),
        ] {
            frame[33..37].copy_from_slice(&first);
            frame[37..41].copy_from_slice(&second);
            frame[10] = if click { 2 } else { 0 };
            let report =
                parse_report(DeviceModel::DualSenseEdge, Transport::Usb, &frame, None).unwrap();
            state.update(report.into(), now, |_| Ok(())).unwrap();
            assert_eq!(
                [KeyCode::KEY_Q, KeyCode::KEY_E, KeyCode::BTN_LEFT]
                    .map(|key| state.held.contains(key)),
                expected
            );
        }
    }

    #[test]
    fn touchpad_gestures_require_valid_grid_cells() {
        let binding = "[[keyboard.bindings]]\nbutton = 'touch:1:2'\nhold = 'KEY_Q'\n";
        assert!(crate::profile::parse(&format!("version = 1\n{binding}")).is_err());
        let grid = "[touchpad]\nrows = 1\ncolumns = 2\n";
        for invalid in [
            binding.replace("touch:1:2", "click:2:2"),
            binding.replace("touch:1:2", "touch:0:1"),
            binding.replace("touch:1:2", "click:1:2:3"),
            format!("{binding}{binding}"),
        ] {
            assert!(crate::profile::parse(&format!("version = 1\n{grid}{invalid}")).is_err());
        }
    }

    #[test]
    fn chord_holds_only_while_both_members_are_active_in_either_order() {
        let config = config(
            r#"
            [[keyboard.bindings]]
            chord = ["l1", "trigger:r2:0.6:0.4"]
            hold = "BTN_RIGHT"
            [[keyboard.bindings]]
            button = "l1"
            hold = "KEY_Q"
        "#,
        );
        let mut state = State::new(&config);
        let now = Instant::now();
        for (l1, trigger, right_mouse) in [
            (false, 0.6, false),
            (false, 0.5, false),
            (true, 0.5, true),
            (false, 0.5, false),
            (false, 0.4, false),
            (true, 0.5, false),
            (true, 0.6, true),
            (true, 0.4, false),
        ] {
            let mut controls = Controls {
                right_trigger: trigger,
                ..Controls::default()
            };
            if l1 {
                controls.buttons.insert(Button::LeftShoulder);
            }
            assert_eq!(
                state.update(input(controls), now, |_| Ok(())).unwrap(),
                None
            );
            assert_eq!(state.held.contains(KeyCode::KEY_Q), l1);
            assert_eq!(state.held.contains(KeyCode::BTN_RIGHT), right_mouse);
        }
    }

    #[test]
    fn validates_chord_members_and_rejects_reversed_duplicates() {
        let binding = "[[keyboard.bindings]]\nchord = ['l1', 'r1']\ntap = 'KEY_TAB'\n";
        for invalid in [
            binding.replace("'l1', 'r1'", "'l1', 'l1'"),
            binding.replace("'l1', 'r1'", "'l1'"),
            binding.replace("'l1', 'r1'", "'l1', 'r1', 'cross'"),
            binding.replace("'l1'", "'touch:1:1'"),
            binding.replace("'l1'", "'trigger:l2:0.4:0.6'"),
            format!("{binding}button = 'l1'\n"),
            format!("{binding}{}", binding.replace("'l1', 'r1'", "'r1', 'l1'")),
        ] {
            assert!(
                crate::profile::parse(&format!("version = 1\n{invalid}")).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn keyboard_shortcuts_order_modifiers_and_preserve_shared_keys() {
        let config = config(
            r#"
            [[keyboard.bindings]]
            button = "cross"
            tap = ["KEY_TAB", "KEY_LEFTCTRL", "KEY_LEFTSHIFT"]
            [[keyboard.bindings]]
            button = "l1"
            hold = "KEY_LEFTCTRL"
        "#,
        );
        let mut state = State::new(&config);
        let start = Instant::now();
        let ctrl = KeyCode::KEY_LEFTCTRL.0;
        let shift = KeyCode::KEY_LEFTSHIFT.0;
        let tab = KeyCode::KEY_TAB.0;
        let mut events = Vec::with_capacity(3);
        let allocations = allocation_counter::measure(|| {
            for (ms, cross, shoulder, expected, end) in [
                (0, true, false, &[][..], None),
                (
                    10,
                    false,
                    false,
                    &[(ctrl, 1), (shift, 1), (tab, 1)][..],
                    Some(40),
                ),
                (
                    40,
                    false,
                    false,
                    &[(tab, 0), (ctrl, 0), (shift, 0)][..],
                    None,
                ),
                (50, true, true, &[(ctrl, 1)][..], None),
                (60, false, true, &[(shift, 1), (tab, 1)][..], Some(90)),
                (90, false, true, &[(tab, 0), (shift, 0)][..], None),
                (100, false, false, &[(ctrl, 0)][..], None),
            ] {
                let mut controls = Controls::default();
                if cross {
                    controls.buttons.insert(Button::South);
                }
                if shoulder {
                    controls.buttons.insert(Button::LeftShoulder);
                }
                events.clear();
                let deadline = state
                    .update(
                        input(controls),
                        start + Duration::from_millis(ms),
                        |batch| {
                            events.extend(batch.iter().map(|event| (event.code(), event.value())));
                            Ok(())
                        },
                    )
                    .unwrap();
                assert_eq!(events, expected);
                assert_eq!(deadline, end.map(|ms| start + Duration::from_millis(ms)));
            }
        });
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }

    #[test]
    fn gesture_combinations_reject_empty_duplicate_and_unsupported_outputs() {
        for value in [
            "[]",
            "['KEY_TAB', 'KEY_TAB']",
            "['KEY_LEFTCTRL', 'KEY_MISSING']",
            "['KEY_LEFTCTRL', 'BTN_SOUTH']",
            "['BTN_LEFT', 'BTN_LEFT']",
        ] {
            let profile =
                format!("version = 1\n[[keyboard.bindings]]\nbutton = 'cross'\nhold = {value}\n");
            assert!(crate::profile::parse(&profile).is_err(), "{value}");
        }
    }

    #[test]
    fn scroll_taps_use_the_mouse_without_scheduling_a_key_release() {
        let config = config(
            r#"
            [[keyboard.bindings]]
            button = "cross"
            tap = { scroll = "down" }
            double_tap = { scroll = "right" }
            double_tap_ms = 100
            [[keyboard.bindings]]
            button = "r1"
            hold = "KEY_F13"
        "#,
        );
        let mut state = State::new(&config);
        let mut keys = Vec::with_capacity(1);
        let mut mouse = Vec::with_capacity(2);
        let mut events = Vec::with_capacity(5);
        let start = Instant::now();
        let allocations = allocation_counter::measure(|| {
            for (ms, cross, r1, deadline) in [
                (0, true, false, None),
                (10, false, false, Some(110)),
                (109, false, false, Some(110)),
                (110, false, true, None),
                (200, true, true, None),
                (210, false, true, Some(310)),
                (240, true, true, Some(310)),
                (250, false, true, None),
                (350, false, false, None),
            ] {
                let mut controls = Controls::default();
                if cross {
                    controls.buttons.insert(Button::South);
                }
                if r1 {
                    controls.buttons.insert(Button::RightShoulder);
                }
                let next = state
                    .update(
                        input(controls),
                        start + Duration::from_millis(ms),
                        |batch| {
                            emit_mixed_batches(batch, &mut keys, &mut mouse, |target, batch| {
                                events.extend(batch.iter().map(|event| {
                                    (ms, target, event.event_type(), event.code(), event.value())
                                }));
                                Ok(())
                            })
                        },
                    )
                    .unwrap();
                assert_eq!(next, deadline.map(|ms| start + Duration::from_millis(ms)));
            }
        });
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
        assert_eq!(
            events,
            [
                (110, Target::Keyboard, EventType::KEY, KeyCode::KEY_F13.0, 1),
                (
                    110,
                    Target::Mouse,
                    EventType::RELATIVE,
                    RelativeAxisCode::REL_WHEEL.0,
                    -1
                ),
                (
                    250,
                    Target::Mouse,
                    EventType::RELATIVE,
                    RelativeAxisCode::REL_HWHEEL.0,
                    1
                ),
                (350, Target::Keyboard, EventType::KEY, KeyCode::KEY_F13.0, 0),
            ]
        );
    }

    #[test]
    fn scroll_requires_a_known_direction_and_valid_repeat_settings() {
        for setting in [
            "tap = { scroll = 'diagonal' }",
            "tap = { scroll = 'up', unexpected = true }",
            "hold = { scroll = 'up' }\nscroll_repeat_ms = 0",
            "hold = { scroll = 'up' }\nscroll_repeat_ms = -1",
            "tap = { scroll = 'up' }\nscroll_repeat_ms = 40",
        ] {
            let profile =
                format!("version = 1\n[[keyboard.bindings]]\nbutton = 'cross'\n{setting}\n");
            assert!(crate::profile::parse(&profile).is_err(), "{setting}");
        }
    }

    #[test]
    fn held_scroll_repeats_at_deadlines_and_stops_on_release() {
        let config = config(
            r#"
            [[keyboard.bindings]]
            button = "cross"
            tap = "KEY_TAB"
            hold = { scroll = "left" }
            hold_ms = 100
            scroll_repeat_ms = 40
            [[keyboard.bindings]]
            button = "l1"
            hold = { scroll = "up" }
        "#,
        );
        let mut state = State::new(&config);
        let start = Instant::now();
        let tab = (EventType::KEY, KeyCode::KEY_TAB.0);
        let left = (EventType::RELATIVE, RelativeAxisCode::REL_HWHEEL.0, -1);
        let up = (EventType::RELATIVE, RelativeAxisCode::REL_WHEEL.0, 1);
        let frames = [
            (0, Some(Button::South), Some(100), None),
            (99, Some(Button::South), Some(100), None),
            (100, Some(Button::South), Some(140), Some(left)),
            (140, Some(Button::South), Some(180), Some(left)),
            (310, Some(Button::South), Some(350), Some(left)), // One step despite the late wake.
            (330, None, None, None),
            (350, None, None, None),
            (400, Some(Button::South), Some(500), None),
            (420, None, Some(450), Some((tab.0, tab.1, 1))),
            (450, None, None, Some((tab.0, tab.1, 0))),
            (500, Some(Button::LeftShoulder), Some(600), Some(up)), // Hold-only starts immediately.
            (599, Some(Button::LeftShoulder), Some(600), None),
            (600, Some(Button::LeftShoulder), Some(700), Some(up)),
            (700, None, None, None),
        ];
        let allocations = allocation_counter::measure(|| {
            for (ms, button, deadline, expected) in frames {
                let mut controls = Controls::default();
                if let Some(button) = button {
                    controls.buttons.insert(button);
                }
                let mut actual = None;
                let next = state
                    .update(
                        input(controls),
                        start + Duration::from_millis(ms),
                        |events| {
                            assert_eq!(events.len(), 1);
                            let event = events[0];
                            assert_eq!(
                                actual.replace((event.event_type(), event.code(), event.value())),
                                None
                            );
                            Ok(())
                        },
                    )
                    .unwrap();
                assert_eq!(actual, expected, "{ms} ms");
                assert_eq!(next, deadline.map(|ms| start + Duration::from_millis(ms)));
            }
        });
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }

    #[test]
    fn touch_swipe_preserves_remainders_and_tracks_the_contact_across_slots() {
        let config =
            config("[keyboard.touch_scroll]\nvertical_step = 100\nhorizontal_step = 100\n");
        let mut state = State::new(&config);
        let now = Instant::now();
        let mut observed = Vec::with_capacity(2);
        let contact = |id, x, y| Some(TouchContact { id, x, y });
        let v = RelativeAxisCode::REL_WHEEL.0;
        let h = RelativeAxisCode::REL_HWHEEL.0;
        let other = contact(2, 900, 90);
        let frames = [
            ([contact(1, 100, 500), None], &[][..]),
            ([contact(1, 150, 440), other], &[][..]),
            ([other, contact(1, 210, 370)], &[(v, 1), (h, 1)][..]),
            ([other, contact(1, 210, 370)], &[][..]),
            ([other, contact(1, 430, 410)], &[(h, 2)][..]),
            ([other, None], &[][..]), // Rebaseline to the remaining finger.
            ([contact(2, 1000, 400), None], &[(v, -3), (h, 1)][..]),
            ([None, None], &[][..]),
            ([contact(2, 60, 100), None], &[][..]),
        ];
        let allocations = allocation_counter::measure(|| {
            for (contacts, expected) in frames {
                observed.clear();
                let input = Input {
                    contacts,
                    ..Input::default()
                };
                let deadline = state
                    .update(input, now, |events| {
                        for event in events {
                            assert_eq!(event.event_type(), EventType::RELATIVE);
                            observed.push((event.code(), event.value()));
                        }
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(observed, expected, "{contacts:?}");
                assert_eq!(deadline, None);
            }
        });
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }

    #[test]
    fn touch_scroll_settings_require_a_valid_step_for_at_least_one_axis() {
        for settings in [
            "",
            "vertical_step = 0",
            "vertical_step = 1080",
            "horizontal_step = 1920",
            "vertical_step = 100\nfingers = 0",
            "vertical_step = 100\nfingers = 3",
        ] {
            let profile = format!("version = 1\n[keyboard.touch_scroll]\n{settings}\n");
            assert!(crate::profile::parse(&profile).is_err(), "{settings}");
        }
        assert!(config("[keyboard.touch_scroll]\nvertical_step = 120\n").has_scroll());
    }

    #[test]
    fn two_finger_swipes_use_the_midpoint_and_reset_when_the_pair_changes() {
        let config = config(
            "[keyboard.touch_scroll]\nfingers = 2\nvertical_step = 5\nhorizontal_step = 5\n",
        );
        let mut state = State::new(&config);
        let now = Instant::now();
        let contact = |id, x, y| Some(TouchContact { id, x, y });
        let v = RelativeAxisCode::REL_WHEEL.0;
        let h = RelativeAxisCode::REL_HWHEEL.0;
        for (contacts, expected) in [
            ([contact(1, 100, 500), None], &[][..]),
            ([contact(1, 100, 480), None], &[][..]),
            ([contact(1, 100, 480), contact(2, 200, 300)], &[][..]),
            ([contact(2, 209, 300), contact(1, 100, 471)], &[][..]),
            (
                [contact(1, 101, 470), contact(2, 209, 300)],
                &[(v, 1), (h, 1)][..],
            ),
            ([contact(1, 101, 450), None], &[][..]),
            ([contact(1, 101, 450), contact(2, 209, 250)], &[][..]),
            ([contact(1, 101, 445), contact(2, 209, 245)], &[(v, 1)][..]),
            ([contact(1, 101, 430), contact(3, 500, 750)], &[][..]),
            ([contact(1, 101, 425), contact(1, 500, 745)], &[][..]),
        ] {
            let input = Input {
                contacts,
                ..Input::default()
            };
            let mut observed = Vec::new();
            assert_eq!(
                state
                    .update(input, now, |events| {
                        for event in events {
                            assert_eq!(event.event_type(), EventType::RELATIVE);
                            observed.push((event.code(), event.value()));
                        }
                        Ok(())
                    })
                    .unwrap(),
                None
            );
            assert_eq!(observed, expected, "{contacts:?}");
        }
    }

    #[test]
    fn touch_pointer_yields_to_two_finger_scroll_and_resumes_without_a_jump() {
        let config = config(
            r#"
            [keyboard.touch_pointer]
            sensitivity_x = 0.5
            sensitivity_y = 2.0
            [keyboard.touch_scroll]
            fingers = 2
            vertical_step = 10
            horizontal_step = 10
        "#,
        );
        let mut state = State::new(&config);
        let now = Instant::now();
        let contact = |id, x, y| Some(TouchContact { id, x, y });
        let x = RelativeAxisCode::REL_X.0;
        let y = RelativeAxisCode::REL_Y.0;
        let v = RelativeAxisCode::REL_WHEEL.0;
        let h = RelativeAxisCode::REL_HWHEEL.0;
        let mut observed = Vec::with_capacity(4);
        let allocations = allocation_counter::measure(|| {
            for (contacts, expected) in [
                ([contact(1, 100, 500), None], &[][..]),
                ([contact(1, 101, 501), None], &[(y, 2)][..]),
                ([None, contact(1, 102, 501)], &[(x, 1)][..]),
                ([contact(1, 103, 500), None], &[(y, -2)][..]),
                ([contact(1, 103, 500), contact(2, 200, 500)], &[][..]),
                (
                    [contact(2, 220, 480), contact(1, 123, 480)],
                    &[(v, 2), (h, 2)][..],
                ),
                ([contact(1, 400, 600), None], &[][..]),
                ([contact(1, 401, 600), None], &[][..]),
                ([contact(1, 400, 599), None], &[(y, -2)][..]),
                ([None, None], &[][..]),
                ([contact(3, 800, 900), None], &[][..]),
            ] {
                observed.clear();
                let input = Input {
                    contacts,
                    ..Input::default()
                };
                assert_eq!(
                    state
                        .update(input, now, |events| {
                            for event in events {
                                assert_eq!(event.event_type(), EventType::RELATIVE);
                                observed.push((event.code(), event.value()));
                            }
                            Ok(())
                        })
                        .unwrap(),
                    None
                );
                assert_eq!(observed, expected, "{contacts:?}");
            }
        });
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }

    #[test]
    fn touch_pointer_profile_validates_both_sensitivities() {
        let settings = "version = 1\n[keyboard.touch_pointer]\n";
        assert!(crate::profile::parse(settings).is_ok());
        for invalid in [
            "sensitivity_x = -1",
            "sensitivity_y = nan",
            "sensitivity_y = 101",
        ] {
            assert!(
                crate::profile::parse(&format!("{settings}{invalid}\n")).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn touch_pointer_tap_clicks_only_for_short_stationary_single_contacts() {
        let config = config(
            r#"
            [keyboard.touch_pointer]
            sensitivity_x = 0.0
            sensitivity_y = 0.0
            tap_to_click = true
        "#,
        );
        assert!(config.keys().any(|key| key == KeyCode::BTN_LEFT));
        let mut state = State::new(&config);
        let start = Instant::now();
        let contact = |id, x, y| Some(TouchContact { id, x, y });
        let a = contact(1, 100, 100);
        let b = contact(2, 200, 200);
        let mut values = Vec::with_capacity(2);
        let frames = [
            (0, [a, None], false, None),
            (30, [None, contact(1, 110, 115)], false, None),
            (60, [None, None], false, Some(90)),
            (90, [None, None], false, None),
            (100, [a, None], false, None),
            (110, [contact(1, 130, 130), None], false, None), // Diagonal exceeds the allowed travel.
            (120, [a, None], false, None), // Returning does not restore a tap candidate.
            (130, [None, None], false, None),
            (150, [a, None], false, None),
            (160, [a, b], false, None),
            (170, [None, b], false, None),
            (180, [None, None], false, None),
            (200, [a, None], false, None),
            (400, [None, None], false, None), // At the time limit, the contact is not a tap.
            (420, [a, None], false, None),
            (430, [b, None], false, None), // ID change without lift cancels the tap.
            (440, [None, None], false, None),
            (460, [a, None], false, None),
            (470, [a, None], true, None),
            (480, [None, None], false, None),
        ];
        let allocations = allocation_counter::measure(|| {
            for (ms, contacts, clicked, expected_deadline) in frames {
                let mut input = Input {
                    contacts,
                    ..Input::default()
                };
                if clicked {
                    input.controls.buttons.insert(Button::TouchpadClick);
                }
                let deadline = state
                    .update(input, start + Duration::from_millis(ms), |events| {
                        for event in events {
                            assert_eq!(event.event_type(), EventType::KEY);
                            assert_eq!(event.code(), KeyCode::BTN_LEFT.0);
                            values.push((ms, event.value()));
                        }
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(
                    deadline,
                    expected_deadline.map(|ms| start + Duration::from_millis(ms))
                );
            }
        });
        assert_eq!(values, [(60, 1), (90, 0)]);
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }

    #[test]
    fn modified_clicks_keep_keyboard_changes_between_mouse_release_and_press() {
        let config = config(
            r#"
            [[keyboard.bindings]]
            button = "cross"
            hold = ["BTN_LEFT", "KEY_LEFTSHIFT"]
            [[keyboard.bindings]]
            button = "circle"
            hold = ["KEY_LEFTCTRL", "BTN_RIGHT"]
        "#,
        );
        let mut state = State::new(&config);
        let mut keys = Vec::with_capacity(2);
        let mut buttons = Vec::with_capacity(2);
        let mut sent = Vec::with_capacity(8);
        let start = Instant::now();
        let allocations = allocation_counter::measure(|| {
            for button in [Some(Button::South), Some(Button::East), None] {
                let mut controls = Controls::default();
                if let Some(button) = button {
                    controls.buttons.insert(button);
                }
                state
                    .update(input(controls), start, |events| {
                        emit_mixed_batches(events, &mut keys, &mut buttons, |target, batch| {
                            sent.extend(
                                batch
                                    .iter()
                                    .map(|event| (target, KeyCode(event.code()), event.value())),
                            );
                            Ok(())
                        })
                    })
                    .unwrap();
            }
        });
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
        assert_eq!(
            sent,
            [
                (Target::Keyboard, KeyCode::KEY_LEFTSHIFT, 1),
                (Target::Mouse, KeyCode::BTN_LEFT, 1),
                (Target::Mouse, KeyCode::BTN_LEFT, 0),
                (Target::Keyboard, KeyCode::KEY_LEFTSHIFT, 0),
                (Target::Keyboard, KeyCode::KEY_LEFTCTRL, 1),
                (Target::Mouse, KeyCode::BTN_RIGHT, 1),
                (Target::Mouse, KeyCode::BTN_RIGHT, 0),
                (Target::Keyboard, KeyCode::KEY_LEFTCTRL, 0),
            ]
        );
    }

    #[test]
    fn custom_gesture_times_drive_hold_and_double_tap_deadlines() {
        let config = config(
            r#"
            [[keyboard.bindings]]
            button = "cross"
            tap = "KEY_SPACE"
            hold = "KEY_LEFTSHIFT"
            double_tap = "KEY_R"
            hold_ms = 100
            double_tap_ms = 120
            [[keyboard.bindings]]
            button = "l1"
            hold = "KEY_Q"
            hold_ms = 350
        "#,
        );
        let mut state = State::new(&config);
        let start = Instant::now();
        let space = KeyCode::KEY_SPACE.0;
        let r = KeyCode::KEY_R.0;
        let shift = KeyCode::KEY_LEFTSHIFT.0;
        let q = KeyCode::KEY_Q.0;
        for (ms, cross, shoulder, deadline, expected) in [
            (0, true, true, Some(100), &[(q, 1)][..]),
            (99, true, false, Some(100), &[(q, 0)][..]),
            (100, true, false, None, &[(shift, 1)][..]),
            (150, false, false, None, &[(shift, 0)][..]),
            (200, true, false, Some(300), &[][..]),
            (210, false, false, Some(330), &[][..]),
            (330, false, false, Some(360), &[(space, 1)][..]),
            (360, false, false, None, &[(space, 0)][..]),
            (400, true, false, Some(500), &[][..]),
            (410, false, false, Some(530), &[][..]),
            (450, true, false, Some(530), &[][..]),
            (529, false, false, Some(559), &[(r, 1)][..]),
            (559, false, false, None, &[(r, 0)][..]),
        ] {
            let mut controls = Controls::default();
            if cross {
                controls.buttons.insert(Button::South);
            }
            if shoulder {
                controls.buttons.insert(Button::LeftShoulder);
            }
            let mut events = Vec::new();
            let next = state
                .update(
                    input(controls),
                    start + Duration::from_millis(ms),
                    |batch| {
                        events.extend(batch.iter().map(|event| (event.code(), event.value())));
                        Ok(())
                    },
                )
                .unwrap();
            assert_eq!(events, expected, "{ms} ms");
            assert_eq!(next, deadline.map(|ms| start + Duration::from_millis(ms)));
        }
    }

    #[test]
    fn gesture_times_require_positive_integer_milliseconds() {
        for setting in [
            "hold_ms = 0",
            "hold_ms = -10",
            "double_tap_ms = 0",
            "double_tap_ms = 12.5",
        ] {
            let profile = format!(
                "version = 1\n[[keyboard.bindings]]\nbutton = 'cross'\ntap = 'KEY_TAB'\n{setting}\n"
            );
            assert!(crate::profile::parse(&profile).is_err(), "{setting}");
        }
    }
}
