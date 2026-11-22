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
    FactoryCalibration, InputReport, RawImuSample, Timebase, TimestampUpdate, Transport,
};
use joyshockrust_motion::{
    Calibration, ImuSample, MotionConfig, MotionEngine, StationaryCalibration,
};

use super::{
    flick_stick::FlickStick,
    game_calibration::GameCalibration,
    gyro_gate::{Binding, GyroGate},
};

pub struct GyroMouse {
    aim: GyroAim,
    output: VirtualDevice,
    gate: GyroGate,
    flick: Option<FlickStick>,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub struct Options {
    pub calibrate: bool,
    pub binding: Binding,
    pub flick_stick: Option<f64>,
    pub sensitivity: Sensitivity,
    pub game_calibration: GameCalibration,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sensitivity([f32; 2]);

impl Default for Sensitivity {
    fn default() -> Self {
        Self([4.0; 2])
    }
}

impl Sensitivity {
    pub fn from_profile(x: Option<f32>, y: Option<f32>) -> Result<Self, &'static str> {
        let [default_x, default_y] = Self::default().0;
        let scales = [x.unwrap_or(default_x), y.unwrap_or(default_y)];
        if !(0.0..=100.0).contains(&scales[0]) {
            return Err("gyro.sensitivity_x must be between 0 and 100 counts per degree");
        }
        if !(0.0..=100.0).contains(&scales[1]) {
            return Err("gyro.sensitivity_y must be between 0 and 100 counts per degree");
        }
        Ok(Self(scales))
    }
}

impl Options {
    fn gyro_scale(self) -> [f64; 2] {
        self.sensitivity
            .0
            .map(|scale| self.game_calibration.scale(f64::from(scale)))
    }

    fn flick_scale(self) -> Option<f64> {
        self.flick_stick
            .map(|scale| self.game_calibration.scale(scale))
    }
}

pub fn validate_flick_scale(scale: f64) -> Result<f64, &'static str> {
    if !(0.01..=100.0).contains(&scale) {
        return Err("Flick Stick sensitivity must be between 0.01 and 100 counts per degree");
    }
    Ok(scale)
}

pub fn open_output(name: &str) -> io::Result<VirtualDevice> {
    let axes = [Axis::REL_X, Axis::REL_Y]
        .into_iter()
        .collect::<AttributeSet<_>>();
    // Advertise the left button so consumers classify this as a mouse.
    let keys = [KeyCode::BTN_LEFT].into_iter().collect::<AttributeSet<_>>();
    VirtualDevice::builder()?
        .name(name)
        .input_id(InputId::new(BusType::BUS_VIRTUAL, 0, 0, 1))
        .with_keys(&keys)?
        .with_relative_axes(&axes)?
        .build()
}

impl GyroMouse {
    pub fn open(input: &File, transport: Transport, options: Options) -> io::Result<Self> {
        let factory = super::hidraw::read_factory_calibration(input, transport).map_err(|err| {
            io::Error::other(format!("could not read factory calibration: {err}"))
        })?;
        Ok(Self {
            aim: GyroAim::new(factory, options.calibrate, options.gyro_scale()),
            output: open_output("JoyShockRust Gyro Mouse")?,
            gate: GyroGate::new(options.binding),
            flick: options.flick_scale().map(FlickStick::new),
        })
    }

    pub fn set_scaling(&mut self, options: Options) {
        self.aim.set_sensitivity(options.gyro_scale());
        if let (Some(flick), Some(scale)) = (&mut self.flick, options.flick_scale()) {
            flick.set_counts_per_degree(scale);
        }
    }

    pub fn publish(&mut self, report: InputReport) -> io::Result<bool> {
        let was_calibrating = self.aim.calibration.is_some();
        let enabled = self
            .gate
            .enabled(&report.controls, &report.touch_contacts, Instant::now);
        let mut movement = self.aim.advance(report.imu, enabled);
        if !was_calibrating && let Some(flick) = &mut self.flick {
            movement[0] += flick.advance(report.controls.right_stick, report.imu.device_ticks);
        }
        if was_calibrating && self.aim.calibration.is_none() {
            eprintln!("Gyro rest calibration complete");
        }
        emit_movement(movement, |events| self.output.emit(events))
    }
}

