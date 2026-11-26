use joyshockrust_core::{Button, Buttons, Controls, DPad, StickSide, trigger::TriggerGate};
use joyshockrust_input::TouchContact;
use joyshockrust_motion::stick_response::StickCurve;

use crate::touchpad::{ClickState, Grid};

#[derive(Clone, Copy, PartialEq)]
pub struct TriggerStages {
    pub lower: TriggerGate,
    pub upper: TriggerGate,
    pub targets: [Option<Button>; 2],
    pub keep_partial: bool,
    pub travel_percent: u8,
}

pub use joyshockrust_core::trigger::AnalogTriggerRoute;

pub fn route_trigger_axes(
    controls: &mut Controls,
    travel: [Option<u8>; 2],
    routes: [AnalogTriggerRoute; 2],
    suppressed: &mut Buttons,
) {
    let mut output = [0.0_f32; 2];
    for (index, ((axis, full), route)) in [controls.left_trigger, controls.right_trigger]
        .into_iter()
        .zip(travel)
        .zip(routes)
        .enumerate()
    {
        let destination = match route {
            AnalogTriggerRoute::Same => index,
            AnalogTriggerRoute::Other => 1 - index,
            AnalogTriggerRoute::Off => continue,
        };
        if destination != index {
            suppressed.insert([Button::LeftTrigger, Button::RightTrigger][index]);
        }
        let scaled = full.map_or(axis, |full| (axis / (f32::from(full) / 100.0)).min(1.0));
        output[destination] = output[destination].max(scaled);
    }
    [controls.left_trigger, controls.right_trigger] = output;
}

#[derive(Default, PartialEq)]
pub struct ButtonRemaps {
    base: Vec<(Button, Button)>,
    layers: Vec<RemapLayer>,
}

#[derive(PartialEq)]
struct RemapLayer {
    modifier: Button,
    rules: Vec<(Button, Button)>,
}

#[derive(Default)]
pub struct RemapState {
    active: Option<usize>,
    previous: Buttons,
    trigger_pressed: [[bool; 2]; 2],
}

impl RemapState {
    pub fn trigger_outputs(
        &mut self,
        controls: &Controls,
        stages: Option<[TriggerStages; 2]>,
        suppressed: &mut Buttons,
    ) -> Buttons {
        let mut targets = Buttons::default();
        if let Some(stages) = stages {
            for ((settings, physical), active) in stages
                .into_iter()
                .zip([Button::LeftTrigger, Button::RightTrigger])
                .zip(&mut self.trigger_pressed)
            {
                if settings.targets == [None; 2] || suppressed.contains(physical) {
                    *active = [false; 2];
                    continue;
                }
                suppressed.insert(physical);
                let [partial, full] = *active;
                *active = [
                    settings.lower.pressed(controls, partial),
                    settings.upper.pressed(controls, full),
                ];
                let [partial, full] = *active;
                for (pressed, target) in [partial && (!full || settings.keep_partial), full]
                    .into_iter()
                    .zip(settings.targets)
                {
                    if pressed && let Some(target) = target {
                        targets.insert(target);
                    }
                }
            }
        }
        targets
    }
}

impl ButtonRemaps {
    pub fn is_empty(&self) -> bool {
        self.base.is_empty() && self.layers.is_empty()
    }

    pub fn add(&mut self, spec: &str) -> Result<(), &'static str> {
        add_rule(&mut self.base, parse_rule(spec)?)
    }

    pub fn add_layer(&mut self, spec: &str) -> Result<(), &'static str> {
        let (modifier, rule) = spec
            .split_once(':')
            .ok_or("expected MODIFIER:SOURCE=TARGET")?;
        let modifier = remap_button_by_name(modifier).ok_or("unknown remap layer button")?;
        let rule = parse_rule(rule)?;
        if let Some(layer) = self
            .layers
            .iter_mut()
            .find(|layer| layer.modifier == modifier)
        {
            add_rule(&mut layer.rules, rule)
        } else {
            self.layers.push(RemapLayer {
                modifier,
                rules: vec![rule],
            });
            Ok(())
        }
    }

    pub fn apply(&self, controls: Controls, state: &mut RemapState) -> Controls {
        self.apply_filtered(controls, state, Buttons::default(), Buttons::default())
    }

    pub fn apply_filtered(
        &self,
        mut controls: Controls,
        state: &mut RemapState,
        suppressed: Buttons,
        direct_outputs: Buttons,
    ) -> Controls {
        let physical = controls.buttons.union(controls.dpad.buttons());
        if state
            .active
            .is_some_and(|i| !physical.contains(self.layers[i].modifier))
        {
            state.active = None;
        }
        if state.active.is_none() {
            state.active = self.layers.iter().rposition(|layer| {
                physical.contains(layer.modifier) && !state.previous.contains(layer.modifier)
            });
        }
        state.previous = physical;
        let mut remaining = physical.difference(suppressed);
        let mut targets = direct_outputs;
        let active_layer = state.active.map(|i| &self.layers[i]);
        for &(source, target) in active_layer
            .into_iter()
            .flat_map(|layer| &layer.rules)
            .chain(&self.base)
        {
            if remaining.contains(source) {
                remaining.remove(source);
                targets.insert(target);
            }
        }
        let mapped = remaining.union(targets);
        controls.buttons = mapped.difference(DPad::BUTTONS);
        controls.dpad = DPad::from_buttons(mapped);
        controls
    }
}

