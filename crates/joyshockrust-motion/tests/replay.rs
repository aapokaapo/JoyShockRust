use joyshockrust_motion::{Calibration, ImuSample, MotionConfig, MotionEngine, MotionError};

const REST: [f32; 3] = [0.0, 0.0, 1.0];

fn sample(ms: u64, gyro_dps: [f32; 3], accel_g: [f32; 3]) -> ImuSample {
    ImuSample {
        timestamp_ns: ms * 1_000_000,
        gyro_dps,
        accel_g,
    }
}

#[test]
fn stationary_bias_is_removed_and_retained_across_reset() {
    let bias = [0.2, -0.3, 0.5];
    let mut motion = MotionEngine::new(
        MotionConfig::default(),
        Calibration {
            gyro_bias_dps: bias,
        },
    )
    .unwrap();
    for ms in [0, 5, 10] {
        let output = motion.process(sample(ms, bias, REST)).unwrap();
        assert_eq!(output.angular_velocity, [0.0; 3]);
        assert_eq!(output.aim_delta, [0.0; 2]);
        assert_eq!(output.gravity, Some(REST));
        assert!(output.stationary);
    }
    motion.reset_segment();
    assert_eq!(
        motion
            .process(sample(0, bias, REST))
            .unwrap()
            .angular_velocity,
        [0.0; 3]
    );
}

#[test]
fn signed_axis_sensitivity_and_trapezoidal_integration_are_deterministic() {
    let config = MotionConfig {
        sensitivity_x: 2.0,
        sensitivity_y: -3.0,
        ..MotionConfig::default()
    };
    let trace = [
        sample(0, [0.0; 3], REST),
        sample(10, [20.0, 100.0, 0.0], REST),
        sample(20, [20.0, 100.0, 0.0], REST),
    ];
    let replay = || {
        let mut motion = MotionEngine::new(config, Calibration::default()).unwrap();
        trace.map(|imu| motion.process(imu).unwrap())
    };
    let frames = replay();
    assert_eq!(frames, replay());
    assert_eq!(
        frames.map(|output| output.aim_delta),
        [[0.0, 0.0], [1.0, -0.3], [2.0, -0.6]]
    );
}

#[test]
fn steady_slow_rotation_continues_to_aim() {
    let mut motion = MotionEngine::new(MotionConfig::default(), Calibration::default()).unwrap();
    let gyro = [0.0, 0.05, 0.0];
    motion.process(sample(0, gyro, REST)).unwrap();
    for ms in 1..=1000 {
        let output = motion.process(sample(ms, gyro, REST)).unwrap();
        assert_eq!(output.angular_velocity, gyro);
        assert!((output.aim_delta[0] - 0.00005).abs() < 1e-9);
    }
}

#[test]
fn gravity_tracks_rotation_and_ignores_high_linear_acceleration() {
    let mut motion = MotionEngine::new(MotionConfig::default(), Calibration::default()).unwrap();
    let gyro = [0.0, 900.0, 0.0];
    motion.process(sample(0, gyro, REST)).unwrap();
    motion.process(sample(50, gyro, [0.0, 0.0, 2.0])).unwrap();
    let output = motion.process(sample(100, gyro, [0.0, 0.0, 2.0])).unwrap();
    let [x, y, z] = output.gravity.unwrap();
    assert!((x + 1.0).abs() < 1e-6 && y.abs() < 1e-6 && z.abs() < 1e-6);
    assert!(!output.stationary);
}

#[test]
fn gravity_acquires_rest_and_corrects_small_tilt() {
    let mut motion = MotionEngine::new(MotionConfig::default(), Calibration::default()).unwrap();
    assert_eq!(
        motion
            .process(sample(0, [0.0; 3], [0.0; 3]))
            .unwrap()
            .gravity,
        None
    );
    assert_eq!(
        motion.process(sample(10, [0.0; 3], REST)).unwrap().gravity,
        Some(REST)
    );
    for ms in (20..=1000).step_by(10) {
        motion
            .process(sample(ms, [0.0; 3], [0.6, 0.0, 0.8]))
            .unwrap();
    }
    assert!(
        motion
            .process(sample(1010, [0.0; 3], [0.6, 0.0, 0.8]))
            .unwrap()
            .gravity
            .unwrap()[0]
            > 0.5
    );
}

