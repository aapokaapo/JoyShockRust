//! Request a short rumble effect from the running JoyShockRust bridge.

#[cfg(target_os = "linux")]
fn main() -> std::io::Result<()> {
    linux::run()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("rumble_test requires Linux evdev");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        env,
        ffi::OsStr,
        fs::OpenOptions,
        io,
        os::unix::fs::MetadataExt,
        path::{Path, PathBuf},
        thread,
        time::Duration,
    };

    use evdev::{BusType, Device, FFEffectCode, FFEffectData, FFEffectKind, FFReplay, FFTrigger};
    use joyshockrust_input::DeviceModel;

    const USAGE: &str = "usage: rumble_test [/dev/input/eventN] | rumble_test --physical /dev/input/eventN [--reset-gain]";
    const PULSE_MS: u16 = 750;

    pub fn run() -> io::Result<()> {
        let mut args = env::args_os().skip(1);
        let (path, physical, reset_gain) =
            match (args.next(), args.next(), args.next(), args.next()) {
                (None, None, None, None) => (find_gamepad()?, false, false),
                (Some(arg), None, None, None) if arg == "--help" || arg == "-h" => {
                    println!("{USAGE}");
                    return Ok(());
                }
                (Some(path), None, None, None) if !path.as_encoded_bytes().starts_with(b"-") => {
                    (PathBuf::from(path), false, false)
                }
                (Some(arg), Some(path), option, None)
                    if arg == "--physical"
                        && !path.as_encoded_bytes().starts_with(b"-")
                        && option.as_ref().is_none_or(|value| value == "--reset-gain") =>
                {
                    (PathBuf::from(path), true, option.is_some())
                }
                _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, USAGE)),
            };
        test_gamepad(&path, physical, reset_gain)
            .map_err(|err| io::Error::new(err.kind(), format!("{}: {err}", path.display())))
    }

    fn is_joyshockrust_gamepad(device: &udev::Device) -> bool {
        device.syspath().starts_with("/sys/devices/virtual/input")
            && device.sysname().as_encoded_bytes().starts_with(b"event")
            && device.parent().is_some_and(|input| {
                input
                    .attribute_value("name")
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| {
                        name.starts_with("JoyShockRust Virtual ") && name.ends_with("Gamepad")
                    })
            })
    }

    fn find_gamepad() -> io::Result<PathBuf> {
        let mut enumerator = udev::Enumerator::new()?;
        enumerator.match_subsystem("input")?;
        let mut paths = enumerator
            .scan_devices()?
            .filter(is_joyshockrust_gamepad)
            .filter_map(|device| device.devnode().map(Path::to_path_buf));
        let path = paths.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no JoyShockRust virtual gamepad found; start the bridge with --rumble and connect the controller",
            )
        })?;
        if paths.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "multiple JoyShockRust virtual gamepads found; pass the desired /dev/input/eventN",
            ));
        }
        Ok(path)
    }

    fn test_gamepad(path: &Path, physical: bool, reset_gain: bool) -> io::Result<()> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let node = udev::Device::from_devnum(udev::DeviceType::Character, file.metadata()?.rdev())?;
        if !physical && !is_joyshockrust_gamepad(&node) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a JoyShockRust virtual gamepad",
            ));
        }
        let mut device = Device::from_fd(file.into())?;
        if physical {
            let id = device.input_id();
            if !matches!(id.bus_type(), BusType::BUS_USB | BusType::BUS_BLUETOOTH)
                || !matches!(
                    DeviceModel::from_vid_pid(id.vendor(), id.product()),
                    DeviceModel::DualSense | DeviceModel::DualSenseEdge
                )
                || node.parent_with_subsystem("hid")?.is_none()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "expected a physical DualSense or DualSense Edge event node",
                ));
            }
        }
        if !device
            .supported_ff()
            .is_some_and(|effects| effects.contains(FFEffectCode::FF_RUMBLE))
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                if physical {
                    "physical event node does not offer FF_RUMBLE"
                } else {
                    "virtual gamepad does not offer rumble; start the bridge with --rumble and check for Rumble forwarding ready"
                },
            ));
        }

        if reset_gain {
            if !device
                .supported_ff()
                .is_some_and(|effects| effects.contains(FFEffectCode::FF_GAIN))
            {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "physical event node does not offer FF_GAIN",
                ));
            }
            device.set_ff_gain(u16::MAX)?;
            println!("Reset physical force feedback gain to the kernel default (100%).");
        }

        let mut effect = device.upload_ff_effect(FFEffectData {
            direction: 0,
            trigger: FFTrigger::default(),
            replay: FFReplay {
                length: PULSE_MS,
                delay: 0,
            },
            kind: FFEffectKind::Rumble {
                strong_magnitude: 49_152,
                weak_magnitude: 36_864,
            },
        })?;
        println!(
            "Requesting a {PULSE_MS} ms rumble pulse through {}",
            path.display()
        );
        effect.play(1)?;
        thread::sleep(Duration::from_millis(u64::from(PULSE_MS)));
        effect.stop()?;
        println!(
            "The kernel accepted the playback write. Confirm whether the controller vibrated."
        );
        Ok(())
    }
}