pub fn dpad_button_by_name(name: &str) -> Option<Button> {
    Button::from_name(name).filter(|&button| DPad::BUTTONS.contains(button))
}

fn remap_button_by_name(name: &str) -> Option<Button> {
    Button::from_name(name)
}

fn parse_rule(spec: &str) -> Result<(Button, Button), &'static str> {
    let (source, target) = spec.split_once('=').ok_or("expected SOURCE=TARGET")?;
    let source = remap_button_by_name(source).ok_or("unknown source button")?;
    Ok((source, parse_target(target)?))
}

pub fn parse_target(name: &str) -> Result<Button, &'static str> {
    let target = remap_button_by_name(name).ok_or("unknown target button")?;
    if !DPad::BUTTONS.contains(target)
        && !OutputKind::Generic.buttons().any(|button| button == target)
    {
        return Err("target button is unavailable on the virtual gamepad");
    }
    Ok(target)
}

fn add_rule(rules: &mut Vec<(Button, Button)>, rule: (Button, Button)) -> Result<(), &'static str> {
    if rules.iter().any(|&(source, _)| source == rule.0) {
        return Err("source button already has a remap in this layer");
    }
    rules.push(rule);
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "profile", derive(serde::Deserialize))]
#[cfg_attr(feature = "profile", serde(rename_all = "kebab-case"))]
pub enum OutputKind {
    #[default]
    Generic,
    Xbox360,
    XboxSeries,
}

const BUTTONS: [Button; 11] = [
    Button::South,
    Button::East,
    Button::West,
    Button::North,
    Button::LeftShoulder,
    Button::RightShoulder,
    Button::Select,
    Button::Start,
    Button::LeftStick,
    Button::RightStick,
    Button::Home,
];

impl OutputKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Xbox360 => "xbox360",
            Self::XboxSeries => "xbox-series",
        }
    }

    #[inline]
    pub fn buttons(self) -> impl Iterator<Item = Button> {
        let extra: &[_] = match self {
            Self::Generic => &[Button::LeftTrigger, Button::RightTrigger],
            Self::Xbox360 => &[],
            Self::XboxSeries => &[Button::TouchpadClick],
        };
        BUTTONS.iter().chain(extra).copied()
    }

    pub const fn ranges(self) -> [AxisRange; GamepadAxis::ALL.len()] {
        let stick = AxisRange::centered(32_767);
        let trigger_max = match self {
            Self::Generic | Self::Xbox360 => 255,
            Self::XboxSeries => 1023,
        };
        let trigger = AxisRange::positive(trigger_max);
        let hat = AxisRange::centered(1);
        [stick, stick, stick, stick, trigger, trigger, hat, hat]
    }
}

