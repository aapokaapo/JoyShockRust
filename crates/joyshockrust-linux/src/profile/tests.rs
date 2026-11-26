use super::*;
use crate::{
    gamepad::OutputKind,
    gyro_gate::{self, Binding, DigitalInput},
    gyro_mouse,
};
use joyshockrust_input::{DeviceModel, Transport, parse_report};

#[test]
fn legacy_trigger_aliases_and_profile_precision_are_preserved() {
    let aliases = r#"
version = 1
[[keyboard.bindings]]
trigger = "l2"
hold = "KEY_Q"
[[keyboard.bindings]]
trigger = "l2:0.45:0.35"
hold = "KEY_E"
"#;
    assert!(
        parse(aliases)
            .err()
            .unwrap()
            .to_string()
            .contains("duplicate keyboard binding")
    );
    assert!(parse("version = 1\n[gyro]\nsensitivity_x = 100.0000001").is_ok());
    assert!(parse("version = 1\n[gyro]\nsensitivity_x = nan").is_err());
    assert!(
        parse(
            r#"
version = 1
touchpad = { rows = 1, columns = 2, regions = [{ row = 1, column = 1, touch = "cross" }] }
"#
        )
        .is_ok()
    );
}

#[test]
fn controller_gyro_calibration_bindings_are_opt_in_and_validated() {
    let config =
        |button, hold| format!("version = 1\n[gyro.calibration_hold]\nbutton = {button:?}\n{hold}");
    let configured = parse(&config("ps", ""))
        .unwrap()
        .gyro
        .unwrap()
        .calibration_hold
        .unwrap();
    assert_eq!(configured.button, joyshockrust_core::Button::Home);
    assert_eq!(configured.duration.as_millis(), 1500);
    assert!(parse(&config("ps", "hold_ms = 100")).is_err());
    assert!(parse(&config("not-a-button", "")).is_err());
    assert!(
        parse("version = 1\n[gyro]")
            .unwrap()
            .gyro
            .unwrap()
            .calibration_hold
            .is_none()
    );
}

#[test]
fn touch_region_controls_gyro_independently_of_gamepad_bindings() {
    let profile = r#"
            version = 1
            [touchpad]
            rows = 1
            columns = 2
            [[touchpad.regions]]
            row = 1
            column = 1
            touch = "cross"
            [gyro.binding]
            mode = "hold"
            input = "touch:1:2"
        "#;
    let mut frame = [0; 64];
    frame[0] = 1;
    frame[8] = 8;
    let left = [0x01, 0xbf, 0x03, 0x00]; // x = 959
    let right = [0x02, 0xc0, 0x03, 0x00]; // x = 960
    let released = [0x80, 0, 0, 0];

    for (mode, inverted) in [("hold", false), ("disable-while-held", true)] {
        let runtime =
            parse(&profile.replace("mode = \"hold\"", &format!("mode = \"{mode}\""))).unwrap();
        let mut gate = gyro_gate::GyroGate::new(runtime.gyro.unwrap().binding);
        for (first, second, clicked, occupied) in [
            (released, released, false, false),
            (released, released, true, false),
            (left, released, false, false),
            (left, right, false, true),
            (right, released, true, true),
            (left, released, true, false),
            (released, released, false, false),
        ] {
            frame[10] = if clicked { 2 } else { 0 };
            frame[33..37].copy_from_slice(&first);
            frame[37..41].copy_from_slice(&second);
            let report =
                parse_report(DeviceModel::DualSenseEdge, Transport::Usb, &frame, None).unwrap();
            assert_eq!(
                gate.enabled(
                    &report.controls,
                    &report.touch_contacts,
                    std::time::Instant::now
                ),
                occupied ^ inverted
            );
        }
    }
}

#[test]
fn validates_gyro_touch_regions_and_accepts_digital_modes() {
    let grid = "[touchpad]\nrows = 2\ncolumns = 2";
    let profile = |mode, input, grid| {
        format!("version = 1\n[gyro.binding]\nmode = '{mode}'\ninput = '{input}'\n{grid}")
    };
    for mode in [
        "hold",
        "disable-while-held",
        "toggle",
        "tap-hold",
        "double-tap",
    ] {
        assert!(parse(&profile(mode, "touch:2:2", grid)).is_ok(), "{mode}");
    }
    for input in ["l1,touch:2:2", "touch:1:1,touch:2:2"] {
        assert!(parse(&profile("chord", input, grid)).is_ok(), "{input}");
    }
    for input in [
        "touch:1",
        "touch:a:1",
        "touch:1:2:3",
        "touch:0:1",
        "touch:1:3",
    ] {
        assert!(parse(&profile("hold", input, grid)).is_err(), "{input}");
    }
    let error = parse(&profile("hold", "touch:1:1", "")).err().unwrap();
    assert!(error.to_string().contains("requires a [touchpad] grid"));
    assert!(parse(&profile("chord", "touch:1:1,touch:1:1", grid)).is_err());
}
use crate::gamepad::RemapState;
use joyshockrust_core::{Button, Controls, Stick};

