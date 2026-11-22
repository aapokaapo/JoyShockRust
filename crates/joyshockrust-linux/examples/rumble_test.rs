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

    use evdev::{Device, FFEffectCode, FFEffectData, FFEffectKind, FFReplay, FFTrigger};

    const USAGE: &str = "usage: rumble_test [/dev/input/eventN]";
    const PULSE_MS: u16 = 350;

    pub fn run() -> io::Result<()> {
        let mut args = env::args_os().skip(1);
        let path = match (args.next(), args.next()) {
            (None, None) => find_gamepad()?,
            (Some(arg), None) if arg == "--help" || arg == "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            (Some(path), None) if !path.as_encoded_bytes().starts_with(b"-") => PathBuf::from(path),
            _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, USAGE)),
        };
        test_gamepad(&path)
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

    fn test_gamepad(path: &Path) -> io::Result<()> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let node = udev::Device::from_devnum(udev::DeviceType::Character, file.metadata()?.rdev())?;
        if !is_joyshockrust_gamepad(&node) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a JoyShockRust virtual gamepad",
            ));
        }
        let mut device = Device::from_fd(file.into())?;
        if !device
            .supported_ff()
            .is_some_and(|effects| effects.contains(FFEffectCode::FF_RUMBLE))
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "virtual gamepad does not offer rumble; start the bridge with --rumble and check for Rumble forwarding ready",
            ));
        }

        let mut effect = device.upload_ff_effect(FFEffectData {
            direction: 0,
            trigger: FFTrigger::default(),
            replay: FFReplay {
                length: PULSE_MS,
                delay: 0,
            },
            kind: FFEffectKind::Rumble {
                strong_magnitude: 32_768,
                weak_magnitude: 24_576,
            },
        })?;
        println!(
            "Requesting a {PULSE_MS} ms rumble pulse through {}",
            path.display()
        );
        effect.play(1)?;
        thread::sleep(Duration::from_millis(u64::from(PULSE_MS)));
        effect.stop()?;
        println!("Rumble request complete. Check for one short pulse on the physical controller.");
        Ok(())
    }
}
