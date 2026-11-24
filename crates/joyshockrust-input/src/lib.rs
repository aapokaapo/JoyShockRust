//! Pure decoding of DualSense and DualSense Edge USB and Bluetooth HID reports in the layout described in
//! `docs/REFERENCE_ANALYSIS.md`.

#![no_std]

use core::fmt;

use joyshockrust_core::{Button, Buttons, Controls, DPad, Stick};

mod calibration;
mod timebase;

pub use calibration::{
    CalibratedImu, CalibrationReportError, DUALSENSE_USB_CALIBRATION_REPORT_ID,
    DUALSENSE_USB_CALIBRATION_REPORT_LEN, FactoryCalibration, UsbFactoryCalibration,
};
pub use timebase::{InvalidForwardLimit, Timebase, TimestampUpdate, ticks_in_nanoseconds};

/// The frame size including the numbered HID report ID.
pub const DUALSENSE_EDGE_USB_REPORT_LEN: usize = 64;
const DUALSENSE_EDGE_USB_REPORT_ID: u8 = 0x01;
pub const DUALSENSE_EDGE_BLUETOOTH_REPORT_LEN: usize = 78;
const DUALSENSE_EDGE_BLUETOOTH_REPORT_ID: u8 = 0x31;
pub const DUALSENSE_TOUCHPAD_WIDTH: u16 = 1920;
pub const DUALSENSE_TOUCHPAD_HEIGHT: u16 = 1080;
const TOUCH_CONTACTS_OFFSET: usize = 32;
const BATTERY_STATUS_OFFSET: usize = 52;
const COMMON_REPORT_LEN: usize = DUALSENSE_EDGE_USB_REPORT_LEN - 1;

const DUALSENSE_ID: (u16, u16) = (0x054c, 0x0ce6);
const DUALSENSE_EDGE_ID: (u16, u16) = (0x054c, 0x0df2);
pub const SUPPORTED_CONTROLLER_IDS: [(u16, u16); 2] = [DUALSENSE_ID, DUALSENSE_EDGE_ID];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceModel {
    DualSense,
    DualSenseEdge,
    Other { vendor_id: u16, product_id: u16 },
}

