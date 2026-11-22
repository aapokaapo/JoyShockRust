use core::fmt;

use crate::RawImuSample;

pub const DUALSENSE_USB_CALIBRATION_REPORT_ID: u8 = 0x05;
pub const DUALSENSE_USB_CALIBRATION_REPORT_LEN: usize = 41;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CalibratedImu {
    pub gyro_dps: [f32; 3],
    pub accel_g: [f32; 3],
}

#[derive(Debug, Clone, Copy)]
struct AxisCalibration {
    offset: f32,
    scale: f32,
}

impl AxisCalibration {
    fn convert(self, counts: i16) -> f32 {
        (f32::from(counts) - self.offset) * self.scale
    }
}

#[derive(Debug, Clone, Copy)]
pub struct UsbFactoryCalibration {
    gyro: [AxisCalibration; 3],
    accel: [AxisCalibration; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalibrationReportError {
    InvalidLength(usize),
    InvalidReportId(u8),
    InvalidGyroSpeed,
    InvalidGyroAxis(usize),
    InvalidAccelAxis(usize),
}

impl fmt::Display for CalibrationReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength(length) => write!(f, "expected 41 bytes, received {length}"),
            Self::InvalidReportId(id) => {
                write!(f, "expected USB calibration report 0x05, got {id:#04x}")
            }
            Self::InvalidGyroSpeed => f.write_str("invalid factory gyro reference speed"),
            Self::InvalidGyroAxis(axis) => {
                write!(f, "invalid factory gyro endpoints on axis {axis}")
            }
            Self::InvalidAccelAxis(axis) => {
                write!(f, "invalid factory accelerometer endpoints on axis {axis}")
            }
        }
    }
}

impl core::error::Error for CalibrationReportError {}

impl UsbFactoryCalibration {
    /// Decode the USB factory calibration reply, including the report ID.
    ///
    /// Uses the SDL HIDAPI convention in the project's reference analysis:
    /// remove the factory gyro bias and scale by the measured endpoint spans.
    pub fn from_report(report: &[u8]) -> Result<Self, CalibrationReportError> {
        use CalibrationReportError::*;
        let report: &[u8; DUALSENSE_USB_CALIBRATION_REPORT_LEN] =
            report.try_into().map_err(|_| InvalidLength(report.len()))?;
        if report[0] != DUALSENSE_USB_CALIBRATION_REPORT_ID {
            return Err(InvalidReportId(report[0]));
        }

        let word = |offset| f32::from(i16::from_le_bytes([report[offset], report[offset + 1]]));
        let speed_span = word(19) + word(21);
        if speed_span <= 0.0 {
            return Err(InvalidGyroSpeed);
        }

        let mut gyro = [AxisCalibration {
            offset: 0.0,
            scale: 0.0,
        }; 3];
        let mut accel = gyro;
        for axis in 0..3 {
            let bias = word(1 + 2 * axis);
            let gyro_plus = word(7 + 4 * axis);
            let gyro_minus = word(9 + 4 * axis);
            if gyro_plus <= gyro_minus || !(gyro_minus..=gyro_plus).contains(&bias) {
                return Err(InvalidGyroAxis(axis));
            }
            gyro[axis] = AxisCalibration {
                offset: bias,
                scale: speed_span / (gyro_plus - gyro_minus),
            };

            let accel_plus = word(23 + 4 * axis);
            let accel_minus = word(25 + 4 * axis);
            if accel_plus <= accel_minus {
                return Err(InvalidAccelAxis(axis));
            }
            accel[axis] = AxisCalibration {
                offset: (accel_plus + accel_minus) * 0.5,
                scale: 2.0 / (accel_plus - accel_minus),
            };
        }
        Ok(Self { gyro, accel })
    }

    /// Convert raw counts to physical units in device axis order.
    pub fn convert(&self, sample: RawImuSample) -> CalibratedImu {
        CalibratedImu {
            gyro_dps: core::array::from_fn(|axis| {
                self.gyro[axis].convert(sample.gyro_counts[axis])
            }),
            accel_g: core::array::from_fn(|axis| {
                self.accel[axis].convert(sample.accel_counts[axis])
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_word(report: &mut [u8], offset: usize, value: i16) {
        report[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn synthetic_calibration() -> [u8; DUALSENSE_USB_CALIBRATION_REPORT_LEN] {
        let mut report = [0; DUALSENSE_USB_CALIBRATION_REPORT_LEN];
        report[0] = DUALSENSE_USB_CALIBRATION_REPORT_ID;
        write_word(&mut report, 19, 600);
        write_word(&mut report, 21, 400);
        for (axis, bias) in [10, -20, 30].into_iter().enumerate() {
            write_word(&mut report, 1 + 2 * axis, bias);
            write_word(&mut report, 7 + 4 * axis, bias + 8_000);
            write_word(&mut report, 9 + 4 * axis, bias - 8_000);
            let midpoint = bias * 2;
            write_word(&mut report, 23 + 4 * axis, midpoint + 8192);
            write_word(&mut report, 25 + 4 * axis, midpoint - 8192);
        }
        report
    }

    #[test]
    fn factory_calibration_converts_signed_counts_in_device_axis_order() {
        let calibration = UsbFactoryCalibration::from_report(&synthetic_calibration()).unwrap();
        assert_eq!(
            calibration.convert(RawImuSample {
                gyro_counts: [170, -340, 30],
                accel_counts: [8212, -8232, 4156],
                device_ticks: 0,
            }),
            CalibratedImu {
                gyro_dps: [10.0, -20.0, 0.0],
                accel_g: [1.0, -1.0, 0.5],
            },
        );
    }

    #[test]
    fn malformed_factory_reports_are_rejected() {
        use CalibrationReportError::*;
        let report = synthetic_calibration();
        assert_eq!(
            UsbFactoryCalibration::from_report(&report[..40]).unwrap_err(),
            InvalidLength(40)
        );
        let mut wrong_id = report;
        wrong_id[0] = 1;
        assert_eq!(
            UsbFactoryCalibration::from_report(&wrong_id).unwrap_err(),
            InvalidReportId(1)
        );
        for (offset, value, expected) in [
            (19, -400, InvalidGyroSpeed),
            (9, 8010, InvalidGyroAxis(0)),
            (1, 9000, InvalidGyroAxis(0)),
            (33, 8252, InvalidAccelAxis(2)),
        ] {
            let mut bad = report;
            write_word(&mut bad, offset, value);
            assert_eq!(
                UsbFactoryCalibration::from_report(&bad).unwrap_err(),
                expected
            );
        }
    }
}
