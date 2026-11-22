use core::time::Duration;

use joyshockrust_core::{Button, Buttons, Controls, DPad, Stick};
use joyshockrust_input::{
    DUALSENSE_EDGE_USB_REPORT_LEN, DeviceModel, InputReport, ParseError, RawImuSample, Timebase,
    TimestampUpdate, Transport, parse_report,
};

// Generated from the documented layout. These frames are synthetic fixtures.
fn neutral_frame() -> [u8; DUALSENSE_EDGE_USB_REPORT_LEN] {
    let mut bytes = [0; DUALSENSE_EDGE_USB_REPORT_LEN];
    bytes[0] = 0x01;
    bytes[1..=4].fill(128);
    bytes[8] = 8;
    bytes
}

fn decode(bytes: &[u8]) -> Result<InputReport, ParseError> {
    parse_report(DeviceModel::DualSenseEdge, Transport::Usb, bytes, None)
}

#[test]
fn validates_model_and_transport_before_decoding() {
    let edge = DeviceModel::from_vid_pid(0x054c, 0x0df2);
    assert_eq!(edge, DeviceModel::DualSenseEdge);
    for (vendor_id, product_id) in [(0x054c, 0x0ce6), (0x0001, 0x0df2)] {
        let other = DeviceModel::from_vid_pid(vendor_id, product_id);
        assert_eq!(
            other,
            DeviceModel::Other {
                vendor_id,
                product_id
            }
        );
        assert_eq!(
            parse_report(other, Transport::Usb, &neutral_frame(), None),
            Err(ParseError::UnsupportedDevice(other))
        );
    }
    for transport in [Transport::Bluetooth, Transport::Other] {
        assert_eq!(
            parse_report(edge, transport, &neutral_frame(), None),
            Err(ParseError::UnsupportedTransport(transport))
        );
    }
}

#[test]
fn rejects_wrong_length_and_id() {
    let frame = neutral_frame();
    for length in 0..frame.len() {
        assert_eq!(
            decode(&frame[..length]),
            Err(ParseError::InvalidLength { actual: length })
        );
    }
    let mut oversized = [0; DUALSENSE_EDGE_USB_REPORT_LEN + 1];
    oversized[..frame.len()].copy_from_slice(&frame);
    assert_eq!(
        decode(&oversized),
        Err(ParseError::InvalidLength {
            actual: oversized.len()
        })
    );
    let mut wrong_id = frame;
    wrong_id[0] = 0x31;
    assert_eq!(
        decode(&wrong_id),
        Err(ParseError::InvalidReportId { actual: 0x31 })
    );
}

#[test]
fn decodes_all_hat_values_independently_of_face_buttons() {
    let mut bytes = neutral_frame();
    let directions = [
        DPad::Up,
        DPad::UpRight,
        DPad::Right,
        DPad::DownRight,
        DPad::Down,
        DPad::DownLeft,
        DPad::Left,
        DPad::UpLeft,
        DPad::Neutral,
    ];
    for (hat, direction) in (0..=8).zip(directions) {
        bytes[8] = 0x20 | hat;
        let controls = decode(&bytes).unwrap().controls;
        assert_eq!(controls.dpad, direction);
        assert!(controls.buttons.contains(Button::South));
    }
    for value in 9..=15 {
        bytes[8] = 0x20 | value;
        assert_eq!(decode(&bytes), Err(ParseError::InvalidHat { value }));
    }
}

#[test]
fn every_button_bit_maps_to_exactly_one_expected_button() {
    let button_cases = [
        (8, 4, Button::West),
        (8, 5, Button::South),
        (8, 6, Button::East),
        (8, 7, Button::North),
        (9, 0, Button::LeftShoulder),
        (9, 1, Button::RightShoulder),
        (9, 2, Button::LeftTrigger),
        (9, 3, Button::RightTrigger),
        (9, 4, Button::Select),
        (9, 5, Button::Start),
        (9, 6, Button::LeftStick),
        (9, 7, Button::RightStick),
        (10, 0, Button::Home),
        (10, 1, Button::TouchpadClick),
        (10, 2, Button::MicrophoneMute),
        (10, 4, Button::LeftFunction),
        (10, 5, Button::RightFunction),
        (10, 6, Button::LeftPaddle),
        (10, 7, Button::RightPaddle),
    ];
    for (offset, bit, button) in button_cases {
        let mut frame = neutral_frame();
        frame[offset] |= 1 << bit;
        let mut expected = Buttons::default();
        expected.insert(button);
        assert_eq!(decode(&frame).unwrap().controls.buttons, expected);
    }
}

