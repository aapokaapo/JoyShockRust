use std::io;

use evdev::{
    AbsInfo, AbsoluteAxisCode as Axis, AttributeSet, BusType, EventType, InputEvent, InputId,
    KeyCode as Key, UinputAbsSetup, uinput::VirtualDevice,
};
use joyshockrust_core::{Button, Buttons, Controls, DPad, Stick};

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
        let modifier =
            super::gyro_gate::button_by_name(modifier).ok_or("unknown remap layer button")?;
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

    pub fn apply(&self, mut controls: Controls, state: &mut RemapState) -> Controls {
        let physical = controls.buttons;
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
        let mut remaining = physical;
        let mut targets = Buttons::default();
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
        controls.buttons = remaining.union(targets);
        controls
    }
}

fn parse_rule(spec: &str) -> Result<(Button, Button), &'static str> {
    let (source, target) = spec.split_once('=').ok_or("expected SOURCE=TARGET")?;
    let source = super::gyro_gate::button_by_name(source).ok_or("unknown source button")?;
    Ok((source, parse_target(target)?))
}

pub fn parse_target(name: &str) -> Result<Button, &'static str> {
    let target = super::gyro_gate::button_by_name(name).ok_or("unknown target button")?;
    if !OutputKind::Generic
        .buttons()
        .any(|(button, _)| button == target)
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

#[derive(Clone, Copy)]
pub struct StickCurve([f32; 2]);

impl StickCurve {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        Self::new(value.parse().map_err(|_| "invalid stick curve exponent")?)
    }

    pub fn new(exponent: f32) -> Result<Self, &'static str> {
        Self::validate(exponent)?;
        Ok(Self([exponent; 2]))
    }

    fn validate(exponent: f32) -> Result<(), &'static str> {
        if !(0.25..=4.0).contains(&exponent) {
            return Err("stick curve exponent must be between 0.25 and 4");
        }
        Ok(())
    }

    pub fn from_profile(
        shared: Option<f32>,
        left: Option<f32>,
        right: Option<f32>,
    ) -> Result<Option<Self>, &'static str> {
        [shared, left, right]
            .into_iter()
            .flatten()
            .try_for_each(Self::validate)?;
        let fallback = shared.unwrap_or(1.0);
        Ok((shared.is_some() || left.is_some() || right.is_some())
            .then_some(Self([left.unwrap_or(fallback), right.unwrap_or(fallback)])))
    }

    pub fn apply(self, mut controls: Controls) -> Controls {
        controls.left_stick = Self::shape(controls.left_stick, self.0[0]);
        controls.right_stick = Self::shape(controls.right_stick, self.0[1]);
        controls
    }

    fn shape(stick: Stick, exponent: f32) -> Stick {
        let radius_squared = stick.x * stick.x + stick.y * stick.y;
        // Keep center and full travel unchanged, including square corners.
        if exponent == 1.0 || radius_squared == 0.0 || radius_squared >= 1.0 {
            return stick;
        }
        let scale = radius_squared.powf((exponent - 1.0) * 0.5);
        Stick {
            x: stick.x * scale,
            y: stick.y * scale,
        }
    }
}

const BUTTONS: [(Button, Key); 11] = [
    (Button::South, Key::BTN_SOUTH),
    (Button::East, Key::BTN_EAST),
    // SDL maps generic gamepads by the Linux BTN_X/BTN_Y aliases rather
    // than using the Sony-specific positional mapping.
    (Button::West, Key::BTN_NORTH), // BTN_X
    (Button::North, Key::BTN_WEST), // BTN_Y
    (Button::LeftShoulder, Key::BTN_TL),
    (Button::RightShoulder, Key::BTN_TR),
    (Button::Select, Key::BTN_SELECT),
    (Button::Start, Key::BTN_START),
    (Button::LeftStick, Key::BTN_THUMBL),
    (Button::RightStick, Key::BTN_THUMBR),
    (Button::Home, Key::BTN_MODE),
];
const DIGITAL_TRIGGERS: [(Button, Key); 2] = [
    (Button::LeftTrigger, Key::BTN_TL2),
    (Button::RightTrigger, Key::BTN_TR2),
];

