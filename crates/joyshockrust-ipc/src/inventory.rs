//! Versioned, read-only controller inventory subscription.

use std::io::{self, Read};

use joyshockrust_config::controller::{ControllerId, ModelSelection, Selection};

use crate::stream::{Stamp, Topic};

pub const MAX_CONTROLLERS: usize = 128;
const CONTROLLER_LEN: usize = 13;
const MAX_BODY: usize = 1 + MAX_CONTROLLERS * CONTROLLER_LEN;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Unreadable,
    Readable(Option<ControllerId>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controller {
    pub hidraw: u32,
    pub model: ModelSelection,
    pub transport: Selection,
    pub access: Access,
}

impl Controller {
    pub fn node(self) -> String {
        format!("/dev/hidraw{}", self.hidraw)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub instance: u128,
    pub revision: u64,
    pub controllers: Option<Vec<Controller>>,
}

impl Snapshot {
    pub fn encode(&self, request_id: u32) -> io::Result<Vec<u8>> {
        let controllers = self.controllers.as_deref().unwrap_or(&[]);
        if controllers.len() > MAX_CONTROLLERS {
            return Err(invalid("too many controllers in inventory"));
        }
        let stamp = Stamp {
            instance: self.instance,
            revision: self.revision,
        };
        let mut frame = Topic::Inventory.open_frame(
            request_id,
            stamp,
            1 + controllers.len() * CONTROLLER_LEN,
        )?;
        frame.push(u8::from(self.controllers.is_some()));
        for controller in controllers {
            let model = match controller.model {
                ModelSelection::DualSense => 0,
                ModelSelection::DualSenseEdge => 1,
                ModelSelection::Any => return Err(invalid("expected a controller model")),
            };
            let transport = match controller.transport {
                Selection::Usb => 0,
                Selection::Bluetooth => 1,
                Selection::Auto => return Err(invalid("expected a controller transport")),
            };
            let (access, id) = match controller.access {
                Access::Unreadable => (0, [0; 6]),
                Access::Readable(None) => (1, [0; 6]),
                Access::Readable(Some(id)) => (2, id.bytes()),
            };
            frame.extend(controller.hidraw.to_le_bytes());
            frame.extend([model, transport, access]);
            frame.extend(id);
        }
        Ok(frame)
    }

    pub fn read(stream: &mut impl Read, request_id: u32) -> io::Result<Self> {
        let length = Topic::Inventory.read_header(stream, request_id, MAX_BODY)?;
        if length == 0 || !(length - 1).is_multiple_of(CONTROLLER_LEN) {
            return Err(invalid("invalid controller inventory length"));
        }
        let Stamp { instance, revision } = Stamp::read(stream)?;
        let mut payload = [0; MAX_BODY];
        let payload = &mut payload[..length];
        stream.read_exact(payload)?;
        let controllers = match payload[0] {
            0 if length == 1 => None,
            1 => Some(
                payload[1..]
                    .as_chunks::<CONTROLLER_LEN>()
                    .0
                    .iter()
                    .map(decode_controller)
                    .collect::<io::Result<_>>()?,
            ),
            _ => return Err(invalid("invalid inventory availability")),
        };
        Ok(Self {
            instance,
            revision,
            controllers,
        })
    }
}

fn decode_controller(entry: &[u8; CONTROLLER_LEN]) -> io::Result<Controller> {
    let model = match entry[4] {
        0 => ModelSelection::DualSense,
        1 => ModelSelection::DualSenseEdge,
        _ => return Err(invalid("invalid inventory model")),
    };
    let transport = match entry[5] {
        0 => Selection::Usb,
        1 => Selection::Bluetooth,
        _ => return Err(invalid("invalid inventory transport")),
    };
    let id = entry[7..].try_into().unwrap();
    let access = match entry[6] {
        0 if id == [0; 6] => Access::Unreadable,
        1 if id == [0; 6] => Access::Readable(None),
        2 => Access::Readable(Some(ControllerId::from_bytes(id))),
        _ => return Err(invalid("invalid inventory access")),
    };
    Ok(Controller {
        hidraw: u32::from_le_bytes(entry[..4].try_into().unwrap()),
        model,
        transport,
        access,
    })
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_round_trip_distinguishes_unavailable_empty_and_unreadable() {
        let controller = Controller {
            hidraw: 123,
            model: ModelSelection::DualSenseEdge,
            transport: Selection::Bluetooth,
            access: Access::Unreadable,
        };
        for controllers in [
            None,
            Some(vec![]),
            Some(vec![
                controller,
                Controller {
                    access: Access::Readable(None),
                    ..controller
                },
                Controller {
                    access: Access::Readable(ControllerId::parse("00:11:22:33:44:55")),
                    ..controller
                },
            ]),
        ] {
            let snapshot = Snapshot {
                instance: 5,
                revision: 9,
                controllers,
            };
            let frame = snapshot.encode(17).unwrap();
            assert_eq!(Snapshot::read(&mut frame.as_slice(), 17).unwrap(), snapshot);
            assert!(Snapshot::read(&mut frame.as_slice(), 18).is_err());
            assert!(Snapshot::read(&mut &frame[..frame.len() - 1], 17).is_err());
        }
    }

    #[test]
    fn incompatible_and_oversized_messages_are_rejected_before_payload_reads() {
        let mut request = Topic::Inventory.subscribe(42);
        assert_eq!(Topic::Inventory.parse_subscription(&request).unwrap(), 42);
        request[4] = 255;
        assert!(Topic::Inventory.parse_subscription(&request).is_err());
        let oversized = Topic::Inventory
            .open_frame(
                42,
                Stamp {
                    instance: 1,
                    revision: 1,
                },
                MAX_BODY + CONTROLLER_LEN,
            )
            .unwrap();
        assert_eq!(
            Snapshot::read(&mut oversized.as_slice(), 42)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
