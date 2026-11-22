use std::{
    fs::File,
    io,
    time::{Duration, Instant},
};

use evdev::{
    AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode, RelativeAxisCode as Axis,
    uinput::VirtualDevice,
};
use joyshockrust_input::{
    InputReport, RawImuSample, Timebase, TimestampUpdate, UsbFactoryCalibration,
};
use joyshockrust_motion::{
    Calibration, ImuSample, MotionConfig, MotionEngine, StationaryCalibration,
};

use super::{
    flick_stick::FlickStick,
    gyro_gate::{Binding, GyroGate},
};

pub struct GyroMouse {
    aim: GyroAim,
    output: VirtualDevice,
    gate: GyroGate,
    flick: Option<FlickStick>,
}

#[derive(Clone, Copy, Default)]
pub struct Options {
    pub calibrate: bool,
    pub binding: Binding,
    pub flick_stick: Option<f64>,
}

pub fn validate_flick_scale(scale: f64) -> Result<f64, &'static str> {
    if !(0.01..=100.0).contains(&scale) {
        return Err("Flick Stick sensitivity must be between 0.01 and 100 counts per degree");
    }
    Ok(scale)
}

impl GyroMouse {
    pub fn open(input: &File, options: Options) -> io::Result<Self> {
        let factory = super::hidraw::read_factory_calibration(input).map_err(|err| {
            io::Error::other(format!("could not read factory calibration: {err}"))
        })?;
        let axes = [Axis::REL_X, Axis::REL_Y]
            .into_iter()
            .collect::<AttributeSet<_>>();
        // Advertise the left button so consumers classify this as a mouse.
        let keys = [KeyCode::BTN_LEFT].into_iter().collect::<AttributeSet<_>>();
        let output = VirtualDevice::builder()?
            .name("JoyShockRust Gyro Mouse")
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0, 0, 1))
            .with_keys(&keys)?
            .with_relative_axes(&axes)?
            .build()?;
        Ok(Self {
            aim: GyroAim::new(factory, options.calibrate),
            output,
            gate: GyroGate::new(options.binding),
            flick: options.flick_stick.map(FlickStick::new),
        })
    }

    pub fn publish(&mut self, report: InputReport) -> io::Result<()> {
        let was_calibrating = self.aim.calibration.is_some();
        let enabled = self.gate.enabled(&report.controls, Instant::now);
        let mut movement = self.aim.advance(report.imu, enabled);
        if !was_calibrating && let Some(flick) = &mut self.flick {
            movement[0] += flick.advance(report.controls.right_stick, report.imu.device_ticks);
        }
        if was_calibrating && self.aim.calibration.is_none() {
            eprintln!("Gyro rest calibration complete");
        }
        let mut events = [InputEvent::new(0, 0, 0); 2];
        let mut count = 0;
        for (axis, delta) in [Axis::REL_X, Axis::REL_Y].into_iter().zip(movement) {
            if delta != 0 {
                events[count] = InputEvent::new(EventType::RELATIVE.0, axis.0, delta);
                count += 1;
            }
        }
        if count > 0 {
            self.output.emit(&events[..count])?;
        }
        Ok(())
    }
}

struct GyroAim {
    factory: UsbFactoryCalibration,
    timebase: Timebase,
    motion: MotionEngine,
    calibration: Option<StationaryCalibration>,
    remainder: [f64; 2],
    was_enabled: bool,
}

impl GyroAim {
    fn new(factory: UsbFactoryCalibration, calibrate: bool) -> Self {
        let config = Self::config();
        // The sensor clock advances at three ticks per microsecond.
        let max_gap_ticks = (config.max_gap_ns * 3 / 1_000) as u32;
        Self {
            factory,
            timebase: Timebase::new(max_gap_ticks).expect("valid default sensor gap limit"),
            motion: MotionEngine::new(config, Calibration::default())
                .expect("valid gyro mouse defaults"),
            calibration: calibrate.then(StationaryCalibration::default),
            remainder: [0.0; 2],
            was_enabled: false,
        }
    }

    fn config() -> MotionConfig {
        MotionConfig {
            // Linux pointer directions use the opposite signs to the
            // controller rotations used for horizontal and vertical aim.
            sensitivity_x: -4.0,
            sensitivity_y: -4.0,
            ..MotionConfig::default()
        }
    }

