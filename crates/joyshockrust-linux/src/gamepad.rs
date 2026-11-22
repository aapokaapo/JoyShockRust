use std::io;

use evdev::{
    AbsInfo, AbsoluteAxisCode as Axis, AttributeSet, BusType, EventType, InputEvent, InputId,
    KeyCode as Key, UinputAbsSetup, uinput::VirtualDevice,
};
use joyshockrust_core::{Button, Controls, DPad};

const BUTTONS: [(Button, Key); 13] = [
    (Button::South, Key::BTN_SOUTH),
    (Button::East, Key::BTN_EAST),
    (Button::West, Key::BTN_WEST),
    (Button::North, Key::BTN_NORTH),
    (Button::LeftShoulder, Key::BTN_TL),
    (Button::RightShoulder, Key::BTN_TR),
    (Button::LeftTrigger, Key::BTN_TL2),
    (Button::RightTrigger, Key::BTN_TR2),
    (Button::Select, Key::BTN_SELECT),
    (Button::Start, Key::BTN_START),
    (Button::LeftStick, Key::BTN_THUMBL),
    (Button::RightStick, Key::BTN_THUMBR),
    (Button::Home, Key::BTN_MODE),
];

const STICK_SCALE: i32 = 32_767;
const TRIGGER_SCALE: i32 = 255;
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

pub fn open() -> io::Result<VirtualDevice> {
    let keys = BUTTONS
        .into_iter()
        .map(|(_, code)| code)
        .collect::<AttributeSet<Key>>();
    let mut builder = VirtualDevice::builder()?
        .name("JoyShockRust Virtual Gamepad")
        .input_id(InputId::new(BusType::BUS_VIRTUAL, 0, 0, 1))
        .with_keys(&keys)?;
    for (code, min, max) in AXES {
        builder = builder.with_absolute_axis(&UinputAbsSetup::new(
            code,
            AbsInfo::new(0, min, max, 0, 0, 0),
        ))?;
    }
    builder.build()
}

#[derive(Default)]
pub struct GamepadState {
    current: Controls,
}

impl GamepadState {
    pub fn publish(
        &mut self,
        next: Controls,
        mut emit: impl FnMut(&[InputEvent]) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut changes = [InputEvent::new(0, 0, 0); BUTTONS.len() + AXES.len()];
        let mut count = 0;
        for (button, code) in BUTTONS {
            let pressed = next.buttons.contains(button);
            if pressed != self.current.buttons.contains(button) {
                changes[count] = InputEvent::new(EventType::KEY.0, code.0, i32::from(pressed));
                count += 1;
            }
        }
        for (((code, _, _), old), new) in AXES.into_iter().zip(axes(self.current)).zip(axes(next)) {
            if old != new {
                changes[count] = InputEvent::new(EventType::ABSOLUTE.0, code.0, new);
                count += 1;
            }
        }
        if count > 0 {
            emit(&changes[..count])?;
        }
        self.current = next;
        Ok(())
    }
}

fn axes(controls: Controls) -> [i32; AXES.len()] {
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
    let trigger = |axis: f32| (axis * TRIGGER_SCALE as f32).round() as i32;
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
    #[ignore = "requires writable /dev/uinput and readable virtual evdev nodes"]
    fn uinput_cross_press_and_release() {
        use std::{
            os::fd::AsFd,
            thread,
            time::{Duration, Instant},
        };

        use evdev::{Device, SynchronizationCode};
        use nix::poll::{PollFd, PollFlags, poll};

        let mut virtual_device = open().expect("create the virtual gamepad");
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
        assert_eq!(observer.name(), Some("JoyShockRust Virtual Gamepad"));
        observer.set_nonblocking(true).unwrap();

        let mut report = [0; 64];
        report[0] = 1;
        report[1..=4].fill(128);
        report[8] = 0x28;
        let pressed = parse_report(DeviceModel::DualSenseEdge, Transport::Usb, &report, None)
            .unwrap()
            .controls;
        let mut state = GamepadState::default();
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
        gamepad.publish(controls, &mut capture).unwrap();
        gamepad.publish(controls, &mut capture).unwrap();
        gamepad.publish(Controls::default(), &mut capture).unwrap();
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