impl std::str::FromStr for OutputKind {
    type Err = &'static str;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        [Self::Generic, Self::Xbox360, Self::XboxSeries]
            .into_iter()
            .find(|kind| kind.as_str() == name)
            .ok_or("expected generic, xbox360 or xbox-series")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GamepadAxis {
    LeftX,
    LeftY,
    RightX,
    RightY,
    LeftTrigger,
    RightTrigger,
    HatX,
    HatY,
}

impl GamepadAxis {
    pub const ALL: [Self; 8] = [
        Self::LeftX,
        Self::LeftY,
        Self::RightX,
        Self::RightY,
        Self::LeftTrigger,
        Self::RightTrigger,
        Self::HatX,
        Self::HatY,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxisRange {
    min: i32,
    max: i32,
}

impl AxisRange {
    pub const fn centered(max: i32) -> Self {
        Self { min: -max, max }
    }

    pub const fn positive(max: i32) -> Self {
        Self { min: 0, max }
    }

    pub const fn limits(self) -> (i32, i32) {
        (self.min, self.max)
    }
}

pub const MAX_GAMEPAD_EVENTS: usize = BUTTONS.len() + 2 + GamepadAxis::ALL.len();

#[derive(Clone, Copy)]
struct OutputState {
    buttons: Buttons,
    axes: [i32; GamepadAxis::ALL.len()],
}

pub struct GamepadChanges {
    kind: OutputKind,
    changed_buttons: Buttons,
    buttons: Buttons,
    previous_axes: [i32; GamepadAxis::ALL.len()],
    axes: [i32; GamepadAxis::ALL.len()],
}

impl GamepadChanges {
    #[inline]
    pub fn buttons(&self) -> impl Iterator<Item = (Button, bool)> + '_ {
        self.kind
            .buttons()
            .filter(|&button| self.changed_buttons.contains(button))
            .map(|button| (button, self.buttons.contains(button)))
    }

    #[inline]
    pub fn axes(&self) -> impl Iterator<Item = (GamepadAxis, i32)> + '_ {
        GamepadAxis::ALL
            .into_iter()
            .zip(self.previous_axes.into_iter().zip(self.axes))
            .filter_map(|(axis, (old, value))| (old != value).then_some((axis, value)))
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.changed_buttons == Buttons::default() && self.previous_axes == self.axes
    }
}

/// Remembers the last successful output, including quantized axes.
pub struct GamepadOutput {
    kind: OutputKind,
    supported: Buttons,
    scales: [f32; GamepadAxis::ALL.len()],
    current: OutputState,
}

impl GamepadOutput {
    pub fn new(kind: OutputKind, ranges: [AxisRange; GamepadAxis::ALL.len()]) -> Self {
        let mut supported = Buttons::default();
        for button in kind.buttons() {
            supported.insert(button);
        }
        Self {
            kind,
            supported,
            scales: ranges.map(|range| range.max as f32),
            current: OutputState {
                buttons: Buttons::default(),
                axes: [0; GamepadAxis::ALL.len()],
            },
        }
    }

    /// A failed submission leaves the committed state intact. An adapter
    /// with an uncertain device state should release or recreate its device.
    #[inline]
    pub fn publish<E>(
        &mut self,
        controls: Controls,
        emit: impl FnOnce(&GamepadChanges) -> Result<(), E>,
    ) -> Result<bool, E> {
        let next = OutputState {
            buttons: self
                .supported
                .difference(self.supported.difference(controls.buttons)),
            axes: self.axes(controls),
        };
        let changes = GamepadChanges {
            kind: self.kind,
            changed_buttons: next.buttons.symmetric_difference(self.current.buttons),
            buttons: next.buttons,
            previous_axes: self.current.axes,
            axes: next.axes,
        };
        let changed = !changes.is_empty();
        if changed {
            emit(&changes)?;
        }
        self.current = next;
        Ok(changed)
    }

    #[inline]
    fn axes(&self, controls: Controls) -> [i32; GamepadAxis::ALL.len()] {
        let hat = controls.dpad.buttons();
        let hat_x =
            i8::from(hat.contains(Button::DPadRight)) - i8::from(hat.contains(Button::DPadLeft));
        let hat_y =
            i8::from(hat.contains(Button::DPadDown)) - i8::from(hat.contains(Button::DPadUp));
        let axes = [
            controls.left_stick.x,
            controls.left_stick.y,
            controls.right_stick.x,
            controls.right_stick.y,
            controls.left_trigger,
            controls.right_trigger,
            f32::from(hat_x),
            f32::from(hat_y),
        ];
        std::array::from_fn(|index| (axes[index] * self.scales[index]).round() as i32)
    }
}

/// Resolved routes for one virtual gamepad. These borrow the active configuration.
pub struct GamepadMapping<'a> {
    pub stick_curve: Option<StickCurve>,
    pub flick_stick: Option<StickSide>,
    pub suppressed: Buttons,
    pub touchpad: Option<&'a Grid>,
    pub trigger_stages: Option<[TriggerStages; 2]>,
    pub trigger_full_travel: [Option<u8>; 2],
    pub trigger_analog_routes: [AnalogTriggerRoute; 2],
    pub remaps: &'a ButtonRemaps,
}

