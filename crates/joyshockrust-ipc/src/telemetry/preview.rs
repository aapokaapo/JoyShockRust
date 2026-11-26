//! Ordered controller history carried in each editor preview snapshot.

use std::io::{self, Read};

use joyshockrust_config::controller::{ModelSelection, Selection};
use joyshockrust_core::{Buttons, Stick};

use super::{GyroSpeed, ImuReading, TouchPoints};
use crate::stream::{Stamp, Topic};

pub const HISTORY_LIMIT: usize = 96;
const SAMPLE_BYTES: usize = 37;
const HEADER_BYTES: usize = 19;
const IMU_BYTES: usize = 32;
pub(crate) const BODY_LIMIT: usize = HEADER_BYTES + IMU_BYTES + HISTORY_LIMIT * SAMPLE_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Device {
    pub model: ModelSelection,
    pub transport: Selection,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub buttons: Buttons,
    pub sticks: [Stick; 2],
    pub triggers: [f32; 2],
    pub touch: TouchPoints,
}

#[derive(Clone, Debug, PartialEq)]
pub struct History {
    /// Changes on each session and whenever retained history is discarded on unsubscribe.
    pub session: u64,
    pub device: Option<Device>,
    pub first: u64,
    pub samples: Vec<Sample>,
    pub imu: Option<(ImuReading, GyroSpeed)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub instance: u128,
    pub history: History,
}

#[derive(Default)]
pub struct Cursor {
    session: Option<(u128, u64)>,
    next: u64,
}

impl Cursor {
    /// A fresh connection or a missed range starts from the latest sample.
    /// Otherwise, return only samples following the range already consumed.
    pub fn advance<'a>(&mut self, preview: &'a Preview) -> (bool, &'a [Sample]) {
        let history = &preview.history;
        let session = (preview.instance, history.session);
        let end = history.first + history.samples.len() as u64;
        let reset = self.session != Some(session) || self.next < history.first || self.next > end;
        let begin = if reset {
            history.samples.len().saturating_sub(1)
        } else {
            (self.next - history.first) as usize
        };
        self.session = Some(session);
        self.next = end;
        (reset, &history.samples[begin..])
    }
}

impl History {
    pub fn frame(&self, request: u32, stamp: Stamp) -> io::Result<Vec<u8>> {
        let mut frame = Topic::Preview.open_frame(request, stamp, self.body_len())?;
        self.write_body(&mut frame)?;
        Ok(frame)
    }

    pub(crate) fn body_len(&self) -> usize {
        HEADER_BYTES
            + self.samples.len() * SAMPLE_BYTES
            + usize::from(self.imu.is_some()) * IMU_BYTES
    }

    pub(crate) fn write_body(&self, target: &mut Vec<u8>) -> io::Result<()> {
        if self.samples.len() > HISTORY_LIMIT
            || (self.device.is_none() && (!self.samples.is_empty() || self.imu.is_some()))
            || self.first.checked_add(self.samples.len() as u64).is_none()
        {
            return Err(invalid());
        }
        let device = match self.device {
            None => 0,
            Some(Device { model, transport }) => {
                1 + match model {
                    ModelSelection::DualSense => 0,
                    ModelSelection::DualSenseEdge => 1,
                    ModelSelection::Any => return Err(invalid()),
                } + match transport {
                    Selection::Usb => 0,
                    Selection::Bluetooth => 2,
                    Selection::Auto => return Err(invalid()),
                }
            }
        };
        target.extend(self.session.to_le_bytes());
        target.extend(self.first.to_le_bytes());
        target.extend([
            device,
            self.samples.len() as u8,
            u8::from(self.imu.is_some()),
        ]);
        for sample in &self.samples {
            target.extend(sample.buttons.bits().to_le_bytes());
            let [left, right] = sample.sticks;
            for value in [
                left.x,
                left.y,
                right.x,
                right.y,
                sample.triggers[0],
                sample.triggers[1],
            ] {
                target.extend(value.to_le_bytes());
            }
            let present = sample.touch.0.map(|contact| u8::from(contact.is_some()));
            target.push(present[0] | (present[1] << 1));
            for (x, y) in sample.touch.0.into_iter().map(Option::unwrap_or_default) {
                target.extend(x.to_le_bytes());
                target.extend(y.to_le_bytes());
            }
        }
        if let Some((imu, speed)) = self.imu {
            for value in imu.gyro_dps.into_iter().chain(imu.accel_g) {
                target.extend(value.to_le_bytes());
            }
            target.extend(speed.0.to_le_bytes());
        }
        Ok(())
    }

