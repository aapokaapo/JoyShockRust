//! Deterministic keyboard, touch and mouse binding state.

use std::{
    num::NonZeroU32,
    time::{Duration, Instant},
};

use joyshockrust_core::{
    Button, Buttons, Controls,
    trigger::{TriggerGate, TriggerSide},
};
use joyshockrust_input::{InputReport, TouchContact};

use crate::{
    gyro_gate::{self, DOUBLE_TAP_WINDOW, HOLD_TIME},
    touchpad::{Grid, TouchRegion},
};

mod key_set;
mod stick;
mod touch_motion;

use key_set::{KeySet, key_set};

/// Meaning of a resolved output key, independent of the OS event encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Key,
    Modifier,
    MouseButton,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub id: u16,
    pub kind: KeyKind,
}

/// Supplies key names, capabilities and output IDs during configuration.
pub trait KeyResolver {
    fn keyboard(&self, name: &str) -> Result<Key, String>;
    fn mouse_button(&self, name: &str) -> Result<Key, String>;

    fn gesture(&self, name: &str) -> Result<Key, String> {
        self.mouse_button(name).or_else(|_| self.keyboard(name))
    }

    fn default_tap_buttons(&self) -> [Key; 2];

    fn describe(&self, key: Key) -> String {
        key.id.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelativeAxis {
    X,
    Y,
    Wheel,
    HorizontalWheel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Key { id: u16, pressed: bool },
    Relative { axis: RelativeAxis, delta: i32 },
}

impl Event {
    fn key_id(self) -> u16 {
        match self {
            Self::Key { id, .. } => id,
            Self::Relative { .. } => unreachable!("only key events are sorted"),
        }
    }
}

const TAP_KEY_TIME: Duration = Duration::from_millis(30);
const SCROLL_REPEAT_TIME: Duration = Duration::from_millis(100);
const WHEEL_AXES: [RelativeAxis; 2] = [RelativeAxis::Wheel, RelativeAxis::HorizontalWheel];
const POINTER_AXES: [RelativeAxis; 2] = [RelativeAxis::X, RelativeAxis::Y];

#[cfg_attr(feature = "profile", derive(serde::Deserialize))]
#[cfg_attr(feature = "profile", serde(deny_unknown_fields))]
pub struct Settings {
    #[cfg_attr(feature = "profile", serde(default))]
    pub grab_touchpad: bool,
    #[cfg_attr(feature = "profile", serde(default))]
    bindings: Vec<RawBinding>,
    #[cfg_attr(feature = "profile", serde(default))]
    keep_partial: KeepPartial,
    #[cfg_attr(feature = "profile", serde(default))]
    button_output: std::collections::BTreeMap<String, ButtonOutput>,
    #[cfg_attr(feature = "profile", serde(default))]
    sticks: Vec<stick::Settings>,
    touch_scroll: Option<touch_motion::ScrollSettings>,
    touch_pointer: Option<touch_motion::PointerSettings>,
}

#[derive(Clone, Copy, PartialEq)]
#[cfg_attr(feature = "profile", derive(serde::Deserialize))]
#[cfg_attr(feature = "profile", serde(rename_all = "lowercase"))]
enum ButtonOutput {
    Keyboard,
    Mouse,
}

#[derive(Default, PartialEq)]
pub struct GamepadMask {
    pub buttons: Buttons,
}

#[derive(Default)]
#[cfg_attr(feature = "profile", derive(serde::Deserialize))]
#[cfg_attr(feature = "profile", serde(default, deny_unknown_fields))]
struct KeepPartial {
    l2: bool,
    r2: bool,
}

impl KeepPartial {
    fn for_side(&self, side: TriggerSide) -> bool {
        match side {
            TriggerSide::Left => self.l2,
            TriggerSide::Right => self.r2,
        }
    }
}

#[cfg_attr(feature = "profile", derive(serde::Deserialize))]
#[cfg_attr(feature = "profile", serde(deny_unknown_fields))]
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

#[cfg_attr(feature = "profile", derive(serde::Deserialize))]
#[cfg_attr(feature = "profile", serde(untagged))]
enum RawOutput {
    Single(String),
    Combination(Vec<String>),
    Scroll(ScrollOutput),
}

#[cfg_attr(feature = "profile", derive(serde::Deserialize))]
#[cfg_attr(feature = "profile", serde(deny_unknown_fields))]
struct ScrollOutput {
    scroll: ScrollDirection,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "profile", derive(serde::Deserialize))]
#[cfg_attr(feature = "profile", serde(rename_all = "lowercase"))]
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
    Keys(Box<[Key]>),
    Scroll(ScrollDirection),
}

