use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{self, Write},
    os::fd::AsFd,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use joyshockrust_input::{DeviceModel, Transport};
use nix::{
    poll::{PollFd, PollFlags, poll},
    sys::{
        signal::{SigSet, Signal},
        signalfd::SignalFd,
    },
};
use serde::Deserialize;

use crate::discovery::{self, ControllerFilter, Selection};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Effect {
    #[default]
    Off,
    Resistance {
        start: u8,
        strength: u8,
    },
}

impl Effect {
    pub fn validate(self) -> Result<Self, &'static str> {
        if let Self::Resistance { start, strength } = self
            && (start > 9 || !(1..=8).contains(&strength))
        {
            return Err("adaptive trigger resistance requires start 0–9 and strength 1–8");
        }
        Ok(self)
    }

    fn encode(self) -> [u8; 11] {
        let mut data = [0; 11];
        match self {
            Self::Off => data[0] = 0x05,
            Self::Resistance { start, strength } => {
                data[0] = 0x21;
                let active = 0x03ff_u16 & (u16::MAX << start);
                let forces = (start..10).fold(0_u32, |bits, zone| {
                    bits | (u32::from(strength - 1) << (3 * zone))
                });
                data[1..3].copy_from_slice(&active.to_le_bytes());
                data[3..7].copy_from_slice(&forces.to_le_bytes());
            }
        }
        data
    }
}

fn packet(
    transport: Transport,
    sequence: u8,
    [left, right]: [Option<Effect>; 2],
) -> ([u8; 78], usize) {
    let mut bytes = [0; 78];
    let (offset, len) = if transport == Transport::Bluetooth {
        bytes[..3].copy_from_slice(&[0x31, (sequence & 0x0f) << 4, 0x10]);
        (3, 78)
    } else {
        bytes[0] = 0x02;
        (1, 63)
    };
    // Only set the valid bits for the actuators being updated.
    if let Some(right) = right {
        bytes[offset] |= 0x04;
        bytes[offset + 10..offset + 21].copy_from_slice(&right.encode());
    }
    if let Some(left) = left {
        bytes[offset] |= 0x08;
        bytes[offset + 21..offset + 32].copy_from_slice(&left.encode());
    }
    if transport == Transport::Bluetooth {
        let crc = joyshockrust_input::bluetooth_crc32(0xa2, &bytes[..74]);
        bytes[74..].copy_from_slice(&crc.to_le_bytes());
    }
    (bytes, len)
}

pub struct AdaptiveTriggers {
    output: Option<File>,
    transport: Transport,
    sequence: u8,
    applied: [Effect; 2],
}

impl AdaptiveTriggers {
    pub fn new(transport: Transport) -> Self {
        Self {
            output: None,
            transport,
            sequence: 0,
            applied: [Effect::Off; 2],
        }
    }

    pub fn update(&mut self, path: &Path, input: &File, effects: [Effect; 2]) {
        if let Err(error) = self.apply(path, input, effects) {
            eprintln!(
                "Adaptive triggers unavailable on {}: {error}",
                path.display()
            );
        }
    }

    fn apply(&mut self, path: &Path, input: &File, effects: [Effect; 2]) -> io::Result<()> {
        if self.output.is_none() && effects != [Effect::Off; 2] {
            let output = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(nix::libc::O_NONBLOCK)
                .open(path)?;
            if output.metadata()?.rdev() != input.metadata()?.rdev()
                || crate::hidraw::identity(&output)? != crate::hidraw::identity(input)?
            {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "controller changed before opening adaptive trigger output",
                ));
            }
            self.output = Some(output);
        }
        self.write(effects)
    }

    fn write(&mut self, effects: [Effect; 2]) -> io::Result<()> {
        let Some(output) = &mut self.output else {
            return Ok(());
        };
        // Re-send enabled effects on each profile apply. A controller or
        // another HID client can reset them between profile updates.
        let updates = std::array::from_fn(|side| {
            (effects[side] != Effect::Off || self.applied[side] != Effect::Off)
                .then_some(effects[side])
        });
        if updates == [None; 2] {
            return Ok(());
        }
        let (buffer, length) = packet(self.transport, self.sequence, updates);
        if output.write(&buffer[..length])? != length {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "incomplete adaptive trigger report",
            ));
        }
        self.sequence = (self.sequence + 1) & 0x0f;
        self.applied = effects;
        Ok(())
    }
}

impl Drop for AdaptiveTriggers {
    fn drop(&mut self) {
        if self.applied != [Effect::Off; 2] {
            let _ = self.write([Effect::Off; 2]);
        }
    }
}

