use std::io;

use evdev::{
    AbsInfo, AbsoluteAxisCode as Axis, AttributeSet, BusType, EventType, InputEvent, InputId,
    KeyCode as Key, UinputAbsSetup, uinput::VirtualDevice,
};
use joyshockrust_core::{Button, Controls};
use joyshockrust_mapping::gamepad::{GamepadAxis, GamepadOutput, MAX_GAMEPAD_EVENTS};

pub use joyshockrust_mapping::gamepad::{ButtonRemaps, GamepadMapping, OutputKind, RemapState};
pub use joyshockrust_motion::stick_response::StickCurve;

fn key_code(button: Button) -> Key {
    match button {
        Button::South => Key::BTN_SOUTH,
        Button::East => Key::BTN_EAST,
        // SDL names the Linux BTN_X/BTN_Y aliases by their position.
        Button::West => Key::BTN_NORTH,
        Button::North => Key::BTN_WEST,
        Button::LeftShoulder => Key::BTN_TL,
        Button::RightShoulder => Key::BTN_TR,
        Button::Select => Key::BTN_SELECT,
        Button::Start => Key::BTN_START,
        Button::LeftStick => Key::BTN_THUMBL,
        Button::RightStick => Key::BTN_THUMBR,
        Button::Home => Key::BTN_MODE,
        Button::LeftTrigger => Key::BTN_TL2,
        Button::RightTrigger => Key::BTN_TR2,
        Button::TouchpadClick => Key::KEY_RECORD,
        _ => unreachable!("unsupported virtual gamepad button: {button:?}"),
    }
}

fn axis_code(axis: GamepadAxis) -> Axis {
    match axis {
        GamepadAxis::LeftX => Axis::ABS_X,
        GamepadAxis::LeftY => Axis::ABS_Y,
        GamepadAxis::RightX => Axis::ABS_RX,
        GamepadAxis::RightY => Axis::ABS_RY,
        GamepadAxis::LeftTrigger => Axis::ABS_Z,
        GamepadAxis::RightTrigger => Axis::ABS_RZ,
        GamepadAxis::HatX => Axis::ABS_HAT0X,
        GamepadAxis::HatY => Axis::ABS_HAT0Y,
    }
}

fn identity(kind: OutputKind) -> (&'static str, InputId) {
    match kind {
        OutputKind::Generic => (
            "JoyShockRust Virtual Gamepad",
            InputId::new(BusType::BUS_VIRTUAL, 0, 0, 1),
        ),
        OutputKind::Xbox360 => (
            "JoyShockRust Virtual Xbox 360 Gamepad",
            InputId::new(BusType::BUS_USB, 0x045e, 0x028e, 0x0114),
        ),
        OutputKind::XboxSeries => (
            "JoyShockRust Virtual Xbox Series X|S Gamepad",
            InputId::new(BusType::BUS_USB, 0x045e, 0x0b12, 0x0507),
        ),
    }
}

pub fn open(kind: OutputKind, rumble_enabled: bool) -> io::Result<VirtualDevice> {
    let (name, id) = identity(kind);
    let keys = kind.buttons().map(key_code).collect::<AttributeSet<Key>>();
    let mut builder = VirtualDevice::builder()?
        .name(name)
        .input_id(id)
        .with_keys(&keys)?;
    for (axis, range) in GamepadAxis::ALL.into_iter().zip(kind.ranges()) {
        let (min, max) = range.limits();
        builder = builder.with_absolute_axis(&UinputAbsSetup::new(
            axis_code(axis),
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

pub struct GamepadState(GamepadOutput);

impl GamepadState {
    pub fn new(kind: OutputKind) -> Self {
        Self(GamepadOutput::new(kind, kind.ranges()))
    }

    pub fn publish(
        &mut self,
        next: Controls,
        mut emit: impl FnMut(&[InputEvent]) -> io::Result<()>,
    ) -> io::Result<bool> {
        self.0.publish(next, |changes| {
            let mut batch = [InputEvent::new(0, 0, 0); MAX_GAMEPAD_EVENTS];
            let mut count = 0;
            for (button, pressed) in changes.buttons() {
                batch[count] =
                    InputEvent::new(EventType::KEY.0, key_code(button).0, i32::from(pressed));
                count += 1;
            }
            for (axis, value) in changes.axes() {
                batch[count] = InputEvent::new(EventType::ABSOLUTE.0, axis_code(axis).0, value);
                count += 1;
            }
            emit(&batch[..count])
        })
    }
}

#[cfg(test)]
impl Default for GamepadState {
    fn default() -> Self {
        Self::new(OutputKind::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joyshockrust_core::{DPad, Stick};
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
        GamepadState::default()
            .publish(result, |events| {
                assert!(
                    events
                        .iter()
                        .any(|event| event.code() == Axis::ABS_RX.0 && event.value() == -8192)
                );
                Ok(())
            })
            .unwrap();
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
        let (name, id) = identity(kind);
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
        let max = if kind == OutputKind::XboxSeries {
            1023
        } else {
            255
        };
        assert_eq!(trigger_axes, [(0, max); 2]);
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
                (EventType::ABSOLUTE, Axis::ABS_X.0, 32_767),
                (EventType::ABSOLUTE, Axis::ABS_Z.0, 255),
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
