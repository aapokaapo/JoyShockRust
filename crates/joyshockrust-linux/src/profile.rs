use std::{
    env,
    error::Error,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process,
};

use serde::Deserialize;

use crate::{
    discovery::{ControllerFilter, ModelSelection, Selection},
    game_calibration::GameCalibration,
    gamepad::{ButtonRemaps, OutputKind, StickCurve},
    gyro_gate::{self, Binding, DigitalInput, TriggerGate},
    gyro_mouse,
    hidraw::ControllerId,
    touchpad,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    version: u32,
    bridge: Option<BridgeOptions>,
    stick_curve: Option<f32>,
    #[serde(default)]
    remap_buttons: Vec<String>,
    #[serde(default)]
    layer_remaps: Vec<String>,
    gyro: Option<Gyro>,
    touchpad: Option<touchpad::Config>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Gyro {
    #[serde(default)]
    calibrate: bool,
    flick_stick: Option<f64>,
    sensitivity_x: Option<f32>,
    sensitivity_y: Option<f32>,
    real_world_calibration: Option<f64>,
    in_game_sens: Option<f64>,
    binding: Option<GyroBinding>,
}

#[derive(Deserialize)]
#[serde(
    tag = "mode",
    content = "input",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
enum GyroBinding {
    Hold(String),
    DisableWhileHeld(String),
    Toggle(String),
    TapHold(String),
    DoubleTap(String),
    Chord(String),
    Trigger(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BridgeOptions {
    pub transport: Selection,
    pub model: ModelSelection,
    pub controller_id: Option<ControllerId>,
    pub gamepad: OutputKind,
    pub rumble: bool,
}

impl BridgeOptions {
    pub fn controller_filter(self) -> ControllerFilter {
        ControllerFilter {
            transport: self.transport,
            model: self.model,
            controller_id: self.controller_id,
        }
    }
}

pub struct RuntimeOptions {
    pub bridge: Option<BridgeOptions>,
    pub stick_curve: Option<StickCurve>,
    pub remaps: ButtonRemaps,
    pub gyro: Option<gyro_mouse::Options>,
    pub touchpad: Option<touchpad::Grid>,
}

pub fn load(path: &Path) -> Result<RuntimeOptions, Box<dyn Error>> {
    let source = fs::read_to_string(path)?;
    parse(&source)
}

pub fn default_path() -> io::Result<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute())
                .map(|home| home.join(".config"))
        })
        .map(|dir| dir.join("joyshockrust/profile.toml"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no absolute XDG_CONFIG_HOME or HOME",
            )
        })
}

pub fn install(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    let content =
        fs::read_to_string(source).map_err(|err| format!("{}: {err}", source.display()))?;
    parse(&content).map_err(|err| format!("{}: {err}", source.display()))?;
    if destination.file_name().is_none() {
        return Err("expected a destination file".into());
    }
    replace_file(destination, content.as_bytes())
        .map_err(|err| format!("{}: {err}", destination.display()))?;
    Ok(())
}

pub fn save_game_calibration(
    path: &Path,
    real_world_calibration: f64,
    in_game_sens: f64,
) -> Result<(), Box<dyn Error>> {
    let source = fs::read_to_string(path)?;
    let rendered = render_game_calibration(&source, real_world_calibration, in_game_sens)?;
    replace_file(path, rendered.as_bytes())?;
    Ok(())
}

fn render_game_calibration(
    source: &str,
    real_world_calibration: f64,
    in_game_sens: f64,
) -> Result<String, Box<dyn Error>> {
    parse(source)?;
    let mut document = source.parse::<toml_edit::DocumentMut>()?;
    let gyro = document
        .entry("gyro")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_like_mut()
        .ok_or("expected a [gyro] table")?;
    for (key, number) in [
        ("real_world_calibration", real_world_calibration),
        ("in_game_sens", in_game_sens),
    ] {
        let mut value = toml_edit::Value::from(number);
        if let Some(existing) = gyro.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
            *value.decor_mut() = existing.decor().clone();
            *existing = value;
        } else {
            gyro.insert(key, toml_edit::Item::Value(value));
        }
    }
    let rendered = document.to_string();
    parse(&rendered)?;
    Ok(rendered)
}

fn replace_file(destination: &Path, content: &[u8]) -> io::Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination must be a regular file",
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }

    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    // Stage in the same directory so concurrent readers see either whole file.
    for attempt in 0..64 {
        let temporary = parent.join(format!(
            ".joyshockrust-profile-{}-{attempt}.tmp",
            process::id()
        ));
        if temporary.file_name() == destination.file_name() {
            continue;
        }
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };
        let result = file
            .write_all(content)
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(&temporary, destination));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        return result;
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not reserve a temporary profile file",
    ))
}