    pub(crate) fn read_body(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < HEADER_BYTES {
            return Err(invalid());
        }
        let session = u64::from_le_bytes(bytes[..8].try_into().unwrap());
        let first = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let device = match bytes[16] {
            0 => None,
            kind @ 1..=4 => Some(Device {
                model: if kind % 2 == 1 {
                    ModelSelection::DualSense
                } else {
                    ModelSelection::DualSenseEdge
                },
                transport: if kind <= 2 {
                    Selection::Usb
                } else {
                    Selection::Bluetooth
                },
            }),
            _ => return Err(invalid()),
        };
        let count = usize::from(bytes[17]);
        let calibrated = match bytes[18] {
            0 => false,
            1 => true,
            _ => return Err(invalid()),
        };
        let samples_end = HEADER_BYTES + count * SAMPLE_BYTES;
        if count > HISTORY_LIMIT
            || first.checked_add(count as u64).is_none()
            || (device.is_none() && (count != 0 || calibrated))
            || bytes.len() != samples_end + usize::from(calibrated) * IMU_BYTES
        {
            return Err(invalid());
        }
        let samples = bytes[HEADER_BYTES..samples_end]
            .as_chunks::<SAMPLE_BYTES>()
            .0
            .iter()
            .map(decode_sample)
            .collect::<io::Result<_>>()?;
        let imu = if calibrated {
            let values = &bytes[samples_end..];
            let [gx, gy, gz, ax, ay, az] = std::array::from_fn(|i| float(&values[i * 4..][..4]));
            let speed = f64::from_le_bytes(values[24..].try_into().unwrap());
            if ![gx, gy, gz, ax, ay, az]
                .iter()
                .all(|value| value.is_finite())
                || !speed.is_finite()
                || speed < 0.0
            {
                return Err(invalid());
            }
            Some((
                ImuReading {
                    gyro_dps: [gx, gy, gz],
                    accel_g: [ax, ay, az],
                },
                GyroSpeed(speed),
            ))
        } else {
            None
        };
        Ok(Self {
            session,
            first,
            device,
            samples,
            imu,
        })
    }
}

impl Preview {
    pub fn read(stream: &mut impl Read, request: u32) -> io::Result<(Stamp, Self)> {
        let len = Topic::Preview.read_header(stream, request, BODY_LIMIT)?;
        let stamp = Stamp::read(stream)?;
        let mut bytes = [0; BODY_LIMIT];
        stream.read_exact(&mut bytes[..len])?;
        Ok((
            stamp,
            Self {
                instance: stamp.instance,
                history: History::read_body(&bytes[..len])?,
            },
        ))
    }
}