impl GamepadMapping<'_> {
    #[inline]
    pub fn apply(
        &self,
        physical: Controls,
        contacts: [Option<TouchContact>; 2],
        layer_state: &mut RemapState,
        touch_click: &mut ClickState,
    ) -> Controls {
        let mut controls = self
            .stick_curve
            .map_or(physical, |curve| curve.apply(physical));
        if let Some(side) = self.flick_stick {
            *side.select(&mut controls.left_stick, &mut controls.right_stick) = Default::default();
        }
        let mut suppressed = self.suppressed;
        if suppressed.contains(Button::LeftTrigger) {
            controls.left_trigger = 0.0;
        }
        if suppressed.contains(Button::RightTrigger) {
            controls.right_trigger = 0.0;
        }
        let touch_buttons = self
            .touchpad
            .as_ref()
            .map_or_else(Default::default, |grid| {
                use joyshockrust_core::Button::TouchpadClick;
                let suppress_click =
                    suppressed.contains(TouchpadClick) && controls.buttons.contains(TouchpadClick);
                if suppress_click {
                    controls.buttons.remove(TouchpadClick);
                }
                let buttons = grid.map(&mut controls.buttons, contacts, touch_click);
                if suppress_click {
                    controls.buttons.insert(TouchpadClick);
                }
                buttons
            });
        let trigger_buttons =
            layer_state.trigger_outputs(&controls, self.trigger_stages, &mut suppressed);
        route_trigger_axes(
            &mut controls,
            self.trigger_full_travel,
            self.trigger_analog_routes,
            &mut suppressed,
        );
        self.remaps.apply_filtered(
            controls,
            layer_state,
            suppressed,
            touch_buttons.union(trigger_buttons),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joyshockrust_core::Stick;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum GamepadEvent {
        Button(Button, bool),
        Axis(GamepadAxis, i32),
    }

    fn events(changes: &GamepadChanges) -> impl Iterator<Item = GamepadEvent> + '_ {
        changes
            .buttons()
            .map(|(button, pressed)| GamepadEvent::Button(button, pressed))
            .chain(
                changes
                    .axes()
                    .map(|(axis, value)| GamepadEvent::Axis(axis, value)),
            )
    }

    #[test]
    fn press_and_axes_repeat_after_failed_submission_then_release_to_neutral() {
        for (kind, trigger) in [(OutputKind::Generic, 128), (OutputKind::XboxSeries, 512)] {
            let mut state = GamepadOutput::new(kind, kind.ranges());
            let mut controls = Controls {
                left_stick: Stick { x: -1.0, y: 0.0 },
                left_trigger: 0.5,
                dpad: DPad::UpRight,
                ..Controls::default()
            };
            controls.buttons.insert(Button::South);

            let expected = [
                GamepadEvent::Button(Button::South, true),
                GamepadEvent::Axis(GamepadAxis::LeftX, -32_767),
                GamepadEvent::Axis(GamepadAxis::LeftTrigger, trigger),
                GamepadEvent::Axis(GamepadAxis::HatX, 1),
                GamepadEvent::Axis(GamepadAxis::HatY, -1),
            ];
            assert_eq!(
                state.publish(controls, |changes| {
                    assert!(events(changes).eq(expected));
                    Err("write failed")
                }),
                Err("write failed")
            );
            assert_eq!(
                state.publish::<()>(controls, |changes| {
                    assert!(events(changes).eq(expected));
                    Ok(())
                }),
                Ok(true)
            );
            // Higher precision input that still quantizes to the same values.
            controls.left_trigger += 0.00001;
            assert_eq!(
                state.publish::<()>(controls, |_| panic!("duplicate output")),
                Ok(false)
            );
            assert_eq!(
                state.publish::<()>(Controls::default(), |changes| {
                    assert!(events(changes).eq([
                        GamepadEvent::Button(Button::South, false),
                        GamepadEvent::Axis(GamepadAxis::LeftX, 0),
                        GamepadEvent::Axis(GamepadAxis::LeftTrigger, 0),
                        GamepadEvent::Axis(GamepadAxis::HatX, 0),
                        GamepadEvent::Axis(GamepadAxis::HatY, 0),
                    ]));
                    Ok(())
                }),
                Ok(true)
            );
        }
    }
}
