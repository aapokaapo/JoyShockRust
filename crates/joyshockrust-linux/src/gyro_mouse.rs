use std::{fs::File, io, time::Duration};

use evdev::{
    AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode, RelativeAxisCode as Axis,
    uinput::VirtualDevice,
};
use joyshockrust_input::{RawImuSample, Timebase, TimestampUpdate, UsbFactoryCalibration};
use joyshockrust_motion::{Calibration, ImuSample, MotionConfig, MotionEngine};

pub struct GyroMouse {
    aim: GyroAim,
    output: VirtualDevice,
}

impl GyroMouse {
    pub fn open(input: &File) -> io::Result<Self> {
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
            aim: GyroAim::new(factory),
            output,
        })
    }

    pub fn publish(&mut self, sample: RawImuSample) -> io::Result<()> {
        let mut events = [InputEvent::new(0, 0, 0); 2];
        let mut count = 0;
        for (axis, delta) in [Axis::REL_X, Axis::REL_Y]
            .into_iter()
            .zip(self.aim.advance(sample))
        {
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
    remainder: [f64; 2],
}

impl GyroAim {
    fn new(factory: UsbFactoryCalibration) -> Self {
        let config = MotionConfig {
            sensitivity_x: 4.0,
            sensitivity_y: 4.0,
            ..MotionConfig::default()
        };
        // The sensor clock advances at three ticks per microsecond.
        let max_gap_ticks = (config.max_gap_ns * 3 / 1_000) as u32;
        Self {
            factory,
            timebase: Timebase::new(max_gap_ticks).expect("valid default sensor gap limit"),
            motion: MotionEngine::new(config, Calibration::default())
                .expect("valid gyro mouse defaults"),
            remainder: [0.0; 2],
        }
    }

    fn advance(&mut self, sample: RawImuSample) -> [i32; 2] {
        let elapsed = match self.timebase.observe(sample.device_ticks) {
            TimestampUpdate::FirstSample => Duration::ZERO,
            TimestampUpdate::Advanced { elapsed, .. } => elapsed,
            TimestampUpdate::Gap { .. } => {
                self.reset_motion();
                Duration::ZERO
            }
            TimestampUpdate::Repeated { .. } | TimestampUpdate::OutOfOrder { .. } => return [0; 2],
        };
        let calibrated = self.factory.convert(sample);
        let Ok(output) = self.motion.process(ImuSample {
            timestamp_ns: elapsed.as_nanos() as u64,
            gyro_dps: calibrated.gyro_dps,
            accel_g: calibrated.accel_g,
        }) else {
            self.reset_motion();
            return [0; 2];
        };

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

    fn aim() -> GyroAim {
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
        GyroAim::new(UsbFactoryCalibration::from_report(&report).unwrap())
    }

    fn sample(device_ticks: u32) -> RawImuSample {
        RawImuSample {
            gyro_counts: [-800, 1600, 0],
            accel_counts: [0, 0, 8192],
            device_ticks,
        }
    }

    #[test]
    fn fractional_motion_accumulates_on_both_axes() {
        let mut aim = aim();
        assert_eq!(aim.advance(sample(0)), [0, 0]);
        let mut total = [0, 0];
        for step in 1..=10 {
            let delta = aim.advance(sample(step * 3000));
            total[0] += delta[0];
            total[1] += delta[1];
        }
        assert_eq!(total, [4, -2]);
    }

    #[test]
    fn duplicates_and_gaps_do_not_generate_pointer_jumps() {
        let mut aim = aim();
        assert_eq!(aim.advance(sample(0)), [0, 0]);
        assert_eq!(aim.advance(sample(6000)), [0, 0]);
        assert_eq!(aim.advance(sample(6000)), [0, 0]);
        assert_eq!(aim.advance(sample(3000)), [0, 0]);
        assert_eq!(aim.advance(sample(500_000)), [0, 0]);
        assert_eq!(aim.advance(sample(503_000)), [0, 0]);
    }
}
