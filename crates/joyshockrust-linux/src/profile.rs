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
    gamepad::{ButtonRemaps, StickCurve},
    gyro_gate::{self, Binding, TriggerGate},
    gyro_mouse,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    version: u32,
    stick_curve: Option<f32>,
    #[serde(default)]
    remap_buttons: Vec<String>,
    #[serde(default)]
    layer_remaps: Vec<String>,
    gyro: Option<Gyro>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Gyro {
    #[serde(default)]
    calibrate: bool,
    flick_stick: Option<f64>,
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

pub struct RuntimeOptions {
    pub stick_curve: Option<StickCurve>,
    pub remaps: ButtonRemaps,
    pub gyro: Option<gyro_mouse::Options>,
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

fn parse(source: &str) -> Result<RuntimeOptions, Box<dyn Error>> {
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
    let gyro = profile.gyro.map(Gyro::resolve).transpose()?;
    Ok(RuntimeOptions {
        stick_curve,
        remaps,
        gyro,
    })
}

impl Gyro {
    fn resolve(self) -> Result<gyro_mouse::Options, Box<dyn Error>> {
        let flick_stick = self
            .flick_stick
            .map(gyro_mouse::validate_flick_scale)
            .transpose()?;
        let named_button = |name: &str| {
            gyro_gate::button_by_name(name)
                .ok_or_else(|| format!("unknown gyro binding button: {name}"))
        };
        let binding = match self.binding {
            None => Binding::Always,
            Some(GyroBinding::Hold(name)) => Binding::Hold(named_button(&name)?),
            Some(GyroBinding::DisableWhileHeld(name)) => {
                Binding::DisableWhileHeld(named_button(&name)?)
            }
            Some(GyroBinding::Toggle(name)) => Binding::Toggle(named_button(&name)?),
            Some(GyroBinding::TapHold(name)) => Binding::TapHold(named_button(&name)?),
            Some(GyroBinding::DoubleTap(name)) => Binding::DoubleTap(named_button(&name)?),
            Some(GyroBinding::Chord(spec)) => gyro_gate::chord_by_name(&spec)?,
            Some(GyroBinding::Trigger(spec)) => Binding::AnalogTrigger(TriggerGate::parse(&spec)?),
        };
        Ok(gyro_mouse::Options {
            calibrate: self.calibrate,
            binding,
            flick_stick,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamepad::RemapState;
    use joyshockrust_core::{Button, Controls, Stick};

    #[test]
    fn l2_profile_disables_gyro_only_while_digital_trigger_is_held() {
        let profile = parse(include_str!("../../../profiles/gyro-except-l2.toml")).unwrap();
        let mut gate = gyro_gate::GyroGate::new(profile.gyro.unwrap().binding);
        let mut controls = Controls {
            left_trigger: 0.5,
            ..Controls::default()
        };
        assert!(gate.enabled(&controls, std::time::Instant::now));
        controls.buttons.insert(Button::LeftTrigger);
        assert!(!gate.enabled(&controls, std::time::Instant::now));
        controls.buttons.remove(Button::LeftTrigger);
        assert!(gate.enabled(&controls, std::time::Instant::now));
    }

    #[test]
    fn shipped_profile_resolves_sticks_gyro_and_independent_layers() {
        let profile = parse(include_str!(
            "../../../profiles/dualsense-edge.example.toml"
        ))
        .unwrap();
        assert!(matches!(
            profile.gyro.unwrap().binding,
            Binding::Hold(Button::LeftTrigger)
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
