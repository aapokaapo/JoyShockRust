//! Versioned, read-only controller inventory subscription.

use std::{
    io::{self, Read},
    path::{Path, PathBuf},
};

use joyshockrust_config::controller::{ControllerId, ModelSelection, Selection};

const MAGIC: &[u8; 4] = b"JSRI";
const VERSION: u8 = 1;
const SUBSCRIBE: u8 = 1;
const SNAPSHOT: u8 = 2;
pub const REQUEST_LEN: usize = 12;
pub const MAX_CONTROLLERS: usize = 128;
const SNAPSHOT_LEN: usize = 25; // instance, revision, availability
const CONTROLLER_LEN: usize = 13;
const MAX_PAYLOAD: usize = SNAPSHOT_LEN + MAX_CONTROLLERS * CONTROLLER_LEN;

pub fn socket_path(status: &Path) -> PathBuf {
    let mut path = status.as_os_str().to_owned();
    path.push(".inventory");
    path.into()
}

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

pub fn subscribe(request_id: u32) -> [u8; REQUEST_LEN] {
    header(SUBSCRIBE, request_id, 0)
}

pub fn parse_subscription(request: &[u8; REQUEST_LEN]) -> io::Result<u32> {
    let (id, length) = parse_header(request, SUBSCRIBE)?;
    if length != 0 {
        return Err(invalid("inventory subscription must be empty"));
    }
    Ok(id)
}

impl Snapshot {
    pub fn encode(&self, request_id: u32) -> io::Result<Vec<u8>> {
        let controllers = self.controllers.as_deref().unwrap_or(&[]);
        if controllers.len() > MAX_CONTROLLERS {
            return Err(invalid("too many controllers in inventory"));
        }
        let length = SNAPSHOT_LEN + controllers.len() * CONTROLLER_LEN;
        let mut frame = Vec::with_capacity(REQUEST_LEN + length);
        frame.extend(header(SNAPSHOT, request_id, length));
        frame.extend(self.instance.to_le_bytes());
        frame.extend(self.revision.to_le_bytes());
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
        let mut header = [0; REQUEST_LEN];
        stream.read_exact(&mut header)?;
        let (id, length) = parse_header(&header, SNAPSHOT)?;
        if id != request_id
            || !(SNAPSHOT_LEN..=MAX_PAYLOAD).contains(&length)
            || !(length - SNAPSHOT_LEN).is_multiple_of(CONTROLLER_LEN)
        {
            return Err(invalid("invalid inventory snapshot header"));
        }
        let mut payload = [0; MAX_PAYLOAD];
        let payload = &mut payload[..length];
        stream.read_exact(payload)?;
        let instance = u128::from_le_bytes(payload[..16].try_into().unwrap());
        let revision = u64::from_le_bytes(payload[16..24].try_into().unwrap());
        let controllers = match payload[24] {
            0 if length == SNAPSHOT_LEN => None,
            1 => Some(
                payload[SNAPSHOT_LEN..]
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

fn header(kind: u8, request_id: u32, payload_len: usize) -> [u8; REQUEST_LEN] {
    let mut bytes = [0; REQUEST_LEN];
    bytes[..4].copy_from_slice(MAGIC);
    bytes[4] = VERSION;
    bytes[5] = kind;
    bytes[6..10].copy_from_slice(&request_id.to_le_bytes());
    bytes[10..].copy_from_slice(&(payload_len as u16).to_le_bytes());
    bytes
}

fn parse_header(bytes: &[u8; REQUEST_LEN], kind: u8) -> io::Result<(u32, usize)> {
    if &bytes[..4] != MAGIC || bytes[4] != VERSION || bytes[5] != kind {
        return Err(invalid("unsupported inventory protocol or message"));
    }
    Ok((
        u32::from_le_bytes(bytes[6..10].try_into().unwrap()),
        usize::from(u16::from_le_bytes(bytes[10..].try_into().unwrap())),
    ))
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
        let mut request = subscribe(42);
        assert_eq!(parse_subscription(&request).unwrap(), 42);
        request[4] = VERSION + 1;
        assert!(parse_subscription(&request).is_err());
        let oversized = header(SNAPSHOT, 42, MAX_PAYLOAD + CONTROLLER_LEN);
        assert_eq!(
            Snapshot::read(&mut oversized.as_slice(), 42)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