fn emit_movement(
    movement: [i32; 2],
    emit: impl FnOnce(&[InputEvent]) -> io::Result<()>,
) -> io::Result<bool> {
    let mut events = [InputEvent::new(0, 0, 0); 2];
    let mut count = 0;
    for (axis, delta) in [Axis::REL_X, Axis::REL_Y].into_iter().zip(movement) {
        if delta != 0 {
            events[count] = InputEvent::new(EventType::RELATIVE.0, axis.0, delta);
            count += 1;
        }
    }
    if count > 0 {
        emit(&events[..count])?;
    }
    Ok(count > 0)
}

struct GyroAim {
    factory: FactoryCalibration,
    timebase: Timebase,
    motion: MotionEngine,
    calibration: Option<StationaryCalibration>,
    remainder: [f64; 2],
    sensitivity: [f64; 2],
    was_enabled: bool,
}

impl GyroAim {
    fn new(factory: FactoryCalibration, calibrate: bool, sensitivity: [f64; 2]) -> Self {
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
            sensitivity,
            was_enabled: false,
        }
    }

    fn config() -> MotionConfig {
        MotionConfig {
            // Linux pointer directions use the opposite signs to the
            // controller rotations used for horizontal and vertical aim.
            sensitivity_x: -1.0,
            sensitivity_y: -1.0,
            ..MotionConfig::default()
        }
    }

    fn set_sensitivity(&mut self, sensitivity: [f64; 2]) {
        for (axis, remainder) in self.remainder.iter_mut().enumerate() {
            if self.sensitivity[axis] != sensitivity[axis] {
                *remainder = 0.0;
            }
        }
        self.sensitivity = sensitivity;
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
            let total =
                self.remainder[axis] + f64::from(output.aim_delta[axis]) * self.sensitivity[axis];
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

    #[test]
    fn mouse_batch_requires_movement_and_successful_emission() {
        assert!(!emit_movement([0, 0], |_| panic!("empty batch emitted")).unwrap());
        assert!(
            emit_movement([-4, 2], |events| {
                assert_eq!(events.len(), 2);
                assert_eq!(events[0].code(), Axis::REL_X.0);
                assert_eq!(events[1].code(), Axis::REL_Y.0);
                assert_eq!(events[0].value(), -4);
                assert_eq!(events[1].value(), 2);
                Ok(())
            })
            .unwrap()
        );
        let err = emit_movement([1, 0], |_| Err(io::ErrorKind::BrokenPipe.into())).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

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
            FactoryCalibration::from_report(&report).unwrap(),
            calibrate,
            Options::default().gyro_scale(),
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
        let mut batches = 0;
        let allocations = allocation_counter::measure(|| {
            for step in 1..=10 {
                let delta = aim.advance(sample(step * 3000), true);
                total[0] += delta[0];
                total[1] += delta[1];
                if emit_movement(delta, |events| {
                    std::hint::black_box(events);
                    Ok(())
                })
                .unwrap()
                {
                    batches += 1;
                }
            }
        });
        assert_eq!(total, [-4, 2]);
        assert!(batches > 0);
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }

    #[test]
    fn game_calibration_scales_both_gyro_axes_and_flick_stick() {
        let profile = crate::profile::parse(
            "version = 1\n[gyro]\nsensitivity_x = 1.0\nsensitivity_y = 0.5\nflick_stick = 1.0\nreal_world_calibration = 40.0\nin_game_sens = 2.0",
        ).unwrap();
        let options = profile.gyro.unwrap();
        assert_eq!(options.gyro_scale(), [20.0, 10.0]);
        assert_eq!(options.flick_scale(), Some(20.0));

        let mut aim = aim(false);
        aim.set_sensitivity(options.gyro_scale());
        assert_eq!(aim.advance(sample(0), true), [0, 0]);
        let mut total = [0, 0];
        for step in 1..=10 {
            let delta = aim.advance(sample(step * 3000), true);
            total[0] += delta[0];
            total[1] += delta[1];
        }
        assert_eq!(total, [-20, 5]);
    }

    #[test]
    fn changing_one_sensitivity_does_not_change_other_axis_or_restart_motion() {
        let mut aim = aim(false);
        assert_eq!(aim.advance(sample(0), true), [0, 0]);
        let mut total = [0, 0];
        for step in 1..=9 {
            let delta = aim.advance(sample(step * 3000), true);
            total[0] += delta[0];
            total[1] += delta[1];
        }
        assert_eq!(total, [-3, 1]);
        aim.set_sensitivity([20.0, 4.0]);
        // X emits at the new scale immediately. Y retains its fractional carry.
        assert_eq!(aim.advance(sample(30_000), true), [-2, 1]);
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
