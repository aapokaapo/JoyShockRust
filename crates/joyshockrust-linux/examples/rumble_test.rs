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
        fs::{File, OpenOptions},
        io::{self, Write},
        os::{fd::AsFd, unix::fs::MetadataExt},
        path::{Path, PathBuf},
        thread,
        time::Duration,
    };

    use evdev::{BusType, Device, FFEffectCode, FFEffectData, FFEffectKind, FFReplay, FFTrigger};
    use joyshockrust_input::DeviceModel;
    use nix::{
        poll::{PollFd, PollFlags, poll},
        sys::{
            signal::{SigSet, Signal},
            signalfd::SignalFd,
        },
    };

    const USAGE: &str = concat!(
        "usage: rumble_test [/dev/input/eventN]\n",
        "       rumble_test --physical /dev/input/eventN [--reset-gain]\n",
        "       rumble_test --edge-usb-legacy /dev/hidrawN",
    );
    const PULSE_MS: u16 = 750;

    enum Target {
        Virtual,
        Physical { reset_gain: bool },
        EdgeUsbLegacy,
    }

    pub fn run() -> io::Result<()> {
        let mut args = env::args_os().skip(1);
        let (path, target) = match (args.next(), args.next(), args.next(), args.next()) {
            (None, None, None, None) => (find_gamepad()?, Target::Virtual),
            (Some(arg), None, None, None) if arg == "--help" || arg == "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            (Some(path), None, None, None) if !path.as_encoded_bytes().starts_with(b"-") => {
                (PathBuf::from(path), Target::Virtual)
            }
            (Some(arg), Some(path), option, None)
                if arg == "--physical"
                    && !path.as_encoded_bytes().starts_with(b"-")
                    && option.as_ref().is_none_or(|value| value == "--reset-gain") =>
            {
                (
                    PathBuf::from(path),
                    Target::Physical {
                        reset_gain: option.is_some(),
                    },
                )
            }
            (Some(arg), Some(path), None, None)
                if arg == "--edge-usb-legacy" && !path.as_encoded_bytes().starts_with(b"-") =>
            {
                (PathBuf::from(path), Target::EdgeUsbLegacy)
            }
            _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, USAGE)),
        };
        match target {
            Target::Virtual => test_gamepad(&path, false, false),
            Target::Physical { reset_gain } => test_gamepad(&path, true, reset_gain),
            Target::EdgeUsbLegacy => test_edge_usb_legacy(&path),
        }
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
    fn test_edge_usb_legacy(path: &Path) -> io::Result<()> {
        let mut device = OpenOptions::new().write(true).open(path)?;
        let node =
            udev::Device::from_devnum(udev::DeviceType::Character, device.metadata()?.rdev())?;
        let is_usb_edge = node.subsystem() == Some(OsStr::new("hidraw"))
            && node.parent_with_subsystem("hid")?.is_some_and(|hid| {
                hid.property_value("HID_ID") == Some(OsStr::new("0003:0000054C:00000DF2"))
            });
        if !is_usb_edge {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a wired DualSense Edge hidraw node",
            ));
        }

        // The USB command has no timer. Handle ordinary termination signals
        // in this thread so the stop report can be sent immediately.
        let mut signals = SigSet::empty();
        signals.add(Signal::SIGINT);
        signals.add(Signal::SIGTERM);
        signals.thread_block()?;
        let signal = SignalFd::new(&signals)?;
        let mut fds = [PollFd::new(signal.as_fd(), PollFlags::POLLIN)];

        let mut pulse = LegacyPulse {
            device: &mut device,
            stopped: false,
        };
        println!(
            "Requesting a {PULSE_MS} ms legacy rumble pulse through {}",
            path.display()
        );
        write_legacy_report(pulse.device, 96, 72)?;
        let waited = poll(&mut fds, PULSE_MS);
        pulse.stop()?;
        waited?;
        println!(
            "The start and stop reports were written. Confirm whether the controller vibrated."
        );
        Ok(())
    }

    struct LegacyPulse<'a> {
        device: &'a mut File,
        stopped: bool,
    }

    impl LegacyPulse<'_> {
        fn stop(&mut self) -> io::Result<()> {
            write_legacy_report(self.device, 0, 0)?;
            self.stopped = true;
            Ok(())
        }
    }

    impl Drop for LegacyPulse<'_> {
        fn drop(&mut self) {
            if !self.stopped {
                let _ = self.stop();
            }
        }
    }

    fn write_legacy_report(device: &mut File, left: u8, right: u8) -> io::Result<()> {
        let report = legacy_usb_report(left, right);
        if device.write(&report)? != report.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "short DualSense USB output report",
            ));
        }
        Ok(())
    }

    fn legacy_usb_report(left: u8, right: u8) -> [u8; 63] {
        let mut report = [0; 63];
        // Report ID, legacy vibration enable plus haptics select, other
        // feature flags, then the right and left motor levels.
        report[..5].copy_from_slice(&[0x02, 0x03, 0, right, left]);
        report
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn legacy_usb_reports_address_only_vibration() {
            let start = legacy_usb_report(96, 72);
            assert_eq!(&start[..5], &[0x02, 0x03, 0, 72, 96]);
            assert!(start[5..].iter().all(|&byte| byte == 0));
            let stop = legacy_usb_report(0, 0);
            assert_eq!(&stop[..2], &start[..2]);
            assert!(stop[2..].iter().all(|&byte| byte == 0));
        }
    }
}
