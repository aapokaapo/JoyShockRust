use std::{
    fs::File,
    io,
    time::{Duration, Instant},
};

use evdev::{
    AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode, RelativeAxisCode as Axis,
    uinput::VirtualDevice,
};
use joyshockrust_core::{Button, StickSide};
use joyshockrust_input::{
    FactoryCalibration, InputReport, RawImuSample, Timebase, TimestampUpdate, Transport,
};
use joyshockrust_motion::{
    AimMode, Calibration, ImuSample, MotionConfig, MotionEngine, StationaryCalibration,
    aim_response::{
        DEFAULT_FLICK_AXIAL_THRESHOLD_DEG, DEFAULT_FLICK_DURATION_MS, DEFAULT_FLICK_ENGAGE_PERCENT,
        DEFAULT_FLICK_STEERING_SENSITIVITY, GyroResponse,
    },
};

use super::{
    flick_stick::FlickStick,
    game_calibration::GameCalibration,
    gyro_calibration::BiasWriter,
    gyro_gate::{Binding, GyroGate},
    haptics::{Cue, Haptics},
};

pub struct GyroMouse {
    aim: GyroAim,
    output: VirtualDevice,
    gate: GyroGate,
    flick: Option<(FlickStick, StickSide)>,
    hold: Option<HoldDetector>,
    shortcut: Option<ShortcutCalibration>,
    haptics: Option<Haptics>,
    bias_writer: Option<BiasWriter>,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub struct Options {
    pub calibrate: bool,
    pub aim_mode: AimMode,
    pub calibration_hold: Option<HoldBinding>,
    pub binding: Binding,
    pub flick_stick: Option<f64>,
    pub flick_stick_side: StickSide,
    pub flick_engage_percent: Option<f64>,
    pub flick_duration_ms: Option<f64>,
    pub flick_axial_threshold_deg: Option<f64>,
    pub flick_steering_sensitivity: Option<f64>,
    pub response: GyroResponse,
    pub sensitivity: Sensitivity,
    pub game_calibration: GameCalibration,
}

#[derive(Clone, Copy, PartialEq)]
pub struct HoldBinding {
    pub button: Button,
    pub duration: Duration,
}

struct HoldDetector {
    binding: HoldBinding,
    pressed_since: Option<Instant>,
    fired: bool,
}

impl HoldDetector {
    fn new(binding: HoldBinding) -> Self {
        Self {
            binding,
            pressed_since: None,
            fired: true,
        }
    }

    fn observe(&mut self, pressed: bool, now: Instant) -> bool {
        if !pressed {
            self.pressed_since = None;
            self.fired = false;
            return false;
        }
        if self.fired {
            return false;
        }
        if now.duration_since(*self.pressed_since.get_or_insert(now)) >= self.binding.duration {
            self.fired = true;
            return true;
        }
        false
    }
}

struct ShortcutCalibration {
    settle_at: Instant,
    deadline: Instant,
    measuring: bool,
}

pub use joyshockrust_motion::sensitivity::GyroSensitivity as Sensitivity;

impl Options {
    fn gyro_scale(self) -> [f64; 2] {
        self.sensitivity
            .axes()
            .map(|scale| self.game_calibration.scale(f64::from(scale)))
    }

    pub fn flick_axial_threshold(self) -> f64 {
        self.flick_axial_threshold_deg
            .unwrap_or(DEFAULT_FLICK_AXIAL_THRESHOLD_DEG)
    }

    fn flick_steering_sensitivity(self) -> f64 {
        self.flick_steering_sensitivity
            .unwrap_or(DEFAULT_FLICK_STEERING_SENSITIVITY)
    }

    fn flick_scale(self) -> Option<f64> {
        self.flick_stick
            .map(|scale| self.game_calibration.scale(scale))
    }

    fn create_flick(self) -> Option<(FlickStick, StickSide)> {
        self.flick_scale().map(|scale| {
            (
                FlickStick::new(scale)
                    .with_timing(
                        self.flick_engage_percent
                            .unwrap_or(DEFAULT_FLICK_ENGAGE_PERCENT),
                        self.flick_duration_ms.unwrap_or(DEFAULT_FLICK_DURATION_MS),
                    )
                    .with_axial_threshold(self.flick_axial_threshold())
                    .with_steering_sensitivity(self.flick_steering_sensitivity()),
                self.flick_stick_side,
            )
        })
    }