const STICK_SCALE: i32 = 32_767;
const TRIGGER_SCALE: i32 = 255;
const SERIES_TRIGGER_SCALE: i32 = 1023;
const AXES: [(Axis, i32, i32); 8] = [
    (Axis::ABS_X, -STICK_SCALE, STICK_SCALE),
    (Axis::ABS_Y, -STICK_SCALE, STICK_SCALE),
    (Axis::ABS_RX, -STICK_SCALE, STICK_SCALE),
    (Axis::ABS_RY, -STICK_SCALE, STICK_SCALE),
    (Axis::ABS_Z, 0, TRIGGER_SCALE),
    (Axis::ABS_RZ, 0, TRIGGER_SCALE),
    (Axis::ABS_HAT0X, -1, 1),
    (Axis::ABS_HAT0Y, -1, 1),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputKind {
    #[default]
    Generic,
    Xbox360,
    XboxSeries,
}

impl OutputKind {
    fn identity(self) -> (&'static str, InputId) {
        match self {
            Self::Generic => (
                "JoyShockRust Virtual Gamepad",
                InputId::new(BusType::BUS_VIRTUAL, 0, 0, 1),
            ),
            Self::Xbox360 => (
                "JoyShockRust Virtual Xbox 360 Gamepad",
                InputId::new(BusType::BUS_USB, 0x045e, 0x028e, 0x0114),
            ),
            Self::XboxSeries => (
                "JoyShockRust Virtual Xbox Series X|S Gamepad",
                InputId::new(BusType::BUS_USB, 0x045e, 0x0b12, 0x0507),
            ),
        }
    }

    fn buttons(self) -> impl Iterator<Item = (Button, Key)> {
        let extra: &[_] = match self {
            Self::Generic => &DIGITAL_TRIGGERS,
            Self::Xbox360 => &[],
            Self::XboxSeries => &[(Button::TouchpadClick, Key::KEY_RECORD)],
        };
        BUTTONS.iter().chain(extra).copied()
    }

    fn trigger_scale(self) -> i32 {
        match self {
            Self::Generic | Self::Xbox360 => TRIGGER_SCALE,
            Self::XboxSeries => SERIES_TRIGGER_SCALE,
        }
    }
}

pub fn open(kind: OutputKind, rumble_enabled: bool) -> io::Result<VirtualDevice> {
    let (name, id) = kind.identity();
    let keys = kind
        .buttons()
        .map(|(_, code)| code)
        .collect::<AttributeSet<Key>>();
    let mut builder = VirtualDevice::builder()?
        .name(name)
        .input_id(id)
        .with_keys(&keys)?;
    for (code, min, max) in AXES {
        let max = match code {
            Axis::ABS_Z | Axis::ABS_RZ => kind.trigger_scale(),
            _ => max,
        };
        builder = builder.with_absolute_axis(&UinputAbsSetup::new(
            code,
            AbsInfo::new(0, min, max, 0, 0, 0),
        ))?;
    }
    if rumble_enabled {
        let effects = AttributeSet::from_iter([evdev::FFEffectCode::FF_RUMBLE]);
        builder = builder
            .with_ff(&effects)?
            .with_ff_effects_max(super::rumble::EFFECT_SLOTS as u32);
    }
    builder.build()
}

#[derive(Default)]
pub struct GamepadState {
    kind: OutputKind,
    current: Controls,
}

impl GamepadState {
    pub fn new(kind: OutputKind) -> Self {
        Self {
            kind,
            current: Controls::default(),
        }
    }

    pub fn publish(
        &mut self,
        next: Controls,
        mut emit: impl FnMut(&[InputEvent]) -> io::Result<()>,
    ) -> io::Result<bool> {
        let mut changes =
            [InputEvent::new(0, 0, 0); BUTTONS.len() + DIGITAL_TRIGGERS.len() + AXES.len()];
        let mut count = 0;
        for (button, code) in self.kind.buttons() {
            let pressed = next.buttons.contains(button);
            if pressed != self.current.buttons.contains(button) {
                changes[count] = InputEvent::new(EventType::KEY.0, code.0, i32::from(pressed));
                count += 1;
            }
        }
        let trigger_scale = self.kind.trigger_scale();
        for (((code, _, _), old), new) in AXES
            .into_iter()
            .zip(axes(self.current, trigger_scale))
            .zip(axes(next, trigger_scale))
        {
            if old != new {
                changes[count] = InputEvent::new(EventType::ABSOLUTE.0, code.0, new);
                count += 1;
            }
        }
        if count > 0 {
            emit(&changes[..count])?;
        }
        self.current = next;
        Ok(count > 0)
    }
}

fn axes(controls: Controls, trigger_scale: i32) -> [i32; AXES.len()] {
    let (x, y) = match controls.dpad {
        DPad::Neutral => (0, 0),
        DPad::Up => (0, -1),
        DPad::UpRight => (1, -1),
        DPad::Right => (1, 0),
        DPad::DownRight => (1, 1),
        DPad::Down => (0, 1),
        DPad::DownLeft => (-1, 1),
        DPad::Left => (-1, 0),
        DPad::UpLeft => (-1, -1),
    };
    let stick = |axis: f32| (axis * STICK_SCALE as f32).round() as i32;
    let trigger = |axis: f32| (axis * trigger_scale as f32).round() as i32;
    [
        stick(controls.left_stick.x),
        stick(controls.left_stick.y),
        stick(controls.right_stick.x),
        stick(controls.right_stick.y),
        trigger(controls.left_trigger),
        trigger(controls.right_trigger),
        x,
        y,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use joyshockrust_input::{DeviceModel, Transport, parse_report};

    #[test]
    fn usb_square_and_triangle_emit_generic_gamepad_x_and_y() {
        let mut state = GamepadState::default();
        let mut events = Vec::new();
        let mut frame = [0; 64];
        frame[0] = 1;
        frame[1..=4].fill(128);
        for buttons in [0x18, 0x08, 0x88, 0x08] {
            frame[8] = buttons;
            let report =
                parse_report(DeviceModel::DualSenseEdge, Transport::Usb, &frame, None).unwrap();
            state
                .publish(report.controls, |batch| {
                    events.extend(batch.iter().map(|event| (event.code(), event.value())));
                    Ok(())
                })
                .unwrap();
        }
        // Explicit Linux BTN_X and BTN_Y values guard against alias confusion.
        assert_eq!(events, [(0x133, 1), (0x133, 0), (0x134, 1), (0x134, 0)]);
    }

    #[test]
    fn active_layer_stays_selected_until_its_modifier_is_released() {
        let mut remaps = ButtonRemaps::default();
        remaps.add("cross=triangle").unwrap();
        remaps.add("options=create").unwrap();
        remaps.add_layer("l1:cross=circle").unwrap();
        remaps.add_layer("r1:cross=square").unwrap();
        remaps.add_layer("l1:l3=r3").unwrap();
        assert!(remaps.add_layer("l1:cross=square").is_err());

        let mut state = RemapState::default();
        for (left, right, expected, left_layer_active) in [
            (false, false, Button::North, false),
            (true, false, Button::East, true),
            (true, true, Button::East, true),
            (false, true, Button::North, false),
            (false, false, Button::North, false),
            (false, true, Button::West, false),
            (true, true, Button::West, false),
        ] {
            let mut physical = Controls::default();
            for button in [Button::South, Button::Start, Button::LeftStick] {
                physical.buttons.insert(button);
            }
            if left {
                physical.buttons.insert(Button::LeftShoulder);
            }
            if right {
                physical.buttons.insert(Button::RightShoulder);
            }
            let mapped = remaps.apply(physical, &mut state).buttons;
            for face in [Button::South, Button::East, Button::West, Button::North] {
                assert_eq!(mapped.contains(face), face == expected);
            }
            assert!(mapped.contains(Button::Select));
            assert_eq!(mapped.contains(Button::RightStick), left_layer_active);
            assert_eq!(mapped.contains(Button::LeftStick), !left_layer_active);
            assert_eq!(mapped.contains(Button::RightShoulder), right);
        }

        remaps.add_layer("l1:r1=l2").unwrap();
        let mut state = RemapState::default();
        let mut physical = Controls::default();
        physical.buttons.insert(Button::LeftShoulder);
        remaps.apply(physical, &mut state);
        physical.buttons.insert(Button::RightShoulder);
        let buttons = remaps.apply(physical, &mut state).buttons;
        assert!(buttons.contains(Button::LeftTrigger));
        assert!(!buttons.contains(Button::RightShoulder));
    }

    #[test]
    fn layer_switch_releases_and_rebinds_held_buttons_in_one_batch() {
        let mut remaps = ButtonRemaps::default();
        remaps.add_layer("l1:cross=circle").unwrap();

        let mut base = Controls::default();
        base.buttons.insert(Button::South);
        let mut layered = base;
        layered.buttons.insert(Button::LeftShoulder);
        let mut gamepad = GamepadState::default();
        let mut layer = RemapState::default();
        let mut batches = Vec::new();
        for controls in [base, layered, base, Controls::default()] {
            gamepad
                .publish(remaps.apply(controls, &mut layer), |events| {
                    batches.push(
                        events
                            .iter()
                            .map(|e| (e.code(), e.value()))
                            .collect::<Vec<_>>(),
                    );
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(
            batches,
            [
                vec![(Key::BTN_SOUTH.0, 1)],
                vec![
                    (Key::BTN_SOUTH.0, 0),
                    (Key::BTN_EAST.0, 1),
                    (Key::BTN_TL.0, 1)
                ],
                vec![
                    (Key::BTN_SOUTH.0, 1),
                    (Key::BTN_EAST.0, 0),
                    (Key::BTN_TL.0, 0)
                ],
                vec![(Key::BTN_SOUTH.0, 0)],
            ]
        );
    }

    #[test]
    fn remaps_swap_buttons_without_chaining() {
        let mut remaps = ButtonRemaps::default();
        remaps.add("cross=circle").unwrap();
        remaps.add("circle=cross").unwrap();
        let mut raw = Controls::default();
        raw.buttons.insert(Button::South);
        let mut layer = RemapState::default();
        let virtual_buttons = remaps.apply(raw, &mut layer).buttons;
        assert!(!virtual_buttons.contains(Button::South));
        assert!(virtual_buttons.contains(Button::East));
        assert_eq!(
            remaps.apply(Controls::default(), &mut layer),
            Controls::default()
        );
        assert!(remaps.add("cross=r1").is_err());
        assert!(remaps.add("l1=fn-left").is_err());
    }

    #[test]
    fn remap_keeps_virtual_button_pressed_until_all_sources_release() {
        let mut remaps = ButtonRemaps::default();
        remaps.add("cross=circle").unwrap();
        let mut state = GamepadState::default();
        let mut layer = RemapState::default();
        let mut changes = Vec::new();
        let mut emit = |events: &[InputEvent]| {
            changes.extend(events.iter().map(|event| (event.code(), event.value())));
            Ok(())
        };
        let mut raw = Controls::default();
        raw.buttons.insert(Button::South);
        state
            .publish(remaps.apply(raw, &mut layer), &mut emit)
            .unwrap();
        raw.buttons.insert(Button::East);
        state
            .publish(remaps.apply(raw, &mut layer), &mut emit)
            .unwrap();
        raw.buttons.remove(Button::South);
        state
            .publish(remaps.apply(raw, &mut layer), &mut emit)
            .unwrap();
        state.publish(Controls::default(), &mut emit).unwrap();
        assert_eq!(changes, [(Key::BTN_EAST.0, 1), (Key::BTN_EAST.0, 0)]);
    }

    #[test]
    fn stick_curve_preserves_direction_and_shapes_both_sticks() {
        let controls = Controls {
            left_stick: Stick { x: 0.3, y: -0.4 },
            right_stick: Stick { x: -0.5, y: 0.0 },
            left_trigger: 0.5,
            dpad: DPad::Up,
            ..Controls::default()
        };
        let result = StickCurve::parse("2").unwrap().apply(controls);
        assert!((result.left_stick.x - 0.15).abs() < 1e-6);
        assert!((result.left_stick.y + 0.2).abs() < 1e-6);
        assert_eq!(axes(result, TRIGGER_SCALE)[2], -8192);
        assert_eq!(result.left_trigger, controls.left_trigger);
        assert_eq!(result.dpad, controls.dpad);
        assert_eq!(StickCurve::parse("1").unwrap().apply(controls), controls);
    }

    #[test]
    fn stick_curve_accepts_center_and_full_travel_and_rejects_invalid_exponents() {
        let controls = Controls {
            left_stick: Stick { x: 1.0, y: -1.0 },
            ..Controls::default()
        };
        let curve = StickCurve::parse("0.5").unwrap();
        assert_eq!(curve.apply(controls), controls);
        assert!(
            curve
                .apply(Controls {
                    left_stick: Stick { x: 0.25, y: 0.0 },
                    ..Controls::default()
                })
                .left_stick
                .x
                > 0.25
        );
        for invalid in ["NaN", "inf", "0", "-1", "4.1"] {
            assert!(StickCurve::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    #[ignore = "requires writable /dev/uinput and readable virtual evdev nodes"]
    fn uinput_cross_press_and_release() {
        assert_uinput_gamepad(OutputKind::Generic, false);
    }

    #[test]
    #[ignore = "requires writable /dev/uinput and readable virtual evdev nodes"]
    fn uinput_xbox360_identity_and_cross_press() {
        assert_uinput_gamepad(OutputKind::Xbox360, false);
    }

    #[test]
    #[ignore = "requires writable /dev/uinput and readable virtual evdev nodes"]
    fn uinput_xbox_series_identity_and_cross_press() {
        assert_uinput_gamepad(OutputKind::XboxSeries, false);
    }

    #[test]
    #[ignore = "requires writable /dev/uinput and readable virtual evdev nodes"]
    fn uinput_rumble_gamepad_advertises_ff_rumble() {
        assert_uinput_gamepad(OutputKind::XboxSeries, true);
    }

    fn assert_uinput_gamepad(kind: OutputKind, rumble_enabled: bool) {
        use std::{
            os::fd::AsFd,
            thread,
            time::{Duration, Instant},
        };

        use evdev::{Device, SynchronizationCode};
        use nix::poll::{PollFd, PollFlags, poll};

        let mut virtual_device = open(kind, rumble_enabled).expect("create the virtual gamepad");
        let node = virtual_device
            .enumerate_dev_nodes_blocking()
            .expect("enumerate the virtual gamepad's event nodes")
            .next()
            .expect("virtual gamepad event node")
            .expect("read the event node path");
        // The kernel can expose the event node before udev applies its ACL.
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut observer = loop {
            match Device::open(&node) {
                Ok(observer) => break observer,
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound
                    ) && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(err) => panic!(
                    "could not read virtual gamepad {}: {err}. See the uinput permissions in TESTING_AND_PERFORMANCE.md",
                    node.display()
                ),
            }
        };
        let (name, id) = kind.identity();
        assert_eq!(observer.name(), Some(name));
        assert_eq!(observer.input_id(), id);
        assert_eq!(
            observer
                .supported_ff()
                .is_some_and(|effects| effects.contains(evdev::FFEffectCode::FF_RUMBLE)),
            rumble_enabled,
        );
        let keys = observer.supported_keys().unwrap();
        assert_eq!(
            keys.contains(Key::BTN_TL2),
            matches!(kind, OutputKind::Generic)
        );
        assert_eq!(
            keys.contains(Key::KEY_RECORD),
            matches!(kind, OutputKind::XboxSeries)
        );
        let trigger_axes: Vec<_> = observer
            .get_absinfo()
            .unwrap()
            .filter(|(code, _)| matches!(*code, Axis::ABS_Z | Axis::ABS_RZ))
            .map(|(_, info)| (info.minimum(), info.maximum()))
            .collect();
        assert_eq!(trigger_axes, [(0, kind.trigger_scale()); 2]);
        observer.set_nonblocking(true).unwrap();

        let mut report = [0; 64];
        report[0] = 1;
        report[1..=4].fill(128);
        report[8] = 0x28;
        let pressed = parse_report(DeviceModel::DualSenseEdge, Transport::Usb, &report, None)
            .unwrap()
            .controls;
        let mut state = GamepadState::new(kind);
        state
            .publish(pressed, |events| virtual_device.emit(events))
            .unwrap();
        state
            .publish(Controls::default(), |events| virtual_device.emit(events))
            .unwrap();

        let mut fds = [PollFd::new(observer.as_fd(), PollFlags::POLLIN)];
        assert!(poll(&mut fds, 1_000_u16).unwrap() > 0, "no uinput events");
        let events: Vec<_> = observer
            .fetch_events()
            .unwrap()
            .map(|event| (event.event_type(), event.code(), event.value()))
            .collect();
        let sync = (
            EventType::SYNCHRONIZATION,
            SynchronizationCode::SYN_REPORT.0,
            0,
        );
        assert_eq!(
            events,
            [
                (EventType::KEY, Key::BTN_SOUTH.0, 1),
                sync,
                (EventType::KEY, Key::BTN_SOUTH.0, 0),
                sync,
            ],
        );
    }

    #[test]
    fn xbox360_output_keeps_face_button_order_and_emits_analog_trigger_only() {
        let mut frame = [0; 64];
        frame[..10].copy_from_slice(&[1, 128, 128, 128, 128, 255, 0, 0, 0x98, 0x04]);
        let controls = parse_report(DeviceModel::DualSenseEdge, Transport::Usb, &frame, None)
            .unwrap()
            .controls;
        let mut gamepad = GamepadState::new(OutputKind::Xbox360);
        let mut events = Vec::new();
        gamepad
            .publish(controls, |batch| {
                events.extend(
                    batch
                        .iter()
                        .map(|event| (event.event_type(), event.code(), event.value())),
                );
                Ok(())
            })
            .unwrap();
        assert_eq!(
            events,
            [
                (EventType::KEY, Key::BTN_NORTH.0, 1),
                (EventType::KEY, Key::BTN_WEST.0, 1),
                (EventType::ABSOLUTE, Axis::ABS_Z.0, 255),
            ]
        );
    }

    #[test]
    fn xbox_series_reports_share_and_scales_both_triggers() {
        let mut gamepad = GamepadState::new(OutputKind::XboxSeries);
        let mut frame = [0; 64];
        frame[..11].copy_from_slice(&[1, 128, 128, 128, 128, 255, 128, 0, 8, 0x0c, 0x02]);
        let controls = parse_report(DeviceModel::DualSenseEdge, Transport::Usb, &frame, None)
            .unwrap()
            .controls;
        let mut batches = Vec::new();
        let mut capture = |events: &[InputEvent]| {
            batches.push(
                events
                    .iter()
                    .map(|event| (event.event_type(), event.code(), event.value()))
                    .collect::<Vec<_>>(),
            );
            Ok(())
        };
        gamepad.publish(controls, &mut capture).unwrap();
        gamepad.publish(Controls::default(), &mut capture).unwrap();
        assert_eq!(
            batches,
            [
                vec![
                    (EventType::KEY, Key::KEY_RECORD.0, 1),
                    (EventType::ABSOLUTE, Axis::ABS_Z.0, 1023),
                    (EventType::ABSOLUTE, Axis::ABS_RZ.0, 514),
                ],
                vec![
                    (EventType::KEY, Key::KEY_RECORD.0, 0),
                    (EventType::ABSOLUTE, Axis::ABS_Z.0, 0),
                    (EventType::ABSOLUTE, Axis::ABS_RZ.0, 0),
                ],
            ]
        );
    }

    #[test]
    fn usb_report_changes_are_batched_and_released() {
        let mut report = [0; 64];
        report[..10].copy_from_slice(&[1, 255, 128, 128, 128, 255, 0, 0, 0x21, 0x04]);
        let controls = parse_report(DeviceModel::DualSenseEdge, Transport::Usb, &report, None)
            .unwrap()
            .controls;
        let mut gamepad = GamepadState::default();
        let mut batches = Vec::new();
        let mut capture = |events: &[InputEvent]| {
            batches.push(
                events
                    .iter()
                    .map(|e| (e.event_type(), e.code(), e.value()))
                    .collect::<Vec<_>>(),
            );
            Ok(())
        };
        assert!(gamepad.publish(controls, &mut capture).unwrap());
        assert!(!gamepad.publish(controls, &mut capture).unwrap());
        assert!(gamepad.publish(Controls::default(), &mut capture).unwrap());
        assert_eq!(batches.len(), 2);
        assert_eq!(
            batches[0],
            [
                (EventType::KEY, Key::BTN_SOUTH.0, 1),
                (EventType::KEY, Key::BTN_TL2.0, 1),
                (EventType::ABSOLUTE, Axis::ABS_X.0, STICK_SCALE),
                (EventType::ABSOLUTE, Axis::ABS_Z.0, TRIGGER_SCALE),
                (EventType::ABSOLUTE, Axis::ABS_HAT0X.0, 1),
                (EventType::ABSOLUTE, Axis::ABS_HAT0Y.0, -1),
            ]
        );
        assert_eq!(
            batches[1],
            batches[0]
                .iter()
                .map(|&(kind, code, _)| (kind, code, 0))
                .collect::<Vec<_>>()
        );
    }
}
