//! Deterministic gyro and Flick Stick composition for relative pointer output.

use std::time::Duration;

use joyshockrust_core::{Controls, StickSide};
use joyshockrust_input::{FactoryCalibration, ImuStream, RawImuSample, TimedImu, TimestampUpdate};

use crate::{
    AimMode, Calibration, ImuSample, MotionConfig, MotionEngine, StationaryCalibration,
    aim_response::{DEFAULT_FLICK_DURATION_MS, DEFAULT_FLICK_ENGAGE_PERCENT, GyroResponse},
    flick_stick::FlickStick,
};

/// Relative pointer counts in horizontal and vertical order.
pub type MouseDelta = [i32; 2];

#[derive(Clone, Copy)]
pub struct PointerOptions {
    pub mode: AimMode,
    pub response: GyroResponse,
    /// Effective horizontal and vertical sensitivity, including game calibration.
    pub sensitivity: [f64; 2],
    pub flick: Option<FlickOptions>,
}

#[derive(Clone, Copy)]
pub struct FlickOptions {
    /// Effective mouse counts per degree, including game calibration.
    pub scale: f64,
    pub side: StickSide,
    pub engage_percent: Option<f64>,
    pub duration_ms: Option<f64>,
    pub axial_threshold_deg: f64,
    pub steering_sensitivity: f64,
}

impl FlickOptions {
    fn build(self) -> FlickStick {
        FlickStick::new(self.scale)
            .with_timing(
                self.engage_percent.unwrap_or(DEFAULT_FLICK_ENGAGE_PERCENT),
                self.duration_ms.unwrap_or(DEFAULT_FLICK_DURATION_MS),
            )
            .with_axial_threshold(self.axial_threshold_deg)
            .with_steering_sensitivity(self.steering_sensitivity)
    }

    fn keeps_timing(self, next: Self) -> bool {
        self.side == next.side
            && self.engage_percent == next.engage_percent
            && self.duration_ms == next.duration_ms
    }
}

pub struct PointerMotion {
    input: ImuStream,
    gyro: GyroAim,
    flick: Option<(FlickOptions, FlickStick)>,
}

impl PointerMotion {
    /// Start a session with a controller's factory calibration and validated pointer options.
    pub fn new(
        factory: FactoryCalibration,
        calibrate: bool,
        options: PointerOptions,
        stored: Option<Calibration>,
    ) -> Self {
        let max_forward_ticks =
            joyshockrust_input::ticks_in_nanoseconds(MotionConfig::default().max_gap_ns) as u32;
        Self {
            input: ImuStream::new(factory, max_forward_ticks).expect("valid sensor gap limit"),
            gyro: GyroAim::new(calibrate, options.sensitivity, stored)
                .with_response(options.response)
                .with_mode(options.mode),
            flick: options.flick.map(|flick| (flick, flick.build())),
        }
    }

    pub fn reconfigure(&mut self, options: PointerOptions) {
        self.gyro.set_mode(options.mode);
        self.gyro.set_sensitivity(options.sensitivity);
        if self.gyro.response != options.response {
            self.gyro.set_response(options.response);
        }
        self.set_flick(options.flick);
    }

    fn set_flick(&mut self, options: Option<FlickOptions>) {
        if let (Some((previous, flick)), Some(next)) = (&mut self.flick, options)
            && previous.keeps_timing(next)
        {
            flick.set_counts_per_degree(next.scale);
            flick.set_axial_threshold(next.axial_threshold_deg);
            flick.set_steering_sensitivity(next.steering_sensitivity);
            *previous = next;
        } else {
            self.flick = options.map(|flick| (flick, flick.build()));
        }
    }

    pub fn is_calibrating(&self) -> bool {
        self.gyro.calibration.is_some()
    }

    /// Clear prior movement while waiting for the controller to settle.
    pub fn prepare_calibration(&mut self) {
        self.cancel_calibration();
        self.gyro.was_enabled = false;
    }

    pub fn start_calibration(&mut self) {
        self.gyro.calibration = Some(StationaryCalibration::default());
    }

    pub fn cancel_calibration(&mut self) {
        self.gyro.calibration = None;
        self.gyro.reset_motion();
    }

    pub fn calibration(&self) -> Calibration {
        self.gyro.motion.calibration()
    }

