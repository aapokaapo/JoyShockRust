//! Framing for versioned desktop subscriptions.

use std::{
    io::{self, Read},
    path::{Path, PathBuf},
};

const MAGIC: &[u8; 4] = b"JSRI";
const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 12;
pub const STAMP_LEN: usize = 24;
pub const STATUS_LIMIT: usize = u16::MAX as usize - STAMP_LEN;

#[derive(Clone, Copy)]
pub enum Topic {
    Inventory,
    Status,
    Preview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub instance: u128,
    pub revision: u64,
}

impl Topic {
    pub fn socket_path(self, status: &Path) -> PathBuf {
        let mut path = status.as_os_str().to_owned();
        path.push(match self {
            Self::Inventory => ".inventory",
            Self::Status => ".updates",
            Self::Preview => ".preview",
        });
        path.into()
    }

    fn request_kind(self) -> u8 {
        match self {
            Self::Inventory => 1,
            Self::Status => 3,
            Self::Preview => 5,
        }
    }

    pub fn subscribe(self, id: u32) -> [u8; HEADER_LEN] {
        header(self.request_kind(), id, 0)
    }

    pub fn parse_subscription(self, request: &[u8; HEADER_LEN]) -> io::Result<u32> {
        let (id, length) = parse_header(request, self.request_kind())?;
        if length != 0 {
            return Err(invalid("subscription request must be empty"));
        }
        Ok(id)
    }

    pub fn open_frame(self, id: u32, stamp: Stamp, body_len: usize) -> io::Result<Vec<u8>> {
        let length = body_len
            .checked_add(STAMP_LEN)
            .filter(|&length| length <= u16::MAX.into())
            .ok_or_else(|| invalid("subscription snapshot is too large"))?;
        let mut frame = Vec::with_capacity(HEADER_LEN + length);
        frame.extend(header(self.request_kind() + 1, id, length as u16));
        frame.extend(stamp.instance.to_le_bytes());
        frame.extend(stamp.revision.to_le_bytes());
        Ok(frame)
    }

    pub fn read_header(
        self,
        stream: &mut impl Read,
        request: u32,
        body_limit: usize,
    ) -> io::Result<usize> {
        let mut header = [0; HEADER_LEN];
        stream.read_exact(&mut header)?;
        let (id, length) = parse_header(&header, self.request_kind() + 1)?;
        let body_len = length
            .checked_sub(STAMP_LEN)
            .filter(|&len| id == request && len <= body_limit)
            .ok_or_else(|| invalid("invalid subscription snapshot header"))?;
        Ok(body_len)
    }
}

impl Stamp {
    pub fn read(stream: &mut impl Read) -> io::Result<Self> {
        let mut bytes = [0; STAMP_LEN];
        stream.read_exact(&mut bytes)?;
        Ok(Self {
            instance: u128::from_le_bytes(bytes[..16].try_into().unwrap()),
            revision: u64::from_le_bytes(bytes[16..].try_into().unwrap()),
        })
    }
}

pub fn encode_status(id: u32, stamp: Stamp, status: &str) -> io::Result<Vec<u8>> {
    let mut frame = Topic::Status.open_frame(id, stamp, status.len())?;
    frame.extend_from_slice(status.as_bytes());
    Ok(frame)
}

pub fn read_status(stream: &mut impl Read, id: u32) -> io::Result<(Stamp, String)> {
    let length = Topic::Status.read_header(stream, id, STATUS_LIMIT)?;
    let stamp = Stamp::read(stream)?;
    let mut body = vec![0; length];
    stream.read_exact(&mut body)?;
    let status = String::from_utf8(body).map_err(|_| invalid("status snapshot is not UTF-8"))?;
    Ok((stamp, status))
}

fn header(kind: u8, request_id: u32, payload_len: u16) -> [u8; HEADER_LEN] {
    let mut bytes = [0; HEADER_LEN];
    bytes[..4].copy_from_slice(MAGIC);
    bytes[4] = VERSION;
    bytes[5] = kind;
    bytes[6..10].copy_from_slice(&request_id.to_le_bytes());
    bytes[10..].copy_from_slice(&payload_len.to_le_bytes());
    bytes
}

fn parse_header(bytes: &[u8; HEADER_LEN], kind: u8) -> io::Result<(u32, usize)> {
    if &bytes[..4] != MAGIC || bytes[4] != VERSION || bytes[5] != kind {
        return Err(invalid("unsupported subscription protocol or message"));
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
    fn status_frames_are_bounded_and_separate_from_inventory() {
        let stamp = Stamp {
            instance: 7,
            revision: 19,
        };
        let status = "version=1\ncontroller=waiting\n";
        let frame = encode_status(9, stamp, status).unwrap();
        assert_eq!(
            read_status(&mut frame.as_slice(), 9).unwrap(),
            (stamp, status.into())
        );
        assert!(read_status(&mut frame.as_slice(), 8).is_err());
        assert!(
            Topic::Inventory
                .read_header(&mut frame.as_slice(), 9, STATUS_LIMIT)
                .is_err()
        );
        assert!(
            Topic::Status
                .parse_subscription(&Topic::Inventory.subscribe(9))
                .is_err()
        );
        assert!(encode_status(9, stamp, &"a".repeat(STATUS_LIMIT + 1)).is_err());
        assert!(read_status(&mut &frame[..frame.len() - 1], 9).is_err());
    }
}
