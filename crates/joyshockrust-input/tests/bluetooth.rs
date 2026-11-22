use joyshockrust_core::Button;
use joyshockrust_input::{
    CalibrationReportError, DeviceModel, FactoryCalibration, ParseError, RawImuSample, Transport,
    parse_report,
};

fn full_bluetooth_frame() -> [u8; 78] {
    let mut frame = [0; 78];
    frame[..12].copy_from_slice(&[0x31, 0xab, 1, 128, 200, 255, 128, 255, 23, 0x28, 4, 0x82]);
    for (chunk, sample) in frame[17..29]
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip([123_i16, -456, 789, 0, 8192, -512])
    {
        *chunk = sample.to_le_bytes();
    }
    frame[29..33].copy_from_slice(&0x1234_5678_u32.to_le_bytes());
    // Reference CRC generated with zlib over seed 0xa1 followed by bytes 0–73.
    frame[74..].copy_from_slice(&0xd378_a50a_u32.to_le_bytes());
    frame
}

#[test]
fn full_bluetooth_frame_decodes_the_same_common_payload_as_usb() {
    let frame = full_bluetooth_frame();
    let report = parse_report(
        DeviceModel::DualSenseEdge,
        Transport::Bluetooth,
        &frame,
        Some(43),
    )
    .unwrap();
    let mut usb = [0; 64];
    usb[0] = 1;
    usb[1..].copy_from_slice(&frame[2..65]);
    let expected =
        parse_report(DeviceModel::DualSenseEdge, Transport::Usb, &usb, Some(43)).unwrap();
    assert_eq!(report, expected);
    assert!(report.controls.buttons.contains(Button::South));
    assert!(report.controls.buttons.contains(Button::RightPaddle));
    assert_eq!(report.imu.gyro_counts, [123, -456, 789]);
    assert_eq!(report.imu.device_ticks, 0x1234_5678);
}

#[test]
fn bluetooth_validates_framing_and_checksum_before_decoding_controls() {
    let mut frame = full_bluetooth_frame();
    let parse = |bytes: &[u8]| {
        parse_report(
            DeviceModel::DualSenseEdge,
            Transport::Bluetooth,
            bytes,
            None,
        )
    };
    assert_eq!(
        parse(&frame[..10]),
        Err(ParseError::InvalidLength { actual: 10 })
    );
    frame[0] = 1;
    assert_eq!(
        parse(&frame),
        Err(ParseError::InvalidReportId { actual: 1 })
    );
    frame[0] = 0x31;
    frame[9] = 0xff;
    assert_eq!(parse(&frame), Err(ParseError::InvalidChecksum));
}

#[test]
fn bluetooth_factory_calibration_checks_the_feature_seed() {
    let mut frame = [0; 41];
    frame[0] = 5;
    for (chunk, word) in frame[1..35].as_chunks_mut::<2>().0.iter_mut().zip([
        10_i16, -20, 30, 1110, -1090, 1100, -1100, 1200, -1000, 50, 50, 8200, -8184, 8192, -8192,
        8300, -8084,
    ]) {
        *chunk = word.to_le_bytes();
    }
    // Reference CRC generated with zlib over seed 0xa3 followed by bytes 0–36.
    frame[37..].copy_from_slice(&0x9f77_dd0d_u32.to_le_bytes());
    let factory = FactoryCalibration::from_bluetooth_report(&frame).unwrap();
    let sample = factory.convert(RawImuSample {
        gyro_counts: [10, -20, 30],
        accel_counts: [8200, 0, -8084],
        device_ticks: 0,
    });
    assert_eq!(sample.gyro_dps, [0.0; 3]);
    assert_eq!(sample.accel_g, [1.0, 0.0, -1.0]);
    frame[40] ^= 1;
    assert_eq!(
        FactoryCalibration::from_bluetooth_report(&frame).unwrap_err(),
        CalibrationReportError::InvalidChecksum,
    );
}

#[test]
fn standard_dualsense_decodes_common_controls_without_edge_paddles() {
    let model = DeviceModel::from_vid_pid(0x054c, 0x0ce6);
    assert_eq!(model, DeviceModel::DualSense);
    let bluetooth = full_bluetooth_frame();
    let mut usb = [0; 64];
    usb[0] = 1;
    usb[1..].copy_from_slice(&bluetooth[2..65]);
    let wireless = parse_report(model, Transport::Bluetooth, &bluetooth, None).unwrap();
    assert_eq!(
        wireless,
        parse_report(model, Transport::Usb, &usb, None).unwrap()
    );
    assert!(wireless.controls.buttons.contains(Button::South));
    assert!(wireless.controls.buttons.contains(Button::TouchpadClick));
    assert!(!wireless.controls.buttons.contains(Button::RightPaddle));
    assert_eq!(wireless.imu.device_ticks, 0x1234_5678);
}