    pub fn can_reconfigure(self, next: Self) -> bool {
        self.calibrate == next.calibrate
            && self.calibration_hold == next.calibration_hold
            && self.binding == next.binding
    }
}

pub fn validate_flick_scale(scale: f64) -> Result<f64, &'static str> {
    if !(0.01..=100.0).contains(&scale) {
        return Err("Flick Stick sensitivity must be between 0.01 and 100 counts per degree");
    }
    Ok(scale)
}

pub fn open_output(name: &str) -> io::Result<VirtualDevice> {
    open_output_with_buttons(name, AttributeSet::new(), false)
}

pub(crate) fn open_output_with_buttons(
    name: &str,
    mut keys: AttributeSet<KeyCode>,
    scroll: bool,
) -> io::Result<VirtualDevice> {
    let mut axes = [Axis::REL_X, Axis::REL_Y]
        .into_iter()
        .collect::<AttributeSet<_>>();
    if scroll {
        axes.insert(Axis::REL_WHEEL);
        axes.insert(Axis::REL_HWHEEL);
    }
    // Advertise the left button so consumers classify this as a mouse.
    keys.insert(KeyCode::BTN_LEFT);
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
        let stored = super::gyro_calibration::load(input);
        let output = open_output("JoyShockRust Gyro Mouse")?;
        let bias_writer = if options.calibration_hold.is_some() {
            BiasWriter::open(input).unwrap_or_else(|error| {
                eprintln!("Could not prepare controller gyro calibration storage: {error}");
                None
            })
        } else {
            None
        };
        if stored.is_some() {
            eprintln!("Applied saved controller gyro calibration");
        }
        Ok(Self {
            aim: GyroAim::new(factory, options.calibrate, options.gyro_scale(), stored)
                .with_response(options.response)
                .with_mode(options.aim_mode),
            output,
            hold: options.calibration_hold.map(HoldDetector::new),
            shortcut: None,
            haptics: Haptics::open_controller(input, transport),
            bias_writer,
            gate: GyroGate::new(options.binding),
            flick: options.create_flick(),
        })
    }

    pub fn signal_outputs_ready(&self) {
        // A startup calibration announces readiness with its completion cue.
        if self.aim.calibration.is_none()
            && let Some(haptics) = &self.haptics
        {
            haptics.send(Cue::OutputsReady);
        }
    }

    pub fn reconfigure(&mut self, current: Options, options: Options) {
        self.aim.set_mode(options.aim_mode);
        self.aim.set_sensitivity(options.gyro_scale());
        if self.aim.response != options.response {
            self.aim.set_response(options.response);
        }
        if current.flick_stick.is_some() != options.flick_stick.is_some()
            || current.flick_stick_side != options.flick_stick_side
            || current.flick_engage_percent != options.flick_engage_percent
            || current.flick_duration_ms != options.flick_duration_ms
        {
            self.flick = options.create_flick();
        } else if let (Some((flick, _)), Some(scale)) = (&mut self.flick, options.flick_scale()) {
            flick.set_counts_per_degree(scale);
            flick.set_axial_threshold(options.flick_axial_threshold());
            flick.set_steering_sensitivity(options.flick_steering_sensitivity());
        }
    }

    pub fn factory_calibration(&self) -> FactoryCalibration {
        self.aim.factory
    }

    pub fn stationary_calibration(&self) -> Calibration {
        self.aim.motion.calibration()
    }

    pub fn projected_interval(&self) -> Option<([f32; 2], Duration)> {
        self.aim.projected_interval
    }

    pub fn publish(&mut self, report: InputReport) -> io::Result<bool> {
        let mut suppress_flick = false;
        if let Some(hold) = &mut self.hold {
            let pressed = report.controls.buttons.contains(hold.binding.button);
            let now = Instant::now();
            if hold.observe(pressed, now) && self.shortcut.is_none() {
                self.shortcut = Some(ShortcutCalibration {
                    settle_at: now + Duration::from_millis(500),
                    deadline: now + Duration::from_secs(15),
                    measuring: false,
                });
                self.aim.calibration = None;
                self.aim.reset_motion();
                self.aim.was_enabled = false;
                if let Some(haptics) = &self.haptics {
                    haptics.send(Cue::CalibrationStart);
                }
                eprintln!(
                    "Gyro calibration requested. Release the button and keep the controller still for {} seconds.",
                    StationaryCalibration::SAMPLE_DURATION.as_secs(),
                );
            }
            if let Some(shortcut) = &mut self.shortcut {
                suppress_flick = true;
                if now >= shortcut.deadline {
                    self.shortcut = None;
                    self.aim.calibration = None;
                    self.aim.reset_motion();
                    eprintln!("Gyro calibration timed out. Previous calibration retained.");
                    return Ok(false);
                }
                if !shortcut.measuring {
                    if pressed || now < shortcut.settle_at {
                        self.aim.projected_interval = None;
                        return Ok(false);
                    }
                    shortcut.measuring = true;
                    self.aim.calibration = Some(StationaryCalibration::default());
                }
            }
        }
        let was_calibrating = self.aim.calibration.is_some();
        let enabled = self
            .gate
            .enabled(&report.controls, &report.touch_contacts, Instant::now);
        let mut movement = self.aim.advance(report.imu, enabled);
        if !was_calibrating
            && !suppress_flick
            && let Some((flick, side)) = &mut self.flick
        {
            let stick = side.select(report.controls.left_stick, report.controls.right_stick);
            movement[0] += flick.advance(stick, report.imu.device_ticks);
        }
        if let Some(bias) = self.aim.completed_bias.take() {
            if self.shortcut.take().is_some()
                && let Some(writer) = &self.bias_writer
            {
                writer.send(bias);
            }
            if let Some(haptics) = &self.haptics {
                haptics.send(Cue::GyroCalibrationDone);
            }
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
    mode: AimMode,
    calibration: Option<StationaryCalibration>,
    completed_bias: Option<Calibration>,
    remainder: [f64; 2],
    sensitivity: [f64; 2],
    response: GyroResponse,
    projected_interval: Option<([f32; 2], Duration)>,
    was_enabled: bool,
}

impl GyroAim {
    fn new(
        factory: FactoryCalibration,
        calibrate: bool,
        sensitivity: [f64; 2],
        stored: Option<Calibration>,
    ) -> Self {
        let config = Self::config();
        // The sensor clock advances at three ticks per microsecond.
        let max_gap_ticks = joyshockrust_input::ticks_in_nanoseconds(config.max_gap_ns) as u32;
        Self {
            factory,
            timebase: Timebase::new(max_gap_ticks).expect("valid default sensor gap limit"),
            motion: MotionEngine::new(config, stored.unwrap_or_default())
                .expect("valid gyro mouse defaults"),
            mode: AimMode::default(),
            calibration: (calibrate && stored.is_none()).then(StationaryCalibration::default),
            completed_bias: None,
            remainder: [0.0; 2],
            sensitivity,
            response: GyroResponse::default(),
            projected_interval: None,
            was_enabled: false,
        }
    }

    fn with_mode(mut self, mode: AimMode) -> Self {
        self.set_mode(mode);
        self
    }

    fn set_mode(&mut self, mode: AimMode) {
        if self.mode != mode {
            self.mode = mode;
            self.reset_motion();
            self.was_enabled = false;
        }
    }

    fn with_response(mut self, response: GyroResponse) -> Self {
        self.set_response(response);
        self
    }

    fn set_response(&mut self, response: GyroResponse) {
        self.response = response;
        self.remainder = [0.0; 2];
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
        let (elapsed, interval) = match self.timebase.observe(sample.device_ticks) {
            TimestampUpdate::FirstSample => (Duration::ZERO, Duration::ZERO),
            TimestampUpdate::Advanced { elapsed, delta, .. } => (elapsed, delta),
            TimestampUpdate::Gap { .. } => {
                self.reset_motion();
                if let Some(calibration) = &mut self.calibration {
                    calibration.reset();
                }
                (Duration::ZERO, Duration::ZERO)
            }
            TimestampUpdate::Repeated { .. } | TimestampUpdate::OutOfOrder { .. } => return [0; 2],
        };
        let timestamp_ns = elapsed.as_nanos() as u64;
        self.projected_interval = None;
        let calibrated_sample = || {
            let calibrated = self.factory.convert(sample);
            ImuSample {
                timestamp_ns,
                gyro_dps: calibrated.gyro_dps,
                accel_g: calibrated.accel_g,
            }
        };
        if let Some(calibration) = &mut self.calibration {
            if let Some(bias) = calibration.observe(calibrated_sample()) {
                self.motion = MotionEngine::new(Self::config(), bias)
                    .expect("stationary calibration returns a bounded finite bias");
                self.calibration = None;
                self.completed_bias = Some(bias);
            }
            return [0; 2];
        }
        let aim_delta = match self.mode {
            AimMode::Gyro => self
                .motion
                .process_gyro_aim(timestamp_ns, self.factory.convert_gyro(sample.gyro_counts)),
            AimMode::SensorFusion => self.motion.process_fused_aim(calibrated_sample()),
        };
        let Ok(aim_delta) = aim_delta else {
            self.reset_motion();
            return [0; 2];
        };
        if !interval.is_zero() {
            self.projected_interval = Some((aim_delta, interval));
        }

        if !emit_motion {
            return [0; 2];
        }

        // Match the speed used for the gain to the interval being integrated.
        let gain = self.response.interval_gain(aim_delta, interval);
        if gain == 0.0 {
            self.remainder = [0.0; 2];
            return [0; 2];
        }

        // Retain fractional movement so repeated sub-count deltas still move
        // the pointer. Clear these fractions on discontinuities.
        std::array::from_fn(|axis| {
            let total =
                self.remainder[axis] + f64::from(aim_delta[axis]) * self.sensitivity[axis] * gain;
            let whole = total as i32;
            self.remainder[axis] = total - f64::from(whole);
            whole
        })
    }

    fn reset_motion(&mut self) {
        self.motion.reset_segment();
        self.remainder = [0.0; 2];
        self.projected_interval = None;
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
        aim_with_bias(calibrate, None)
    }

    fn aim_with_bias(calibrate: bool, bias: Option<Calibration>) -> GyroAim {
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
            bias,
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
        let mut flick = FlickStick::new(options.flick_scale().unwrap());
        let center = joyshockrust_core::Stick::default();
        let right = joyshockrust_core::Stick { x: 1.0, y: 0.0 };
        assert_eq!(flick.advance(center, 0), 0);
        assert_eq!(flick.advance(right, 30_000), 0);
        assert_eq!(flick.advance(right, 180_000), 900);
        assert_eq!(flick.advance(center, 330_000), 900);

        let updated = Options {
            game_calibration: GameCalibration::new(40.0, 1.0).unwrap(),
            ..options
        };
        flick.set_counts_per_degree(updated.flick_scale().unwrap());
        assert_eq!(flick.advance(right, 360_000), 0);
        assert_eq!(flick.advance(right, 510_000), 1800);
        assert_eq!(flick.advance(center, 660_000), 1800);

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
    fn gyro_response_gates_small_motion_and_boosts_both_axes() {
        let mut aim = aim(false).with_response(GyroResponse::new(4.0, 10.0, 100.0, 2.0).unwrap());
        let mut slow = sample(0);
        slow.gyro_counts = [0, 16, 0]; // 1 °/s yaw
        for step in 0..=10 {
            slow.device_ticks = step * 3000;
            assert_eq!(aim.advance(slow, true), [0, 0]);
        }
        // A gap starts the fast segment without carrying an interval over.
        assert_eq!(aim.advance(sample(300_000), true), [0, 0]);
        let mut total = [0, 0];
        for step in 1..=10 {
            let delta = aim.advance(sample(300_000 + step * 3000), true);
            total[0] += delta[0];
            total[1] += delta[1];
        }
        assert_eq!(total, [-8, 4]);
    }

    #[test]
    fn acceleration_preserves_balanced_small_motion_with_uneven_report_timing() {
        let mut aim = aim(false).with_response(GyroResponse::new(0.2, 0.2, 1.0, 4.0).unwrap());
        aim.set_sensitivity([200.0, 200.0]);
        let sample = |ms: u32, yaw| RawImuSample {
            gyro_counts: [0, yaw, 0],
            device_ticks: ms * 3_000,
            ..sample(0)
        };
        assert_eq!(aim.advance(sample(0, 0), true), [0, 0]);
        // Equal positive and negative rotations, with the fast end of the
        // interval alternating between the short and long reporting gaps.
        let mut total = 0;
        for start in (0..1000).step_by(50) {
            for (offset, yaw) in [(5, 16), (25, 0), (45, -16), (50, 0)] {
                total += aim.advance(sample(start + offset, yaw), true)[0];
            }
        }
        assert_eq!(total, 0);
    }

    #[test]
    fn aim_mode_changes_pointer_axis_and_rebaselines_with_saved_bias() {
        let rest = RawImuSample {
            gyro_counts: [0, 0, 8],
            ..sample(0)
        };
        let gyro_bias_dps = aim(false).factory.convert(rest).gyro_dps;
        let mut aim = aim_with_bias(false, Some(Calibration { gyro_bias_dps }));
        let turn = |ms: u32, yaw| RawImuSample {
            gyro_counts: [0, 0, yaw],
            device_ticks: ms * 3000,
            ..rest
        };

        assert_eq!(aim.advance(turn(0, 1608), true), [0; 2]);
        assert_eq!(aim.advance(turn(10, 1608), true), [0; 2]);
        aim.set_mode(AimMode::SensorFusion);
        assert_eq!(aim.advance(turn(20, 1608), true), [0; 2]);
        assert_eq!(aim.advance(turn(30, 1608), true), [-4, 0]);
        aim.advance(turn(40, 8), true);
        for ms in (50..=2050).step_by(10) {
            assert_eq!(aim.advance(turn(ms, 8), true), [0; 2]);
        }
        aim.set_mode(AimMode::Gyro);
        assert_eq!(aim.advance(turn(2060, 1608), true), [0; 2]);
        assert_eq!(aim.advance(turn(2070, 1608), true), [0; 2]);
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
    fn calibration_hold_rearms_only_after_release() {
        let mut detector = HoldDetector::new(HoldBinding {
            button: Button::Home,
            duration: Duration::from_millis(1500),
        });
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        assert!(!detector.observe(true, at(0)));
        assert!(!detector.observe(true, at(2000)));
        assert!(!detector.observe(false, at(2100)));
        assert!(!detector.observe(true, at(2200)));
        assert!(!detector.observe(false, at(2300)));
        assert!(!detector.observe(true, at(2400)));
        assert!(!detector.observe(true, at(3899)));
        assert!(detector.observe(true, at(3900)));
        assert!(!detector.observe(true, at(6000)));
        assert!(!detector.observe(false, at(6100)));
        assert!(!detector.observe(true, at(6200)));
        assert!(detector.observe(true, at(7700)));
    }

    #[test]
    fn saved_controller_bias_is_applied_on_open() {
        let raw = RawImuSample {
            gyro_counts: [0, 8, 0],
            ..sample(0)
        };
        let gyro_bias_dps = aim(false).factory.convert(raw).gyro_dps;
        let mut corrected = aim_with_bias(true, Some(Calibration { gyro_bias_dps }));
        assert!(corrected.calibration.is_none());
        let mut uncorrected = aim(false);
        let mut horizontal = 0;
        for ms in (0..=2000).step_by(10) {
            let sample = RawImuSample {
                device_ticks: ms * 3_000,
                ..raw
            };
            assert_eq!(corrected.advance(sample, true), [0; 2]);
            horizontal += uncorrected.advance(sample, true)[0];
        }
        assert!(horizontal < 0);
    }

    #[test]
    fn small_residual_drift_after_saved_calibration_does_not_move_the_pointer() {
        let rest = RawImuSample {
            gyro_counts: [0, 8, 0],
            ..sample(0)
        };
        let gyro_bias_dps = aim(false).factory.convert(rest).gyro_dps;
        let mut aim = aim_with_bias(false, Some(Calibration { gyro_bias_dps }));
        aim.set_sensitivity([200.0, 200.0]);

        for ms in (0..=5000).step_by(10) {
            let drift = RawImuSample {
                // 0.125 degrees per second above the saved bias.
                gyro_counts: [0, 10, 0],
                device_ticks: ms * 3000,
                ..rest
            };
            assert_eq!(aim.advance(drift, true), [0; 2]);
        }
        let deliberate_turn = RawImuSample {
            // 0.25 degrees per second above the saved bias.
            gyro_counts: [0, 12, 0],
            ..rest
        };
        let horizontal: i32 = (5010..=5100)
            .step_by(10)
            .map(|ms| {
                aim.advance(
                    RawImuSample {
                        device_ticks: ms * 3000,
                        ..deliberate_turn
                    },
                    true,
                )[0]
            })
            .sum();
        assert!(horizontal < 0);
    }

    #[test]
    fn optional_calibration_removes_rest_bias_but_preserves_slow_aim() {
        let mut aim = aim(true);
        let rest = |ms: u32| RawImuSample {
            gyro_counts: [0, 8, 0],
            device_ticks: ms * 3_000,
            ..sample(0)
        };

        for ms in (0..5000).step_by(10) {
            assert_eq!(aim.advance(rest(ms), true), [0; 2]);
        }
        assert!(aim.calibration.is_some());
        assert_eq!(aim.advance(rest(5000), true), [0; 2]);
        assert!(aim.calibration.is_none());
        for ms in (5010..=5500).step_by(10) {
            assert_eq!(aim.advance(rest(ms), true), [0; 2]);
        }
        let mut horizontal = 0;
        for ms in (5510..=7000).step_by(10) {
            let mut sample = rest(ms);
            sample.gyro_counts[1] = 12;
            horizontal += aim.advance(sample, true)[0];
        }
        assert_eq!(horizontal, -1);
    }
}