impl Action {
    fn keys(&self) -> &[Key] {
        match self {
            Self::Keys(keys) => keys,
            Self::Scroll(_) => &[],
        }
    }
}

#[derive(PartialEq)]
pub struct Config {
    pub gamepad_mask: GamepadMask,
    buttons: Vec<Binding>,
    sticks: Vec<stick::Binding>,
    touch_scroll: Option<touch_motion::ScrollSettings>,
    touch_pointer: Option<touch_motion::PointerSettings<Key>>,
    tap_buttons: [Option<Key>; 2],
}

impl Config {
    pub fn is_empty(&self) -> bool {
        self.buttons.is_empty()
            && self.sticks.is_empty()
            && self.touch_scroll.is_none()
            && !self.has_pointer()
    }

    pub fn has_pointer(&self) -> bool {
        self.touch_pointer.is_some()
    }

    pub fn keys(&self) -> impl Iterator<Item = Key> {
        self.buttons
            .iter()
            .flat_map(Binding::actions)
            .flat_map(|action| action.keys().iter().copied())
            .chain(self.sticks.iter().flat_map(|binding| binding.keys()))
            .chain(self.tap_buttons.into_iter().flatten())
    }

    pub fn has_scroll(&self) -> bool {
        self.touch_scroll.is_some()
            || self
                .buttons
                .iter()
                .flat_map(Binding::actions)
                .any(|action| matches!(action, Action::Scroll(_)))
    }

    pub fn relative_event_capacity(&self) -> usize {
        usize::from(self.has_scroll()) * WHEEL_AXES.len()
            + usize::from(self.touch_pointer.is_some()) * POINTER_AXES.len()
    }
}