#[test]
fn normalizes_sticks_and_keeps_trigger_travel_independent() {
    let mut frame = neutral_frame();
    assert_eq!(decode(&frame).unwrap().controls, Controls::default());

    frame[1..=6].copy_from_slice(&[0, 255, 127, 129, 255, 0]);
    frame[9] = 0x08;
    let controls = decode(&frame).unwrap().controls;
    assert_eq!(controls.left_stick, Stick { x: -1.0, y: 1.0 });
    assert_eq!(
        controls.right_stick,
        Stick {
            x: -1.0 / 128.0,
            y: 1.0 / 127.0,
        }
    );
    assert_eq!(controls.left_trigger, 1.0);
    assert_eq!(controls.right_trigger, 0.0);
    assert!(!controls.buttons.contains(Button::LeftTrigger));
    assert!(controls.buttons.contains(Button::RightTrigger));
}

#[test]
fn preserves_signed_counts_timestamps_and_counter() {
    let mut frame = neutral_frame();
    frame[7] = 0xe1;
    frame[16..32].copy_from_slice(&[
        0x00, 0x80, 0x34, 0x12, 0xff, 0x7f, 0xff, 0xff, 0x00, 0x00, 0x00, 0x01, 0x78, 0x56, 0x34,
        0x12,
    ]);
    let report = parse_report(
        DeviceModel::DualSenseEdge,
        Transport::Usb,
        &frame,
        Some(543_210),
    )
    .unwrap();
    assert_eq!(
        report.imu,
        RawImuSample {
            gyro_counts: [i16::MIN, 0x1234, i16::MAX],
            accel_counts: [-1, 0, 256],
            device_ticks: 0x1234_5678,
        }
    );
    assert_eq!(report.counter, 0xe1);
    assert_eq!(report.received_at_ns, Some(543_210));
}

#[test]
fn ignores_reserved_bits_and_tail() {
    let mut frame = neutral_frame();
    let expected = decode(&frame).unwrap();
    frame[10] |= 0x08;
    frame[11..16].fill(0xff);
    frame[32..].fill(0xff);
    assert_eq!(decode(&frame).unwrap(), expected);
}

#[test]
fn replays_controls_even_when_sensor_timestamps_repeat_or_go_backwards() {
    let mut rest = neutral_frame();
    rest[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut pressed = rest;
    pressed[8] |= 0x20;
    let mut moved = pressed;
    moved[16] = 3;
    moved[28..32].fill(0);
    let mut late_release = rest;
    late_release[16] = 5;
    let mut released = moved;
    released[8] = 8;
    released[16] = 7;
    released[28] = 1;
    let fixture = [rest, pressed, moved, late_release, released];

    let replay = || {
        let mut timebase = Timebase::new(10).unwrap();
        fixture.map(|frame| {
            let report = decode(&frame).unwrap();
            let update = timebase.observe(report.imu.device_ticks);
            (report, update)
        })
    };

    let output = replay();
    assert_eq!(output, replay());
    assert_eq!(
        output.map(|(report, _)| report.controls.buttons.contains(Button::South)),
        [false, true, true, false, false]
    );
    assert_eq!(
        output.map(|(report, _)| report.imu.gyro_counts[0]),
        [0, 0, 3, 5, 7]
    );
    assert_eq!(
        output.map(|(_, update)| update),
        [
            TimestampUpdate::FirstSample,
            TimestampUpdate::Repeated {
                elapsed: Duration::ZERO
            },
            TimestampUpdate::Advanced {
                elapsed: Duration::from_nanos(333),
                delta: Duration::from_nanos(333),
                wrapped: true,
            },
            TimestampUpdate::OutOfOrder { previous_ticks: 0 },
            TimestampUpdate::Advanced {
                elapsed: Duration::from_nanos(666),
                delta: Duration::from_nanos(333),
                wrapped: false,
            },
        ]
    );
}

#[test]
fn generated_frames_decode_deterministically_and_stay_in_range() {
    let mut state = 0x1234_5678_u32;
    let mut bytes = [0; 128];
    for case in 0..4096 {
        for byte in &mut bytes {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *byte = state as u8;
        }
        // Exercise both arbitrary frames and valid framing with arbitrary payloads.
        let length = if case % 2 == 0 {
            bytes[0] = 0x01;
            DUALSENSE_EDGE_USB_REPORT_LEN
        } else {
            state as usize % (bytes.len() + 1)
        };
        let frame = &bytes[..length];
        let result = decode(frame);
        assert_eq!(result, decode(frame));
        if let Ok(report) = result {
            let controls = report.controls;
            for axis in [
                controls.left_stick.x,
                controls.left_stick.y,
                controls.right_stick.x,
                controls.right_stick.y,
            ] {
                assert!((-1.0..=1.0).contains(&axis));
            }
            assert!((0.0..=1.0).contains(&controls.left_trigger));
            assert!((0.0..=1.0).contains(&controls.right_trigger));
        }
    }
}