fn decode_sample(bytes: &[u8; SAMPLE_BYTES]) -> io::Result<Sample> {
    let buttons = Buttons::from_bits(u32::from_le_bytes(bytes[..4].try_into().unwrap()))
        .ok_or_else(invalid)?;
    let [lx, ly, rx, ry, lt, rt] = std::array::from_fn(|i| float(&bytes[4 + i * 4..][..4]));
    let mask = bytes[28];
    if ![lx, ly, rx, ry]
        .iter()
        .all(|value| (-1.0..=1.0).contains(value))
        || ![lt, rt].iter().all(|value| (0.0..=1.0).contains(value))
        || mask > 3
    {
        return Err(invalid());
    }
    let touch = TouchPoints(std::array::from_fn(|i| {
        (mask & (1 << i) != 0).then(|| {
            let offset = 29 + i * 4;
            (
                u16::from_le_bytes(bytes[offset..][..2].try_into().unwrap()),
                u16::from_le_bytes(bytes[offset + 2..][..2].try_into().unwrap()),
            )
        })
    }));
    Ok(Sample {
        buttons,
        sticks: [Stick { x: lx, y: ly }, Stick { x: rx, y: ry }],
        triggers: [lt, rt],
        touch,
    })
}

fn float(bytes: &[u8]) -> f32 {
    f32::from_le_bytes(bytes.try_into().unwrap())
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid controller preview history",
    )
}

#[cfg(test)]
mod tests {
    use joyshockrust_core::Button;

    use super::*;

    fn sample(pressed: bool) -> Sample {
        let mut buttons = Buttons::default();
        if pressed {
            buttons.insert(Button::South);
        }
        Sample {
            buttons,
            sticks: [Stick::default(), Stick { x: -0.5, y: 0.75 }],
            triggers: [0.25, 1.0],
            touch: TouchPoints([None, Some((1919, 1079))]),
        }
    }

    fn history(session: u64, first: u64, samples: Vec<Sample>) -> History {
        History {
            session,
            device: Some(Device {
                model: ModelSelection::DualSenseEdge,
                transport: Selection::Bluetooth,
            }),
            first,
            samples,
            imu: Some((
                ImuReading {
                    gyro_dps: [-12.0, 1.0, 6.0],
                    accel_g: [0.0, 1.0, 0.0],
                },
                GyroSpeed(12.5),
            )),
        }
    }

    #[test]
    fn preview_wire_preserves_order_touch_slots_and_calibrated_values() {
        let history = history(2, 10, vec![sample(false), sample(true), sample(false)]);
        let stamp = Stamp {
            instance: 4,
            revision: 6,
        };
        let frame = history.frame(11, stamp).unwrap();
        assert_eq!(
            Preview::read(&mut frame.as_slice(), 11).unwrap(),
            (
                stamp,
                Preview {
                    instance: stamp.instance,
                    history: history.clone(),
                }
            )
        );
        assert!(Preview::read(&mut frame.as_slice(), 12).is_err());
        assert!(Preview::read(&mut &frame[..frame.len() - 1], 11).is_err());
        let oversized = History {
            samples: vec![sample(false); HISTORY_LIMIT + 1],
            ..history
        };
        assert!(oversized.frame(11, stamp).is_err());
    }

    #[test]
    fn cursor_keeps_intermediate_transitions_and_resets_after_gaps_and_reconnects() {
        let mut cursor = Cursor::default();
        let mut frame = Preview {
            instance: 1,
            history: history(7, 0, vec![sample(false)]),
        };
        assert_eq!(cursor.advance(&frame), (true, &[sample(false)][..]));
        frame.history.samples.extend([sample(true), sample(false)]);
        assert_eq!(
            cursor.advance(&frame),
            (false, &[sample(true), sample(false)][..])
        );
        assert_eq!(cursor.advance(&frame), (false, &[][..]));
        frame.history = history(7, 10, vec![sample(false), sample(true)]);
        assert_eq!(cursor.advance(&frame), (true, &[sample(true)][..]));
        frame.history = History {
            session: 8,
            device: None,
            first: 0,
            samples: vec![],
            imu: None,
        };
        assert_eq!(cursor.advance(&frame), (true, &[][..]));
        frame.history = history(9, 0, vec![sample(true)]);
        assert_eq!(cursor.advance(&frame), (true, &[sample(true)][..]));
        frame.instance = 2;
        assert_eq!(cursor.advance(&frame), (true, &[sample(true)][..]));
    }
}