impl RawOutput {
    fn resolve(self, keys: &impl KeyResolver) -> Result<Action, String> {
        match self {
            Self::Single(name) => Ok(Action::Keys(Box::new([keys.gesture(&name)?]))),
            Self::Scroll(output) => Ok(Action::Scroll(output.scroll)),
            Self::Combination(names) => {
                if names.is_empty() {
                    return Err("gesture combination needs at least one output".into());
                }
                let mut keys = names
                    .iter()
                    .map(|name| keys.gesture(name))
                    .collect::<Result<Vec<_>, _>>()?;
                keys.sort_unstable_by_key(|key| key.id);
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
    Direction(Button),
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
    PartialTrigger {
        lower: TriggerGate,
        upper: TriggerGate,
        keep_partial: bool,
    },
}

impl PartialEq for BindingSource {
    fn eq(&self, other: &Self) -> bool {
        match (*self, *other) {
            (Self::Single(a), Self::Single(b)) => a == b,
            (
                Self::PartialTrigger {
                    lower: a,
                    upper: b,
                    keep_partial: keep_a,
                },
                Self::PartialTrigger {
                    lower: c,
                    upper: d,
                    keep_partial: keep_b,
                },
            ) => a == c && b == d && keep_a == keep_b,
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
        if let Some(direction) = crate::gamepad::dpad_button_by_name(name) {
            return Ok(Self::Direction(direction));
        }
        gyro_gate::button_by_name(name)
            .map(Self::Button)
            .ok_or_else(|| format!("unknown keyboard binding button {name}"))
    }

    fn pressed(self, input: &Input, click: Option<TouchContact>, was_pressed: bool) -> bool {
        let controls = &input.controls;
        match self {
            Self::Button(button) => controls.buttons.contains(button),
            Self::Trigger(trigger) => trigger.pressed(controls, was_pressed),
            Self::Touch(region) => region.pressed(&input.contacts),
            Self::TouchClick(region) => click.is_some_and(|contact| region.contains(contact)),
            Self::Direction(direction) => controls.dpad.buttons().contains(direction),
        }
    }
}

impl Settings {
    pub fn resolve(
        self,
        grid: Option<&Grid>,
        triggers: [crate::gamepad::TriggerStages; 2],
        keys: &impl KeyResolver,
    ) -> Result<Config, String> {
        let scaled = |gate: TriggerGate| {
            let index = usize::from(gate.side() == super::gyro_gate::TriggerSide::Right);
            gate.within_travel(triggers[index].travel_percent)
        };
        let mut gamepad_mask = GamepadMask::default();
        for name in self.button_output.keys() {
            match Source::parse(name, grid)? {
                Source::Button(button) | Source::Direction(button) => {
                    gamepad_mask.buttons.insert(button)
                }
                _ => return Err(format!("button_output requires a physical button: {name}")),
            }
        }
        let mut bindings = Vec::<Binding>::new();
        for raw in self.bindings {
            let (source, name) = match (raw.button, raw.trigger, raw.chord) {
                (Some(name), None, None) => {
                    (BindingSource::Single(Source::parse(&name, grid)?), name)
                }
                (None, Some(spec), None) => {
                    let named = spec.split_once(':').and_then(|(side, stage)| {
                        let settings = match side {
                            "l2" => triggers[0],
                            "r2" => triggers[1],
                            _ => return None,
                        };
                        match stage {
                            "partial" => Some(BindingSource::PartialTrigger {
                                lower: settings.lower,
                                upper: settings.upper,
                                keep_partial: settings.keep_partial,
                            }),
                            "full" => Some(BindingSource::Single(Source::Trigger(settings.upper))),
                            _ => None,
                        }
                    });
                    let source = if let Some(source) = named {
                        source
                    } else if let Some((lower, upper)) = TriggerGate::partial(&spec)? {
                        BindingSource::PartialTrigger {
                            lower: scaled(lower),
                            upper: scaled(upper),
                            keep_partial: self.keep_partial.for_side(lower.side()),
                        }
                    } else {
                        BindingSource::Single(Source::Trigger(scaled(TriggerGate::parse(&spec)?)))
                    };
                    (source, spec)
                }
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
                tap: raw.tap.map(|action| action.resolve(keys)).transpose()?,
                hold: raw.hold.map(|action| action.resolve(keys)).transpose()?,
                double_tap: raw
                    .double_tap
                    .map(|action| action.resolve(keys))
                    .transpose()?,
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
            let physical = name.split(':').next().unwrap_or(&name);
            if matches!(
                source,
                BindingSource::Single(
                    Source::Button(_) | Source::Direction(_) | Source::Trigger(_)
                ) | BindingSource::PartialTrigger { .. }
            ) && let Some(output) = self.button_output.get(physical)
                && binding.actions().any(|action| match action {
                    Action::Keys(keys) => keys.iter().any(|&key| {
                        (key.kind == KeyKind::MouseButton) != (*output == ButtonOutput::Mouse)
                    }),
                    Action::Scroll(_) => *output != ButtonOutput::Mouse,
                })
            {
                return Err(format!(
                    "keyboard binding for {name} conflicts with button_output for {physical}"
                ));
            }
            if raw.scroll_repeat_ms.is_some() && !matches!(binding.hold, Some(Action::Scroll(_))) {
                return Err(format!(
                    "keyboard binding for {name}: scroll_repeat_ms requires a scroll hold action"
                ));
            }
            bindings.push(binding);
        }
        let sticks = stick::Settings::resolve_all(self.sticks, keys)?;
        let touch_scroll = self
            .touch_scroll
            .map(touch_motion::ScrollSettings::validate)
            .transpose()?;
        let touch_pointer = self
            .touch_pointer
            .map(|settings| settings.resolve(keys))
            .transpose()?;
        let config = Config {
            gamepad_mask,
            buttons: bindings,
            sticks,
            touch_scroll,
            touch_pointer,
            tap_buttons: touch_pointer.map_or([None; 2], |settings| {
                settings.tap_buttons(keys.default_tap_buttons())
            }),
        };
        for key in config
            .sticks
            .iter()
            .flat_map(|stick| stick.pulsed_zone_keys())
        {
            if config
                .keys()
                .filter(|&mapped| mapped == key)
                .nth(1)
                .is_some()
            {
                return Err(format!(
                    "keyboard stick pulse key {} must have a single binding",
                    keys.describe(key)
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
    reached_full_trigger: bool,
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
        let mut suppress_tap = false;
        let pressed = match self.binding.source {
            BindingSource::Single(source) => {
                self.source_pressed[0] = source.pressed(input, click, self.source_pressed[0]);
                self.source_pressed[0]
            }
            BindingSource::PartialTrigger {
                lower,
                keep_partial: true,
                ..
            } => {
                self.source_pressed[0] = lower.pressed(&input.controls, self.source_pressed[0]);
                self.source_pressed[0]
            }
            BindingSource::PartialTrigger {
                lower,
                upper,
                keep_partial: false,
            } => {
                self.source_pressed[0] = lower.pressed(&input.controls, self.source_pressed[0]);
                self.source_pressed[1] = upper.pressed(&input.controls, self.source_pressed[1]);
                if self.source_pressed[1] {
                    // A full pull cancels the partial gesture instead of completing a tap.
                    self.reached_full_trigger = true;
                    self.press = Press::Up;
                    self.next_scroll = None;
                }
                suppress_tap = self.reached_full_trigger;
                if !self.source_pressed[0] {
                    self.reached_full_trigger = false;
                }
                self.source_pressed[0] && !self.source_pressed[1]
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
                if !suppress_tap && now.duration_since(start) < self.binding.hold_time {
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
pub struct Input {
    pub controls: Controls,
    pub contacts: [Option<TouchContact>; 2],
}

impl From<InputReport> for Input {
    fn from(report: InputReport) -> Self {
        Self {
            controls: report.controls,
            contacts: report.touch_contacts,
        }
    }
}

pub struct State {
    gestures: Vec<Gesture>,
    sticks: Vec<stick::State>,
    touch_scroll: Option<touch_motion::ScrollState>,
    touch_pointer: Option<touch_motion::PointerState>,
    touch_tap: Option<(touch_motion::TapState, [Option<Key>; 2])>,
    pulses: Vec<(Key, Instant)>,
    modifiers: KeySet,
    held_zone_keys: KeySet,
    pulsed_zone_keys: KeySet,
    held: KeySet,
    next_held: KeySet,
    events: Vec<Event>,
    input: Input,
    click: Option<TouchContact>,
}

impl State {
    pub fn new(config: &Config) -> Self {
        let gestures = config
            .buttons
            .iter()
            .cloned()
            .map(|binding| Gesture {
                binding,
                source_pressed: [false; 2],
                reached_full_trigger: false,
                press: Press::Up,
                pending_tap: None,
                next_scroll: None,
            })
            .collect();
        let words = config
            .keys()
            .map(|key| usize::from(key.id) / 64 + 1)
            .max()
            .unwrap_or_default();
        let key_count = key_set(words, config.keys()).iter().count();
        let tap_buttons = config.tap_buttons;
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
            touch_tap: config
                .touch_pointer
                .filter(|_| tap_buttons.iter().any(Option::is_some))
                .map(|settings| (touch_motion::TapState::new(settings), tap_buttons)),
            // At most one pulse deadline per mapped key.
            pulses: Vec::with_capacity(key_count),
            modifiers: key_set(
                words,
                config.keys().filter(|key| key.kind == KeyKind::Modifier),
            ),
            held_zone_keys: key_set(
                words,
                config
                    .sticks
                    .iter()
                    .flat_map(|binding| binding.held_zone_keys()),
            ),
            pulsed_zone_keys: key_set(
                words,
                config
                    .sticks
                    .iter()
                    .flat_map(|binding| binding.pulsed_zone_keys()),
            ),
            held: KeySet::new(words),
            next_held: KeySet::new(words),
            events: Vec::with_capacity(key_count + config.relative_event_capacity()),
            input: Input::default(),
            click: None,
        }
    }

    #[inline]
    pub fn update<E>(
        &mut self,
        input: Input,
        now: Instant,
        mut emit: impl FnMut(&[Event]) -> Result<(), E>,
    ) -> Result<Option<Instant>, E> {
        if !input.controls.buttons.contains(Button::TouchpadClick) {
            self.click = None;
        } else if !self.input.controls.buttons.contains(Button::TouchpadClick) {
            // Match gamepad mapping: latch the first active contact at the click edge.
            self.click = input.contacts.into_iter().flatten().next();
        }
        self.input = input;
        let held = &mut self.next_held;
        held.clear();
        let mut deadline = None;
        let mut scroll = self
            .touch_scroll
            .as_mut()
            .map_or([0; WHEEL_AXES.len()], |scroll| {
                scroll.advance(input.contacts)
            });
        self.pulses.retain(|&(_, until)| until > now);
        if let Some((tap, [one_finger, two_fingers])) = &mut self.touch_tap {
            let cancelled = scroll != [0; WHEEL_AXES.len()]
                || input.controls.buttons.contains(Button::TouchpadClick);
            let button = match tap.advance(input.contacts, cancelled, now) {
                Some(touch_motion::TapCount::One) => *one_finger,
                Some(touch_motion::TapCount::Two) => *two_fingers,
                None => None,
            };
            if let Some(button) = button {
                schedule_pulse(&mut self.pulses, button, now + TAP_KEY_TIME);
            }
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
                        held.insert(key.id);
                    }
                }
                Some(Action::Scroll(direction)) => direction.accumulate(&mut scroll),
                None => {}
            }
            deadline = deadline.into_iter().chain(gesture.deadline()).min();
        }
        for &(key, until) in &self.pulses {
            held.insert(key.id);
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
                .map(|key| Event::Key {
                    id: key,
                    pressed: false,
                }),
        );
        sort_releases(&mut self.events, &self.held_zone_keys, &self.modifiers);
        let releases = self.events.len();
        self.events.extend(
            held.iter()
                .filter(|&key| !self.held.contains(key))
                .map(|key| Event::Key {
                    id: key,
                    pressed: true,
                }),
        );
        // Shortcut modifiers precede other keys. Sprint pulses follow movement.
        self.events[releases..].sort_unstable_by_key(|event| {
            let key = event.key_id();
            let order = if self.held_zone_keys.contains(key) {
                0
            } else if self.pulsed_zone_keys.contains(key) {
                3
            } else if self.modifiers.contains(key) {
                1
            } else {
                2
            };
            (order, key)
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
                self.events.push(Event::Relative { axis, delta });
            }
        }
        if !self.events.is_empty() {
            emit(&self.events)?;
        }
        std::mem::swap(&mut self.held, &mut self.next_held);
        Ok(deadline)
    }

    pub fn last_input(&self) -> Input {
        self.input
    }

    pub fn held_keys(&self) -> impl Iterator<Item = u16> + '_ {
        self.held.iter()
    }

    pub fn event_capacity(&self) -> usize {
        self.events.capacity()
    }

    pub fn release<E>(&mut self, emit: impl FnOnce(&[Event]) -> Result<(), E>) -> Result<(), E> {
        self.events.clear();
        self.events
            .extend(self.held.iter().map(|id| Event::Key { id, pressed: false }));
        sort_releases(&mut self.events, &self.held_zone_keys, &self.modifiers);
        if !self.events.is_empty() {
            emit(&self.events)?;
            self.held.clear();
        }
        Ok(())
    }
}

fn schedule_pulse(pulses: &mut Vec<(Key, Instant)>, key: Key, until: Instant) {
    if let Some(pulse) = pulses.iter_mut().find(|pulse| pulse.0 == key) {
        pulse.1 = until;
    } else {
        pulses.push((key, until));
    }
}

fn sort_releases(events: &mut [Event], held_zone_keys: &KeySet, modifiers: &KeySet) {
    events.sort_unstable_by_key(|event| {
        let key = event.key_id();
        (held_zone_keys.contains(key), modifiers.contains(key), key)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_key_and_failed_batches_keep_the_committed_state() {
        // A backend assigns the key ID. Using u16::MAX exercises the
        // portable bitset without relying on the Linux key range.
        let key = Key {
            id: u16::MAX,
            kind: KeyKind::Key,
        };
        let config = Config {
            gamepad_mask: GamepadMask::default(),
            buttons: [Button::South, Button::RightShoulder]
                .map(|button| Binding {
                    source: BindingSource::Single(Source::Button(button)),
                    tap: None,
                    hold: Some(Action::Keys(Box::new([key]))),
                    double_tap: None,
                    hold_time: HOLD_TIME,
                    double_tap_window: DOUBLE_TAP_WINDOW,
                    scroll_repeat_time: SCROLL_REPEAT_TIME,
                })
                .into(),
            sticks: vec![],
            touch_scroll: None,
            touch_pointer: None,
            tap_buttons: [None; 2],
        };
        let mut state = State::new(&config);
        let now = Instant::now();
        let mut input = Input::default();
        input.controls.buttons.insert(Button::South);
        let press = [Event::Key {
            id: key.id,
            pressed: true,
        }];
        let release = [Event::Key {
            id: key.id,
            pressed: false,
        }];
        assert_eq!(
            state.update(input, now, |events| {
                assert_eq!(events, press);
                Err("output rejected")
            }),
            Err("output rejected")
        );
        assert_eq!(state.held_keys().next(), None);
        state
            .update::<()>(input, now, |events| {
                assert_eq!(events, press);
                Ok(())
            })
            .unwrap();

        input.controls.buttons.insert(Button::RightShoulder);
        state
            .update::<()>(input, now, |_| {
                panic!("a second source must not press the key again")
            })
            .unwrap();
        input.controls.buttons.remove(Button::South);
        state
            .update::<()>(input, now, |_| {
                panic!("the other source still holds the key")
            })
            .unwrap();
        assert_eq!(
            state.release(|events| {
                assert_eq!(events, release);
                Err("output rejected")
            }),
            Err("output rejected")
        );
        assert_eq!(state.held_keys().next(), Some(key.id));
        state
            .release::<()>(|events| {
                assert_eq!(events, release);
                Ok(())
            })
            .unwrap();
        assert_eq!(state.held_keys().next(), None);

        let mut new_session = State::new(&config);
        new_session
            .update::<()>(Input::default(), now, |_| {
                panic!("stale output in a new session")
            })
            .unwrap();
        assert_eq!(new_session.held_keys().next(), None);
    }
}