    pub fn factory_calibration(&self) -> FactoryCalibration {
        self.input.factory_calibration()
    }

    pub fn take_completed_bias(&mut self) -> Option<Calibration> {
        self.gyro.completed_bias.take()
    }

    pub fn clear_projection(&mut self) {
        self.gyro.projected_interval = None;
    }

    pub fn projected_interval(&self) -> Option<([f32; 2], Duration)> {
        self.gyro.projected_interval
    }

    /// The caller supplies routing for every report, including repeated timestamps.
    pub fn advance(
        &mut self,
        sample: RawImuSample,
        controls: &Controls,
        gyro_enabled: bool,
        flick_enabled: bool,
    ) -> MouseDelta {
        let was_calibrating = self.is_calibrating();
        let timed = self.input.observe(sample);
        let mut movement = self.gyro.advance(timed, gyro_enabled);
        // Keep independent carry and clock anchoring. The flick path can be
        // suppressed without observing reports, such as during calibration.
        if !was_calibrating
            && flick_enabled
            && let Some((options, flick)) = &mut self.flick
        {
            let stick = options
                .side
                .select(controls.left_stick, controls.right_stick);
            movement[0] += flick.advance(stick, sample.device_ticks);
        }
        movement
    }
}
struct GyroAim {
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
    fn new(calibrate: bool, sensitivity: [f64; 2], stored: Option<Calibration>) -> Self {
        let config = Self::config();
        Self {
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

    fn advance(&mut self, sample: TimedImu<'_>, enabled: bool) -> [i32; 2] {
        // Only emit motion for an interval that starts and ends with aim
        // enabled. Apply button transitions even on repeated IMU timestamps.
        let emit_motion = self.was_enabled && enabled;
        self.was_enabled = enabled;
        if !emit_motion {
            self.remainder = [0.0; 2];
        }
        let (elapsed, interval) = match sample.timing {
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
            let calibrated = sample.calibrated();
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
                .process_gyro_aim(timestamp_ns, sample.gyro_dps()),
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
    use crate::aim_response::{
        DEFAULT_FLICK_AXIAL_THRESHOLD_DEG, DEFAULT_FLICK_STEERING_SENSITIVITY,
    };
    use joyshockrust_core::Stick;

    impl PointerMotion {
        fn advance_gyro(&mut self, sample: RawImuSample, enabled: bool) -> MouseDelta {
            self.advance(sample, &Controls::default(), enabled, false)
        }
    }

    fn aim(calibrate: bool) -> PointerMotion {
        aim_with_bias(calibrate, None)
    }

    fn aim_with_bias(calibrate: bool, bias: Option<Calibration>) -> PointerMotion {
        PointerMotion::new(
            factory(),
            calibrate,
            PointerOptions {
                sensitivity: [4.0, 4.0],
                mode: AimMode::Gyro,
                response: GyroResponse::default(),
                flick: None,
            },
            bias,
        )
    }

    fn factory() -> FactoryCalibration {
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
        FactoryCalibration::from_report(&report).unwrap()
    }

    fn sample(device_ticks: u32) -> RawImuSample {
        RawImuSample {
            gyro_counts: [-800, 1600, 0],
            accel_counts: [0, 0, 8192],
            device_ticks,
        }
    }

    #[test]
    fn combined_pointer_replay() {
        let mut aim = aim(false);
        aim.gyro
            .set_response(GyroResponse::new(0.2, 10.0, 120.0, 2.0).unwrap());
        let options = FlickOptions {
            scale: 0.75,
            side: StickSide::Right,
            engage_percent: Some(90.0),
            duration_ms: Some(40.0),
            axial_threshold_deg: DEFAULT_FLICK_AXIAL_THRESHOLD_DEG,
            steering_sensitivity: DEFAULT_FLICK_STEERING_SENSITIVITY,
        };
        aim.set_flick(Some(options));
        let center = Stick::default();
        let right = Stick { x: 1.0, y: 0.0 };
        let down = Stick { x: 0.0, y: 1.0 };
        let start = u32::MAX - 45_000;
        let trace = [
            (0, true, center),
            (30_000, true, right),
            (60_000, true, down),
            (60_000, false, center),
            (90_000, true, down),
            (120_000, true, center),
            (119_997, false, down),
            (150_000, true, right),
            (180_000, true, right),
            (480_000, true, right),
            (510_000, true, center),
            (540_000, true, down),
            (570_000, true, down),
        ];
        let deltas = trace.map(|(offset, enabled, stick)| {
            let ticks = start.wrapping_add(offset);
            let controls = Controls {
                right_stick: stick,
                ..Controls::default()
            };
            aim.advance(sample(ticks), &controls, enabled, true)
        });
        assert_eq!(
            deltas,
            [
                [0, 0],
                [-7, 3],
                [42, 4],
                [0, 0],
                [17, 0],
                [10, 3],
                [0, 0],
                [17, 0],
                [10, 3],
                [0, 0],
                [-7, 3],
                [-8, 4],
                [25, 4],
            ]
        );
    }

    #[test]
    fn fractional_motion_accumulates_in_pointer_direction_on_both_axes() {
        let mut aim = aim(false);
        assert_eq!(aim.advance_gyro(sample(0), true), [0, 0]);
        let mut total = [0, 0];
        let mut batches = 0;
        let allocations = allocation_counter::measure(|| {
            for step in 1..=10 {
                let delta = aim.advance_gyro(sample(step * 3000), true);
                total[0] += delta[0];
                total[1] += delta[1];
                if delta != [0, 0] {
                    batches += 1;
                }
            }
        });
        assert_eq!(total, [-4, 2]);
        assert!(batches > 0);
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }

    #[test]
    fn gyro_response_gates_small_motion_and_boosts_both_axes() {
        let mut aim = aim(false);
        aim.gyro
            .set_response(GyroResponse::new(4.0, 10.0, 100.0, 2.0).unwrap());
        let mut slow = sample(0);
        slow.gyro_counts = [0, 16, 0]; // 1 °/s yaw
        for step in 0..=10 {
            slow.device_ticks = step * 3000;
            assert_eq!(aim.advance_gyro(slow, true), [0, 0]);
        }
        // A gap starts the fast segment without carrying an interval over.
        assert_eq!(aim.advance_gyro(sample(300_000), true), [0, 0]);
        let mut total = [0, 0];
        for step in 1..=10 {
            let delta = aim.advance_gyro(sample(300_000 + step * 3000), true);
            total[0] += delta[0];
            total[1] += delta[1];
        }
        assert_eq!(total, [-8, 4]);
    }

    #[test]
    fn acceleration_preserves_balanced_small_motion_with_uneven_report_timing() {
        let mut aim = aim(false);
        aim.gyro
            .set_response(GyroResponse::new(0.2, 0.2, 1.0, 4.0).unwrap());
        aim.gyro.set_sensitivity([200.0, 200.0]);
        let sample = |ms: u32, yaw| RawImuSample {
            gyro_counts: [0, yaw, 0],
            device_ticks: ms * 3_000,
            ..sample(0)
        };
        assert_eq!(aim.advance_gyro(sample(0, 0), true), [0, 0]);
        // Equal positive and negative rotations, with the fast end of the
        // interval alternating between the short and long reporting gaps.
        let mut total = 0;
        for start in (0..1000).step_by(50) {
            for (offset, yaw) in [(5, 16), (25, 0), (45, -16), (50, 0)] {
                total += aim.advance_gyro(sample(start + offset, yaw), true)[0];
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
        let gyro_bias_dps = factory().convert(rest).gyro_dps;
        let mut aim = aim_with_bias(false, Some(Calibration { gyro_bias_dps }));
        let turn = |ms: u32, yaw| RawImuSample {
            gyro_counts: [0, 0, yaw],
            device_ticks: ms * 3000,
            ..rest
        };

        assert_eq!(aim.advance_gyro(turn(0, 1608), true), [0; 2]);
        assert_eq!(aim.advance_gyro(turn(10, 1608), true), [0; 2]);
        aim.gyro.set_mode(AimMode::SensorFusion);
        assert_eq!(aim.advance_gyro(turn(20, 1608), true), [0; 2]);
        assert_eq!(aim.advance_gyro(turn(30, 1608), true), [-4, 0]);
        aim.advance_gyro(turn(40, 8), true);
        for ms in (50..=2050).step_by(10) {
            assert_eq!(aim.advance_gyro(turn(ms, 8), true), [0; 2]);
        }
        aim.gyro.set_mode(AimMode::Gyro);
        assert_eq!(aim.advance_gyro(turn(2060, 1608), true), [0; 2]);
        assert_eq!(aim.advance_gyro(turn(2070, 1608), true), [0; 2]);
    }

    #[test]
    fn changing_one_sensitivity_does_not_change_other_axis_or_restart_motion() {
        let mut aim = aim(false);
        assert_eq!(aim.advance_gyro(sample(0), true), [0, 0]);
        let mut total = [0, 0];
        for step in 1..=9 {
            let delta = aim.advance_gyro(sample(step * 3000), true);
            total[0] += delta[0];
            total[1] += delta[1];
        }
        assert_eq!(total, [-3, 1]);
        aim.gyro.set_sensitivity([20.0, 4.0]);
        // X emits at the new scale immediately. Y retains its fractional carry.
        assert_eq!(aim.advance_gyro(sample(30_000), true), [-2, 1]);
    }

    #[test]
    fn duplicates_and_gaps_do_not_generate_pointer_jumps() {
        let mut aim = aim(false);
        assert_eq!(aim.advance_gyro(sample(0), true), [0, 0]);
        assert_eq!(aim.advance_gyro(sample(6000), true), [0, 0]);
        assert_eq!(aim.advance_gyro(sample(6000), true), [0, 0]);
        assert_eq!(aim.advance_gyro(sample(3000), true), [0, 0]);
        assert_eq!(aim.advance_gyro(sample(500_000), true), [0, 0]);
        assert_eq!(aim.advance_gyro(sample(503_000), true), [0, 0]);
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
            assert_eq!(aim.advance_gyro(sample(ticks), enabled), [0, 0], "{ticks}");
        }
        assert_eq!(aim.advance_gyro(sample(42_000), true), [-1, 0]);
    }

    #[test]
    fn saved_controller_bias_is_applied_on_open() {
        let raw = RawImuSample {
            gyro_counts: [0, 8, 0],
            ..sample(0)
        };
        let gyro_bias_dps = factory().convert(raw).gyro_dps;
        let mut corrected = aim_with_bias(true, Some(Calibration { gyro_bias_dps }));
        assert!(!corrected.is_calibrating());
        let mut uncorrected = aim(false);
        let mut horizontal = 0;
        for ms in (0..=2000).step_by(10) {
            let sample = RawImuSample {
                device_ticks: ms * 3_000,
                ..raw
            };
            assert_eq!(corrected.advance_gyro(sample, true), [0; 2]);
            horizontal += uncorrected.advance_gyro(sample, true)[0];
        }
        assert!(horizontal < 0);
    }

    #[test]
    fn small_residual_drift_after_saved_calibration_does_not_move_the_pointer() {
        let rest = RawImuSample {
            gyro_counts: [0, 8, 0],
            ..sample(0)
        };
        let gyro_bias_dps = factory().convert(rest).gyro_dps;
        let mut aim = aim_with_bias(false, Some(Calibration { gyro_bias_dps }));
        aim.gyro.set_sensitivity([200.0, 200.0]);

        for ms in (0..=5000).step_by(10) {
            let drift = RawImuSample {
                // 0.125 degrees per second above the saved bias.
                gyro_counts: [0, 10, 0],
                device_ticks: ms * 3000,
                ..rest
            };
            assert_eq!(aim.advance_gyro(drift, true), [0; 2]);
        }
        let deliberate_turn = RawImuSample {
            // 0.25 degrees per second above the saved bias.
            gyro_counts: [0, 12, 0],
            ..rest
        };
        let horizontal: i32 = (5010..=5100)
            .step_by(10)
            .map(|ms| {
                aim.advance_gyro(
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
            assert_eq!(aim.advance_gyro(rest(ms), true), [0; 2]);
        }
        assert!(aim.is_calibrating());
        assert_eq!(aim.advance_gyro(rest(5000), true), [0; 2]);
        assert!(!aim.is_calibrating());
        for ms in (5010..=5500).step_by(10) {
            assert_eq!(aim.advance_gyro(rest(ms), true), [0; 2]);
        }
        let mut horizontal = 0;
        for ms in (5510..=7000).step_by(10) {
            let mut sample = rest(ms);
            sample.gyro_counts[1] = 12;
            horizontal += aim.advance_gyro(sample, true)[0];
        }
        assert_eq!(horizontal, -1);
    }
}