    fn advance(&mut self, sample: RawImuSample, enabled: bool) -> [i32; 2] {
        // Only emit motion for an interval that starts and ends with aim
        // enabled. Apply button transitions even on repeated IMU timestamps.
        let emit_motion = self.was_enabled && enabled;
        self.was_enabled = enabled;
        if !emit_motion {
            self.remainder = [0.0; 2];
        }
        let elapsed = match self.timebase.observe(sample.device_ticks) {
            TimestampUpdate::FirstSample => Duration::ZERO,
            TimestampUpdate::Advanced { elapsed, .. } => elapsed,
            TimestampUpdate::Gap { .. } => {
                self.reset_motion();
                if let Some(calibration) = &mut self.calibration {
                    calibration.reset();
                }
                Duration::ZERO
            }
            TimestampUpdate::Repeated { .. } | TimestampUpdate::OutOfOrder { .. } => return [0; 2],
        };
        let calibrated = self.factory.convert(sample);
        let sample = ImuSample {
            timestamp_ns: elapsed.as_nanos() as u64,
            gyro_dps: calibrated.gyro_dps,
            accel_g: calibrated.accel_g,
        };
        if let Some(calibration) = &mut self.calibration {
            if let Some(bias) = calibration.observe(sample) {
                self.motion = MotionEngine::new(Self::config(), bias)
                    .expect("stationary calibration returns a bounded finite bias");
                self.calibration = None;
            }
            return [0; 2];
        }
        let Ok(output) = self.motion.process(sample) else {
            self.reset_motion();
            return [0; 2];
        };

        if !emit_motion {
            return [0; 2];
        }

        // Retain fractional movement so repeated sub-count deltas still move
        // the pointer. Clear these fractions on discontinuities.
        std::array::from_fn(|axis| {
            let total = self.remainder[axis] + f64::from(output.aim_delta[axis]);
            let whole = total as i32;
            self.remainder[axis] = total - f64::from(whole);
            whole
        })
    }

    fn reset_motion(&mut self) {
        self.motion.reset_segment();
        self.remainder = [0.0; 2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aim(calibrate: bool) -> GyroAim {
        let mut report = [0; 41];
        report[0] = 5;
        for (offset, value) in [
            (7, 8000_i16),
            (9, -8000),
            (11, 8000),
            (13, -8000),
            (15, 8000),
            (17, -8000),
            (19, 500),
            (21, 500),
            (23, 8192),
            (25, -8192),
            (27, 8192),
            (29, -8192),
            (31, 8192),
            (33, -8192),
        ] {
            report[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        GyroAim::new(
            UsbFactoryCalibration::from_report(&report).unwrap(),
            calibrate,
        )
    }

    fn sample(device_ticks: u32) -> RawImuSample {
        RawImuSample {
            gyro_counts: [-800, 1600, 0],
            accel_counts: [0, 0, 8192],
            device_ticks,
        }
    }

    #[test]
    fn fractional_motion_accumulates_in_pointer_direction_on_both_axes() {
        let mut aim = aim(false);
        assert_eq!(aim.advance(sample(0), true), [0, 0]);
        let mut total = [0, 0];
        for step in 1..=10 {
            let delta = aim.advance(sample(step * 3000), true);
            total[0] += delta[0];
            total[1] += delta[1];
        }
        assert_eq!(total, [-4, 2]);
    }

    #[test]
    fn duplicates_and_gaps_do_not_generate_pointer_jumps() {
        let mut aim = aim(false);
        assert_eq!(aim.advance(sample(0), true), [0, 0]);
        assert_eq!(aim.advance(sample(6000), true), [0, 0]);
        assert_eq!(aim.advance(sample(6000), true), [0, 0]);
        assert_eq!(aim.advance(sample(3000), true), [0, 0]);
        assert_eq!(aim.advance(sample(500_000), true), [0, 0]);
        assert_eq!(aim.advance(sample(503_000), true), [0, 0]);
    }

    #[test]
    fn hold_gate_discards_inactive_motion_and_fractional_carry() {
        let mut aim = aim(false);
        for (ticks, enabled) in [
            (0, false),
            (12_000, false),
            (24_000, true),
            (27_000, true),
            (27_000, false), // release without a new IMU timestamp
            (30_000, false),
            (33_000, true),
            (36_000, true),
            (39_000, true),
        ] {
            assert_eq!(aim.advance(sample(ticks), enabled), [0, 0], "{ticks}");
        }
        assert_eq!(aim.advance(sample(42_000), true), [-1, 0]);
    }

    #[test]
    fn optional_calibration_removes_rest_bias_but_preserves_slow_aim() {
        let mut aim = aim(true);
        let rest = |ms: u32| RawImuSample {
            gyro_counts: [0, 8, 0],
            device_ticks: ms * 3_000,
            ..sample(0)
        };

        for ms in (0..=1000).step_by(10) {
            assert_eq!(aim.advance(rest(ms), true), [0; 2]);
        }
        assert!(aim.calibration.is_none());
        for ms in (1010..=1500).step_by(10) {
            assert_eq!(aim.advance(rest(ms), true), [0; 2]);
        }
        let mut horizontal = 0;
        for ms in (1510..=3000).step_by(10) {
            let mut sample = rest(ms);
            sample.gyro_counts[1] = 12;
            horizontal += aim.advance(sample, true)[0];
        }
        assert_eq!(horizontal, -1);
    }
}
