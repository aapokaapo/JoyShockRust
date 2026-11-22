//! Pure decoding of DualSense Edge USB HID reports in the layout described in
//! `docs/REFERENCE_ANALYSIS.md`.

#![no_std]

use core::fmt;

use joyshockrust_core::{Button, Buttons, Controls, DPad, Stick};

mod calibration;
mod timebase;

pub use calibration::{
    CalibratedImu, CalibrationReportError, DUALSENSE_USB_CALIBRATION_REPORT_ID,
    DUALSENSE_USB_CALIBRATION_REPORT_LEN, UsbFactoryCalibration,
};
pub use timebase::{InvalidForwardLimit, Timebase, TimestampUpdate};

/// The frame size including the numbered HID report ID.
pub const DUALSENSE_EDGE_USB_REPORT_LEN: usize = 64;
const DUALSENSE_EDGE_USB_REPORT_ID: u8 = 0x01;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceModel {
    DualSenseEdge,
    Other { vendor_id: u16, product_id: u16 },
}

impl DeviceModel {
    pub const fn from_vid_pid(vendor_id: u16, product_id: u16) -> Self {
        match (vendor_id, product_id) {
            (0x054c, 0x0df2) => Self::DualSenseEdge,
            _ => Self::Other {
                vendor_id,
                product_id,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Usb,
    Bluetooth,
    Other,
}

/// Signed, uncalibrated sensor counts in device axis order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawImuSample {
    pub gyro_counts: [i16; 3],
    pub accel_counts: [i16; 3],
    pub device_ticks: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputReport {
    pub controls: Controls,
    pub imu: RawImuSample,
    /// Raw controller counter byte. Its sequence semantics require captures.
    pub counter: u8,
    /// Host monotonic receive time, when supplied by the caller.
    pub received_at_ns: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    UnsupportedDevice(DeviceModel),
    UnsupportedTransport(Transport),
    InvalidLength { actual: usize },
    InvalidReportId { actual: u8 },
    InvalidHat { value: u8 },
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedDevice(device) => write!(formatter, "unsupported device {device:?}"),
            Self::UnsupportedTransport(transport) => {
                write!(formatter, "unsupported transport {transport:?}")
            }
            Self::InvalidLength { actual } => write!(
                formatter,
                "expected {DUALSENSE_EDGE_USB_REPORT_LEN} bytes, received {actual}"
            ),
            Self::InvalidReportId { actual } => {
                write!(formatter, "unexpected USB report ID {actual:#04x}")
            }
            Self::InvalidHat { value } => write!(formatter, "invalid D-pad hat value {value}"),
        }
    }
}

impl core::error::Error for ParseError {}

const FACE_BUTTONS: &[(u8, Button)] = &[
    (0x10, Button::West),
    (0x20, Button::South),
    (0x40, Button::East),
    (0x80, Button::North),
];

const STANDARD_BUTTONS: &[(u8, Button)] = &[
    (0x01, Button::LeftShoulder),
    (0x02, Button::RightShoulder),
    (0x04, Button::LeftTrigger),
    (0x08, Button::RightTrigger),
    (0x10, Button::Select),
    (0x20, Button::Start),
    (0x40, Button::LeftStick),
    (0x80, Button::RightStick),
];

const EXTRA_BUTTONS: &[(u8, Button)] = &[
    (0x01, Button::Home),
    (0x02, Button::TouchpadClick),
    (0x04, Button::MicrophoneMute),
    (0x10, Button::LeftFunction),
    (0x20, Button::RightFunction),
    (0x40, Button::LeftPaddle),
    (0x80, Button::RightPaddle),
];

/// Decode one complete report from a device identified by the caller.
///
/// The report ID is part of `bytes`. The parser leaves the receive timestamp
/// and sensor counts unchanged. Timestamp ordering is handled by [`Timebase`].
pub fn parse_report(
    model: DeviceModel,
    transport: Transport,
    bytes: &[u8],
    received_at_ns: Option<u64>,
) -> Result<InputReport, ParseError> {
    if model != DeviceModel::DualSenseEdge {
        return Err(ParseError::UnsupportedDevice(model));
    }
    if transport != Transport::Usb {
        return Err(ParseError::UnsupportedTransport(transport));
    }
    let bytes: &[u8; DUALSENSE_EDGE_USB_REPORT_LEN] =
        bytes.try_into().map_err(|_| ParseError::InvalidLength {
            actual: bytes.len(),
        })?;
    if bytes[0] != DUALSENSE_EDGE_USB_REPORT_ID {
        return Err(ParseError::InvalidReportId { actual: bytes[0] });
    }

    let dpad = match bytes[8] & 0x0f {
        0 => DPad::Up,
        1 => DPad::UpRight,
        2 => DPad::Right,
        3 => DPad::DownRight,
        4 => DPad::Down,
        5 => DPad::DownLeft,
        6 => DPad::Left,
        7 => DPad::UpLeft,
        8 => DPad::Neutral,
        value => return Err(ParseError::InvalidHat { value }),
    };
    let mut buttons = Buttons::default();
    for (byte, mapping) in [
        (bytes[8], FACE_BUTTONS),
        (bytes[9], STANDARD_BUTTONS),
        (bytes[10], EXTRA_BUTTONS),
    ] {
        for &(mask, button) in mapping {
            if byte & mask != 0 {
                buttons.insert(button);
            }
        }
    }

    Ok(InputReport {
        controls: Controls {
            buttons,
            dpad,
            left_stick: Stick {
                x: normalize_stick(bytes[1]),
                y: normalize_stick(bytes[2]),
            },
            right_stick: Stick {
                x: normalize_stick(bytes[3]),
                y: normalize_stick(bytes[4]),
            },
            left_trigger: f32::from(bytes[5]) / 255.0,
            right_trigger: f32::from(bytes[6]) / 255.0,
        },
        imu: RawImuSample {
            gyro_counts: read_sensor_vector([
                bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21],
            ]),
            accel_counts: read_sensor_vector([
                bytes[22], bytes[23], bytes[24], bytes[25], bytes[26], bytes[27],
            ]),
            device_ticks: u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]),
        },
        counter: bytes[7],
        received_at_ns,
    })
}

fn normalize_stick(value: u8) -> f32 {
    let centered = f32::from(value) - 128.0;
    if centered < 0.0 {
        centered / 128.0
    } else {
        centered / 127.0
    }
}

fn read_sensor_vector([x_low, x_high, y_low, y_high, z_low, z_high]: [u8; 6]) -> [i16; 3] {
    [
        i16::from_le_bytes([x_low, x_high]),
        i16::from_le_bytes([y_low, y_high]),
        i16::from_le_bytes([z_low, z_high]),
    ]
}