#[test]
fn bridge_profiles_preserve_defaults_and_configure_output() {
    assert_eq!(parse("version = 1").unwrap().bridge, None);
    assert_eq!(parse("version = 1\n[bridge]\ntransport = 'bluetooth'\nmodel = 'dualsense-edge'\ncontroller_id = '00:11:22:33:44:55'").unwrap().bridge, None);
    let profile = parse(
        r#"
            version = 1
            [bridge]
            transport = "auto"
            model = "dualsense-edge"
            gamepad = "xbox-series"
            rumble = true
        "#,
    )
    .unwrap();
    assert_eq!(
        profile.bridge.unwrap().startup(),
        BridgeOptions {
            gamepad: OutputKind::XboxSeries,
            rumble: true,
            virtual_gamepad: true,
        }
    );
    let partial = parse("version = 1\n[bridge]\ngamepad = \"xbox360\"")
        .unwrap()
        .bridge;
    assert_eq!(
        partial.unwrap().startup(),
        BridgeOptions {
            gamepad: OutputKind::Xbox360,
            ..BridgeOptions::default()
        }
    );
    for setting in [
        "transport = \"ethernet\"",
        "model = \"other\"",
        "controller_id = \"01:23:45:67:89\"",
        "gamepad = \"sony\"",
        "rumble = \"true\"",
        "unknown = true",
    ] {
        assert!(parse(&format!("version = 1\n[bridge]\n{setting}")).is_err());
    }
}

#[test]
fn gyro_aim_mode_defaults_to_gyro_and_accepts_fusion() {
    use joyshockrust_motion::AimMode;
    assert_eq!(
        parse("version = 1\n[gyro]").unwrap().gyro.unwrap().aim_mode,
        AimMode::Gyro
    );
    let fused = parse("version = 1\n[gyro]\naim_mode = 'sensor-fusion'").unwrap();
    assert_eq!(fused.gyro.unwrap().aim_mode, AimMode::SensorFusion);
    assert!(parse("version = 1\n[gyro]\naim_mode = 'unknown'").is_err());
}

#[test]
fn aim_response_profile_validates_thresholds_and_acceleration() {
    let valid = parse("version = 1\n[gyro]\nflick_axial_threshold_deg = 12\nflick_steering_sensitivity = 0.35\nthreshold_dps = 2\naccel_start_dps = 25\naccel_end_dps = 120\naccel_multiplier = 2.5").unwrap().gyro.unwrap();
    assert_eq!(valid.flick_axial_threshold(), 12.0);
    assert_eq!(valid.flick_steering_sensitivity, Some(0.35));
    assert_eq!(valid.response.values(), [2.0, 25.0, 120.0, 2.5]);
    let second = std::time::Duration::from_secs(1);
    assert_eq!(valid.response.interval_gain([0.0, 200.0], second), 2.5);
    for (enabled, gain) in [(false, 1.0), (true, 2.5)] {
        let profile = parse(&format!(
                "version = 1\n[gyro]\nthreshold_dps = 2\naccel_multiplier = 2.5\naccel_enabled = {enabled}"
            )).unwrap();
        let response = profile.gyro.unwrap().response;
        assert_eq!(response.interval_gain([0.0, 200.0], second), gain);
        assert_eq!(response.interval_gain([0.0, 1.0], second), 0.0);
    }
    for settings in [
        "flick_axial_threshold_deg = -1",
        "flick_axial_threshold_deg = 31",
        "flick_steering_sensitivity = -0.1",
        "flick_steering_sensitivity = 1.1",
        "flick_steering_sensitivity = nan",
        "threshold_dps = -1",
        "accel_multiplier = 0.5",
        "accel_end_dps = 20",
    ] {
        assert!(
            parse(&format!("version = 1\n[gyro]\n{settings}")).is_err(),
            "{settings}"
        );
    }
}

#[test]
fn gyro_sensitivity_defaults_independently_and_is_validated() {
    use gyro_mouse::Sensitivity;

    let partial = parse("version = 1\n[gyro]\nsensitivity_x = 12").unwrap();
    assert_eq!(
        partial.gyro.unwrap().sensitivity,
        Sensitivity::from_profile(Some(12.0), Some(4.0)).unwrap()
    );
    let both = parse("version = 1\n[gyro]\nsensitivity_x = 0\nsensitivity_y = 2.5").unwrap();
    assert_eq!(
        both.gyro.unwrap().sensitivity,
        Sensitivity::from_profile(Some(0.0), Some(2.5)).unwrap()
    );
    for field in ["sensitivity_x", "sensitivity_y"] {
        for value in ["-1.0", "100.1", "nan", "inf"] {
            let profile = format!("version = 1\n[gyro]\n{field} = {value}");
            assert!(parse(&profile).is_err(), "{profile}");
        }
    }
}

