use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

use joyshockrust_input::Transport;
use serde::Deserialize;

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

fn packet(transport: Transport, sequence: u8, [left, right]: [Effect; 2]) -> ([u8; 78], usize) {
    let mut bytes = [0; 78];
    let (offset, len) = if transport == Transport::Bluetooth {
        bytes[..3].copy_from_slice(&[0x31, (sequence & 0x0f) << 4, 0x10]);
        (3, 78)
    } else {
        bytes[0] = 0x02;
        (1, 63)
    };
    // Update both trigger actuators only. Leave rumble and lights to hid-playstation.
    bytes[offset] = 0x0c;
    bytes[offset + 10..offset + 21].copy_from_slice(&right.encode());
    bytes[offset + 21..offset + 32].copy_from_slice(&left.encode());
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
        if self.applied == effects {
            return;
        }
        let result = (|| {
            if self.output.is_none() {
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
        })();
        if let Err(error) = result {
            eprintln!(
                "Adaptive triggers unavailable on {}: {error}",
                path.display()
            );
        }
    }

    fn write(&mut self, effects: [Effect; 2]) -> io::Result<()> {
        let Some(output) = &mut self.output else {
            return Ok(());
        };
        let (buffer, length) = packet(self.transport, self.sequence, effects);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_reports_enable_only_triggers_and_encode_usb_and_bluetooth() {
        let effects = [
            Effect::Resistance {
                start: 8,
                strength: 8,
            },
            Effect::Off,
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
}