pub(crate) fn parse(source: &str) -> Result<RuntimeOptions, Box<dyn Error>> {
    let profile: Profile = toml::from_str(source)?;
    if profile.version != 1 {
        return Err(format!(
            "unsupported profile version {}, expected 1",
            profile.version
        )
        .into());
    }
    let stick_curve = profile.stick_curve.map(StickCurve::new).transpose()?;
    let mut remaps = ButtonRemaps::default();
    for rule in &profile.remap_buttons {
        remaps.add(rule)?;
    }
    for rule in &profile.layer_remaps {
        remaps.add_layer(rule)?;
    }
    let touchpad = profile
        .touchpad
        .map(touchpad::Config::resolve)
        .transpose()?;
    let gyro = profile
        .gyro
        .map(|gyro| gyro.resolve(touchpad.as_ref()))
        .transpose()?;
    Ok(RuntimeOptions {
        bridge: profile.bridge,
        stick_curve,
        remaps,
        gyro,
        touchpad,
    })
}

impl Gyro {
    fn resolve(
        self,
        touchpad: Option<&touchpad::Grid>,
    ) -> Result<gyro_mouse::Options, Box<dyn Error>> {
        let flick_stick = self
            .flick_stick
            .map(gyro_mouse::validate_flick_scale)
            .transpose()?;
        let resolve_input = |name: &str| -> Result<DigitalInput, String> {
            if let Some(spec) = name.strip_prefix("touch:") {
                let (row, column) = spec
                    .split_once(':')
                    .ok_or("expected touch:ROW:COLUMN for the gyro binding")?;
                let row = row.parse().map_err(|_| "invalid gyro touch region row")?;
                let column = column
                    .parse()
                    .map_err(|_| "invalid gyro touch region column")?;
                let region = touchpad
                    .ok_or("gyro touch binding requires a [touchpad] grid")?
                    .region(row, column)?;
                return Ok(DigitalInput::Touch(region));
            }
            gyro_gate::button_by_name(name)
                .map(DigitalInput::Button)
                .ok_or_else(|| format!("unknown gyro binding button: {name}"))
        };
        let binding = match self.binding {
            None => Binding::Always,
            Some(GyroBinding::Hold(name)) => Binding::Hold(resolve_input(&name)?),
            Some(GyroBinding::DisableWhileHeld(name)) => {
                Binding::DisableWhileHeld(resolve_input(&name)?)
            }
            Some(GyroBinding::Toggle(name)) => Binding::Toggle(resolve_input(&name)?),
            Some(GyroBinding::TapHold(name)) => Binding::TapHold(resolve_input(&name)?),
            Some(GyroBinding::DoubleTap(name)) => Binding::DoubleTap(resolve_input(&name)?),
            Some(GyroBinding::Chord(spec)) => gyro_gate::parse_chord(&spec, resolve_input)?,
            Some(GyroBinding::Trigger(spec)) => Binding::AnalogTrigger(TriggerGate::parse(&spec)?),
        };
        Ok(gyro_mouse::Options {
            calibrate: self.calibrate,
            binding,
            flick_stick,
            game_calibration: GameCalibration::new(
                self.real_world_calibration.unwrap_or(1.0),
                self.in_game_sens.unwrap_or(1.0),
            )?,
            sensitivity: gyro_mouse::Sensitivity::from_profile(
                self.sensitivity_x,
                self.sensitivity_y,
            )?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joyshockrust_input::{DeviceModel, Transport, parse_report};

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
            profile.bridge.unwrap(),
            BridgeOptions {
                transport: Selection::Auto,
                model: ModelSelection::DualSenseEdge,
                controller_id: None,
                gamepad: OutputKind::XboxSeries,
                rumble: true,
            }
        );
        let partial = parse("version = 1\n[bridge]\ngamepad = \"xbox360\"")
            .unwrap()
            .bridge;
        assert_eq!(
            partial.unwrap(),
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
        for (setting, model) in [
            ("any", ModelSelection::Any),
            ("dualsense", ModelSelection::DualSense),
        ] {
            let bridge = parse(&format!("version = 1\n[bridge]\nmodel = \"{setting}\""))
                .unwrap()
                .bridge
                .unwrap();
            assert_eq!(bridge.model, model);
        }
    }

    #[test]
    fn recorded_game_calibration_preserves_profile_comments_and_other_values() {
        let source = concat!(
            "# My profile\nversion = 1\n",
            "[gyro]\nsensitivity_x = 1.5 # horizontal\n",
            "real_world_calibration = 42.0 # old measurement\n",
            "[gyro.binding]\nmode = \"hold\"\ninput = \"l2\"\n",
        );
        let rendered = render_game_calibration(source, 40.0, 2.0).unwrap();
        assert!(rendered.contains("# My profile\nversion = 1"));
        assert!(rendered.contains("sensitivity_x = 1.5 # horizontal"));
        assert!(rendered.contains("real_world_calibration = 40.0 # old measurement"));
        assert!(rendered.contains("in_game_sens = 2.0"));
        assert!(rendered.contains("[gyro.binding]\nmode = \"hold\"\ninput = \"l2\""));
        assert!(
            render_game_calibration("version = 1\ngyro = { sensitivity_x = 2 }", 40.0, 2.0).is_ok()
        );
        assert!(render_game_calibration("version = 1", 40.0, 2.0).is_ok());
        assert!(render_game_calibration(source, 0.0, 2.0).is_err());
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
        let profile = parse(include_str!("../../../profiles/gyro-except-l2.toml")).unwrap();
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
            "../../../profiles/dualsense-edge.example.toml"
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
}