#[test]
fn per_stick_response_overrides_use_the_shared_curve_as_fallback() {
    use joyshockrust_core::{Controls, Stick};

    let controls = Controls {
        left_stick: Stick { x: 0.5, y: 0.0 },
        right_stick: Stick { x: 0.5, y: 0.0 },
        ..Controls::default()
    };
    for (settings, expected) in [
        ("stick_curve = 2", [0.25, 0.25]),
        ("stick_curve_left = 2", [0.25, 0.5]),
        ("stick_curve = 2\nstick_curve_right = 1", [0.25, 0.5]),
        ("stick_curve_left = 1\nstick_curve_right = 2", [0.5, 0.25]),
    ] {
        let curve = parse(&format!("version = 1\n{settings}"))
            .unwrap()
            .stick_curve
            .unwrap();
        let shaped = curve.apply(controls);
        assert_eq!([shaped.left_stick.x, shaped.right_stick.x], expected);
    }
    for setting in [
        "stick_curve_left = nan",
        "stick_curve_right = 4.01",
        "stick_curve_left = 0.24",
    ] {
        assert!(
            parse(&format!("version = 1\n{setting}")).is_err(),
            "{setting}"
        );
    }
}

#[test]
fn game_calibration_rejects_invalid_profile_values() {
    for fields in [
        "real_world_calibration = 0",
        "real_world_calibration = nan",
        "in_game_sens = -1",
        "in_game_sens = inf",
        "real_world_calibration = 40\nin_game_sens = 0.00001",
    ] {
        let profile = format!("version = 1\n[gyro]\n{fields}");
        assert!(parse(&profile).is_err(), "{profile}");
    }
}

#[test]
fn l2_profile_disables_gyro_only_while_digital_trigger_is_held() {
    let profile = parse(include_str!("../../../../profiles/gyro-except-l2.toml")).unwrap();
    let mut gate = gyro_gate::GyroGate::new(profile.gyro.unwrap().binding);
    let mut controls = Controls {
        left_trigger: 0.5,
        ..Controls::default()
    };
    assert!(gate.enabled(&controls, &[None; 2], std::time::Instant::now));
    controls.buttons.insert(Button::LeftTrigger);
    assert!(!gate.enabled(&controls, &[None; 2], std::time::Instant::now));
    controls.buttons.remove(Button::LeftTrigger);
    assert!(gate.enabled(&controls, &[None; 2], std::time::Instant::now));
}

#[test]
fn shipped_profile_resolves_sticks_gyro_and_independent_layers() {
    let profile = parse(include_str!(
        "../../../../profiles/dualsense-edge.example.toml"
    ))
    .unwrap();
    assert!(matches!(
        profile.gyro.unwrap().binding,
        Binding::Hold(DigitalInput::Button(Button::LeftTrigger))
    ));
    let mut controls = Controls {
        left_stick: Stick { x: 0.5, y: 0.0 },
        ..Controls::default()
    };
    assert_eq!(
        profile.stick_curve.unwrap().apply(controls).left_stick.x,
        0.25
    );
    controls.buttons.insert(Button::South);
    let mut state = RemapState::default();
    assert!(
        profile
            .remaps
            .apply(controls, &mut state)
            .buttons
            .contains(Button::North)
    );
    controls.buttons.insert(Button::LeftShoulder);
    assert!(
        profile
            .remaps
            .apply(controls, &mut state)
            .buttons
            .contains(Button::East)
    );
    controls.buttons.insert(Button::RightShoulder);
    assert!(
        profile
            .remaps
            .apply(controls, &mut state)
            .buttons
            .contains(Button::East)
    );
}

#[test]
fn rejects_invalid_or_unknown_configuration() {
    for (source, message) in [
        ("version = 2", "unsupported profile version"),
        ("version = 1\ntyop = true", "unknown field"),
        ("version = 1\nstick_curve = 0", "stick curve exponent"),
        (
            "version = 1\nlayer_remaps = ['l1:cross=circle', 'l1:cross=square']",
            "already has a remap",
        ),
        (
            "version = 1\n[gyro]\nflick_stick = -1",
            "Flick Stick sensitivity",
        ),
        (
            "version = 1\n[gyro.binding]\nmode = 'toggle'\ninput = 'unknown'",
            "unknown gyro binding button",
        ),
    ] {
        let error = parse(source).err().expect("profile must be rejected");
        assert!(error.to_string().contains(message), "{error}");
    }
}