impl DeviceModel {
    pub const fn from_vid_pid(vendor_id: u16, product_id: u16) -> Self {
        match (vendor_id, product_id) {
            DUALSENSE_ID => Self::DualSense,
            DUALSENSE_EDGE_ID => Self::DualSenseEdge,
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

/// Charge state encoded by a full DualSense input report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryState {
    Discharging,
    Charging,
    Full,
    NotCharging,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryReading {
    /// Approximate charge percentage. Unavailable on error and unknown states.
    pub percent: Option<u8>,
    pub state: BatteryState,
}

impl BatteryReading {
    fn decode(status: u8) -> Self {
        let estimated_percent = ((status & 0x0f) * 10 + 5).min(100);
        let (percent, state) = match status >> 4 {
            0x0 => (Some(estimated_percent), BatteryState::Discharging),
            0x1 => (Some(estimated_percent), BatteryState::Charging),
            0x2 => (Some(100), BatteryState::Full),
            0xa | 0xb => (None, BatteryState::NotCharging),
            _ => (None, BatteryState::Unknown),
        };
        Self { percent, state }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputReport {
    pub controls: Controls,
    pub imu: RawImuSample,
    pub battery: BatteryReading,
    /// Sensor slots, independent of the mechanical touchpad button.
    pub touch_contacts: [Option<TouchContact>; 2],
    /// Raw controller counter byte. Its sequence semantics require captures.
    pub counter: u8,
    /// Host monotonic receive time, when supplied by the caller.
    pub received_at_ns: Option<u64>,
}

/// One active contact, in device units from the top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchContact {
    pub id: u8,
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    UnsupportedDevice(DeviceModel),
    UnsupportedTransport(Transport),
    InvalidLength { actual: usize },
    InvalidReportId { actual: u8 },
    InvalidHat { value: u8 },
    InvalidChecksum,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedDevice(device) => write!(formatter, "unsupported device {device:?}"),
            Self::UnsupportedTransport(transport) => {
                write!(formatter, "unsupported transport {transport:?}")
            }
            Self::InvalidLength { actual } => {
                write!(formatter, "unexpected input report length {actual}")
            }
            Self::InvalidReportId { actual } => {
                write!(formatter, "unexpected input report ID {actual:#04x}")
            }
            Self::InvalidChecksum => write!(formatter, "invalid Bluetooth input CRC"),
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

const SPECIAL_BUTTONS: &[(u8, Button)] = &[
    (0x01, Button::Home),
    (0x02, Button::TouchpadClick),
    (0x04, Button::MicrophoneMute),
];

const EDGE_BUTTONS: &[(u8, Button)] = &[
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
    if matches!(model, DeviceModel::Other { .. }) {
        return Err(ParseError::UnsupportedDevice(model));
    }
    let (length, id, offset) = match transport {
        Transport::Usb => (
            DUALSENSE_EDGE_USB_REPORT_LEN,
            DUALSENSE_EDGE_USB_REPORT_ID,
            1,
        ),
        Transport::Bluetooth => (
            DUALSENSE_EDGE_BLUETOOTH_REPORT_LEN,
            DUALSENSE_EDGE_BLUETOOTH_REPORT_ID,
            2,
        ),
        Transport::Other => return Err(ParseError::UnsupportedTransport(transport)),
    };
    if bytes.len() != length {
        return Err(ParseError::InvalidLength {
            actual: bytes.len(),
        });
    }
    if bytes[0] != id {
        return Err(ParseError::InvalidReportId { actual: bytes[0] });
    }
    if transport == Transport::Bluetooth && !bluetooth_crc_matches(0xa1, bytes) {
        return Err(ParseError::InvalidChecksum);
    }
    let bytes = &bytes[offset..offset + COMMON_REPORT_LEN];

    let dpad = match bytes[7] & 0x0f {
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
    let edge_buttons = if model == DeviceModel::DualSenseEdge {
        EDGE_BUTTONS
    } else {
        &[]
    };
    for (byte, mapping) in [
        (bytes[7], FACE_BUTTONS),
        (bytes[8], STANDARD_BUTTONS),
        (bytes[9], SPECIAL_BUTTONS),
        (bytes[9], edge_buttons),
    ] {
        for &(mask, button) in mapping {
            if byte & mask != 0 {
                buttons.insert(button);
            }
        }
    }

    let (contacts, _) = bytes[TOUCH_CONTACTS_OFFSET..].as_chunks::<4>();
    Ok(InputReport {
        controls: Controls {
            buttons,
            dpad,
            left_stick: Stick {
                x: normalize_stick(bytes[0]),
                y: normalize_stick(bytes[1]),
            },
            right_stick: Stick {
                x: normalize_stick(bytes[2]),
                y: normalize_stick(bytes[3]),
            },
            left_trigger: f32::from(bytes[4]) / 255.0,
            right_trigger: f32::from(bytes[5]) / 255.0,
        },
        imu: RawImuSample {
            gyro_counts: read_sensor_vector([
                bytes[15], bytes[16], bytes[17], bytes[18], bytes[19], bytes[20],
            ]),
            accel_counts: read_sensor_vector([
                bytes[21], bytes[22], bytes[23], bytes[24], bytes[25], bytes[26],
            ]),
            device_ticks: u32::from_le_bytes([bytes[27], bytes[28], bytes[29], bytes[30]]),
        },
        battery: BatteryReading::decode(bytes[BATTERY_STATUS_OFFSET]),
        touch_contacts: [decode_touch(contacts[0]), decode_touch(contacts[1])],
        counter: bytes[6],
        received_at_ns,
    })
}

fn decode_touch([contact, x_low, middle, y_high]: [u8; 4]) -> Option<TouchContact> {
    let x = u16::from_le_bytes([x_low, middle & 0x0f]);
    let y = (u16::from(y_high) << 4) | u16::from(middle >> 4);
    (contact & 0x80 == 0 && x < DUALSENSE_TOUCHPAD_WIDTH && y < DUALSENSE_TOUCHPAD_HEIGHT)
        .then_some(TouchContact { id: contact, x, y })
}

fn bluetooth_crc_matches(seed: u8, bytes: &[u8]) -> bool {
    // Linux uses reflected CRC-32 with the HID transaction byte prepended.
    let Some((body, checksum)) = bytes.split_last_chunk::<4>() else {
        return false;
    };
    bluetooth_crc32(seed, body) == u32::from_le_bytes(*checksum)
}

/// Reflected CRC-32 of a Bluetooth HID report with its transaction byte prepended.
pub fn bluetooth_crc32(seed: u8, body: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in core::iter::once(seed).chain(body.iter().copied()) {
        crc = (crc >> 8) ^ CRC32_TABLE[((crc as u8) ^ byte) as usize];
    }
    !crc
}

const CRC32_TABLE: [u32; 256] = {
    let mut table = [0; 256];
    let mut index = 0;
    while index < table.len() {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            let polynomial = if value & 1 == 0 { 0 } else { 0xedb8_8320 };
            value = (value >> 1) ^ polynomial;
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
};

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