#[test]
fn rejected_samples_preserve_state_and_gaps_can_rebaseline() {
    let mut motion = MotionEngine::new(MotionConfig::default(), Calibration::default()).unwrap();
    let gyro = [0.0, 10.0, 0.0];
    motion.process(sample(10, gyro, REST)).unwrap();
    for (imu, expected) in [
        (sample(10, gyro, REST), MotionError::NonIncreasingTimestamp),
        (sample(9, gyro, REST), MotionError::NonIncreasingTimestamp),
        (
            sample(11, [f32::NAN; 3], REST),
            MotionError::NonFiniteSample,
        ),
        (sample(11, gyro, [5.0; 3]), MotionError::SampleOutOfRange),
        (
            sample(61, gyro, REST),
            MotionError::Gap {
                elapsed_ns: 51_000_000,
            },
        ),
    ] {
        assert_eq!(motion.process(imu), Err(expected));
    }
    assert_eq!(
        motion.process(sample(20, gyro, REST)).unwrap().aim_delta,
        [0.1, 0.0]
    );
    motion.reset_segment();
    assert_eq!(
        motion.process(sample(61, gyro, REST)).unwrap().aim_delta,
        [0.0; 2]
    );
}

#[test]
fn invalid_settings_are_rejected() {
    let bad = MotionConfig {
        sensitivity_y: f32::INFINITY,
        ..MotionConfig::default()
    };
    assert!(matches!(
        MotionEngine::new(bad, Calibration::default()),
        Err(MotionError::InvalidConfig)
    ));
    let bad = Calibration {
        gyro_bias_dps: [20.0, 0.0, 0.0],
    };
    assert!(matches!(
        MotionEngine::new(MotionConfig::default(), bad),
        Err(MotionError::InvalidCalibration)
    ));
}

#[test]
fn gyro_and_fused_aim_use_different_yaw_axes_when_the_controller_is_flat() {
    let mut gyro = MotionEngine::new(MotionConfig::default(), Calibration::default()).unwrap();
    let mut fused = MotionEngine::new(MotionConfig::default(), Calibration::default()).unwrap();
    let spin_about_world_up = [0.0, 0.0, 100.0];
    assert_eq!(
        gyro.process_gyro_aim(0, spin_about_world_up).unwrap(),
        [0.0; 2]
    );
    assert_eq!(
        fused
            .process_fused_aim(sample(0, spin_about_world_up, REST))
            .unwrap(),
        [0.0; 2]
    );
    assert_eq!(
        gyro.process_gyro_aim(10_000_000, spin_about_world_up)
            .unwrap(),
        [0.0; 2]
    );
    assert_eq!(
        fused
            .process_fused_aim(sample(10, spin_about_world_up, REST))
            .unwrap(),
        [1.0, 0.0]
    );
}

#[test]
fn fused_aim_rejects_accel_as_motion_and_reacquires_after_reset() {
    let mut motion = MotionEngine::new(MotionConfig::default(), Calibration::default()).unwrap();
    for (ms, accel) in [
        (0, [0.0; 3]),
        (10, REST),
        (20, [0.1, 0.0, 0.99]),
        (30, [0.0, 0.0, 4.5]),
        (40, [f32::NAN; 3]),
    ] {
        assert_eq!(
            motion
                .process_fused_aim(sample(ms, [0.0; 3], accel))
                .unwrap(),
            [0.0; 2]
        );
    }
    assert!(
        motion
            .process_fused_aim(sample(50, [0.0, 0.0, 100.0], [f32::NAN; 3]))
            .unwrap()[0]
            > 0.4
    );
    motion.reset_segment();
    let yaw = [0.0, 100.0, 0.0];
    assert_eq!(
        motion.process_fused_aim(sample(0, yaw, [0.0; 3])).unwrap(),
        [0.0; 2]
    );
    assert_eq!(
        motion.process_fused_aim(sample(10, yaw, [0.0; 3])).unwrap(),
        [1.0, 0.0]
    );
}