pub fn test_cli(mut args: impl Iterator<Item = OsString>) -> io::Result<()> {
    const USAGE: &str =
        "usage: joyshockrust-linux --test-adaptive-triggers [left|right|both] [/dev/hidrawN]";
    let mut args = args.by_ref().peekable();
    if args.next_if(|arg| arg == "--help").is_some() {
        println!("{USAGE}");
        return Ok(());
    }
    let side = args
        .next_if(|arg| arg == "left" || arg == "right" || arg == "both")
        .unwrap_or_else(|| "both".into());
    let path = args.next().map(PathBuf::from);
    if path
        .as_deref()
        .is_some_and(|path| !path.starts_with("/dev"))
        || args.next().is_some()
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, USAGE));
    }
    let filter = ControllerFilter {
        transport: Selection::Auto,
        ..ControllerFilter::default()
    };
    let (input, identity, path) = if let Some(path) = path {
        let (input, identity) = discovery::open_controller(&path, filter)?;
        (input, identity, path)
    } else {
        let (input, candidate) = discovery::open_first_controller(filter, None)?;
        (input, candidate.identity, candidate.path)
    };
    println!(
        "Controller: {} ({}) on {}",
        identity.model_name(),
        discovery::transport_name(identity.transport),
        path.display()
    );
    if identity.model == DeviceModel::DualSenseEdge {
        println!(
            "Set the Edge trigger stops to full travel and enable Trigger Effect Intensity in its onboard profile."
        );
    }
    let resistance = Effect::Resistance {
        start: 2,
        strength: 5,
    };
    let effects = [
        if side == "right" {
            Effect::Off
        } else {
            resistance
        },
        if side == "left" {
            Effect::Off
        } else {
            resistance
        },
    ];

    let mut signals = SigSet::empty();
    signals.add(Signal::SIGINT);
    signals.add(Signal::SIGTERM);
    signals.thread_block()?;
    let signal = SignalFd::new(&signals)?;
    let mut controller = AdaptiveTriggers::new(identity.transport);
    controller.apply(&path, &input, effects)?;
    println!(
        "Report written. Pull the selected trigger(s) within eight seconds: {}.",
        side.to_string_lossy()
    );
    poll(
        &mut [PollFd::new(signal.as_fd(), PollFlags::POLLIN)],
        8000_u16,
    )?;
    controller.apply(&path, &input, [Effect::Off; 2])?;
    println!("Resistance cleared.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_reports_enable_only_triggers_and_encode_usb_and_bluetooth() {
        let effects = [
            Some(Effect::Resistance {
                start: 8,
                strength: 8,
            }),
            Some(Effect::Off),
        ];
        let (usb, len) = packet(Transport::Usb, 0, effects);
        assert_eq!(len, 63);
        assert_eq!(&usb[..5], &[2, 12, 0, 0, 0]);
        assert_eq!(&usb[11..22], &[5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&usb[22..33], &[0x21, 0, 3, 0, 0, 0, 0x3f, 0, 0, 0, 0]);
        let (bt, len) = packet(Transport::Bluetooth, 15, effects);
        assert_eq!(len, 78);
        assert_eq!(&bt[..3], &[0x31, 0xf0, 0x10]);
        assert_eq!(&bt[3..34], &usb[1..32]);
        assert_eq!(&bt[74..], &[0xf0, 0x36, 0x32, 0x7b]);
        assert!(
            Effect::Resistance {
                start: 10,
                strength: 3
            }
            .validate()
            .is_err()
        );
        assert!(
            Effect::Resistance {
                start: 2,
                strength: 0
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn reapplying_resends_resistance_and_drop_clears_only_the_active_side() {
        use std::{os::fd::OwnedFd, os::unix::net::UnixDatagram, time::Duration};

        let (sink, source) = UnixDatagram::pair().unwrap();
        sink.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut controller = AdaptiveTriggers::new(Transport::Usb);
        controller.output = Some(File::from(OwnedFd::from(source)));
        let settings = [
            Effect::Resistance {
                start: 2,
                strength: 5,
            },
            Effect::Off,
        ];
        controller.write(settings).unwrap();
        controller.write(settings).unwrap();
        drop(controller);

        let mut report = [0; 78];
        for mode in [0x21, 0x21, 0x05] {
            assert_eq!(sink.recv(&mut report).unwrap(), 63);
            assert_eq!(report[1], 0x08);
            assert_eq!(report[11], 0);
            assert_eq!(report[22], mode);
        }
    }
}
