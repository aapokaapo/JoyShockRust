use std::{
    ffi::OsStr,
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use joyshockrust_core::trigger::{TriggerSide, validate_resistance};
use joyshockrust_core::validate_trigger_thresholds;

use crate::LOAD_SUFFIX;

pub const MAX_PROFILE_BYTES: usize = 64 * 1024;
pub const MAX_PROFILE_NAME_BYTES: usize = 4096;
pub const LOAD_PROFILE_PREFIX: &[u8] = b"\0load-profile\n";
pub const LOAD_PROFILE_PATH_PREFIX: &[u8] = b"\0load-profile-path\n";
pub const SELECT_CONTROLLER_PREFIX: &[u8] = b"\0select-controller\n";
pub const UNLOAD_PROFILE_REQUEST: &[u8] = b"\0unload-profile\n";
pub const SET_GAMEPAD_PREFIX: &[u8] = b"\0set-gamepad\n";
pub const SET_GYRO_MOUSE_PREFIX: &[u8] = b"\0set-gyro-mouse\n";
pub const SET_EMULATION_PREFIX: &[u8] = b"\0set-emulation\n";
pub const SET_EMULATION_MASTER_PREFIX: &[u8] = b"\0set-emulation-master\n";
pub const SET_OUTPUTS_PREFIX: &[u8] = b"\0set-outputs\n";
pub const TRIGGER_PREVIEW_PREFIX: &[u8] = b"\0preview-trigger\n";
pub const TRIGGER_TRAVEL_PREFIX: &[u8] = b"\0preview-trigger-travel\n";

pub const MAX_DATAGRAM_BYTES: usize =
    LOAD_PROFILE_PATH_PREFIX.len() + MAX_PROFILE_NAME_BYTES + 1 + MAX_PROFILE_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InlineProfile<'a> {
    pub name: Option<&'a OsStr>,
    pub path: Option<&'a Path>,
    pub contents: &'a [u8],
}

impl<'a> InlineProfile<'a> {
    pub fn decode(data: &'a [u8]) -> Result<Self, &'static str> {
        let (data, has_path) = if let Some(data) = data.strip_prefix(LOAD_PROFILE_PATH_PREFIX) {
            (data, true)
        } else if let Some(data) = data.strip_prefix(LOAD_PROFILE_PREFIX) {
            (data, false)
        } else {
            // Older clients send only the TOML bytes.
            return Ok(InlineProfile {
                name: None,
                path: None,
                contents: data,
            });
        };
        let end = data
            .iter()
            .position(|&byte| byte == 0)
            .ok_or("profile request needs a source and profile")?;
        let source = &data[..end];
        if source.is_empty() || source.len() > MAX_PROFILE_NAME_BYTES {
            return Err("invalid profile source");
        }
        let name = OsStr::from_bytes(source);
        let path = Path::new(name);
        let (name, path) = if has_path {
            if !path.is_absolute() || path.file_name().is_none() {
                return Err("expected an absolute profile path");
            }
            (path.file_name(), Some(path))
        } else {
            if source.contains(&b'/') {
                return Err("invalid profile filename");
            }
            (Some(name), None)
        };
        Ok(InlineProfile {
            name,
            path,
            contents: &data[end + 1..],
        })
    }
}

pub fn load_socket_path(reload_socket: &Path) -> PathBuf {
    let mut path = reload_socket.as_os_str().to_owned();
    path.push(LOAD_SUFFIX);
    path.into()
}

pub fn profile_path_payload(absolute: &Path, contents: &[u8]) -> io::Result<Vec<u8>> {
    let path = absolute.as_os_str().as_bytes();
    if !absolute.is_absolute()
        || path.len() > MAX_PROFILE_NAME_BYTES
        || absolute.file_name().is_none()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid profile path",
        ));
    }
    let mut payload =
        Vec::with_capacity(LOAD_PROFILE_PATH_PREFIX.len() + path.len() + 1 + contents.len());
    payload.extend_from_slice(LOAD_PROFILE_PATH_PREFIX);
    payload.extend_from_slice(path);
    payload.push(0);
    payload.extend_from_slice(contents);
    Ok(payload)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    Applied,
    Rejected,
    Unavailable,
}

impl Reply {
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            Self::Applied => b"applied\n",
            Self::Rejected => b"rejected\n",
            Self::Unavailable => b"unavailable\n",
        }
    }

    pub fn parse(bytes: &[u8]) -> Option<Self> {
        [Self::Applied, Self::Rejected, Self::Unavailable]
            .into_iter()
            .find(|reply| bytes == reply.bytes())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResistancePreview {
    Set {
        side: TriggerSide,
        start: u8,
        strength: u8,
    },
    Reset,
}

impl ResistancePreview {
    pub fn parse(payload: &[u8]) -> Result<Self, &'static str> {
        match payload {
            [] => Ok(Self::Reset),
            &[side @ 0..=1, start, strength] => {
                if start != 0 || strength != 0 {
                    validate_resistance(start, strength)?;
                }
                Ok(Self::Set {
                    side: decode_side(side),
                    start,
                    strength,
                })
            }
            _ => Err("invalid adaptive trigger preview"),
        }
    }

    pub fn encode(self) -> Vec<u8> {
        let mut request = TRIGGER_PREVIEW_PREFIX.to_vec();
        if let Self::Set {
            side,
            start,
            strength,
        } = self
        {
            request.extend_from_slice(&[encode_side(side), start, strength]);
        }
        request
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TravelPreview {
    pub side: TriggerSide,
    pub thresholds: [u8; 4],
    pub stages: [bool; 2],
    pub threshold_scale: u8,
    pub travel_limit: Option<u8>,
}

impl TravelPreview {
    pub fn parse(payload: &[u8]) -> Option<Option<Self>> {
        if payload.is_empty() {
            return Some(None);
        }
        let &[
            side @ 0..=1,
            partial,
            partial_release,
            full,
            full_release,
            ref rest @ ..,
        ] = payload
        else {
            return None;
        };
        let (mask, threshold_scale, travel_limit) = match rest {
            [] => (3, 100, None),
            &[mask @ 1..=3] => (mask, 100, None),
            &[mask @ 1..=3, scale @ 1..=100] => (mask, scale, None),
            &[mask @ 0..=3, scale @ 1..=100, limit @ 1..=100] => (mask, scale, Some(limit)),
            _ => return None,
        };
        let thresholds = [partial, partial_release, full, full_release];
        validate_trigger_thresholds(thresholds).ok()?;
        Some(Some(Self {
            side: decode_side(side),
            thresholds,
            stages: [mask & 1 != 0, mask & 2 != 0],
            threshold_scale,
            travel_limit,
        }))
    }

    pub fn encode(preview: Option<Self>) -> Vec<u8> {
        let mut request = TRIGGER_TRAVEL_PREFIX.to_vec();
        if let Some(preview) = preview {
            let mask = u8::from(preview.stages[0]) | (u8::from(preview.stages[1]) << 1);
            request.push(encode_side(preview.side));
            request.extend_from_slice(&preview.thresholds);
            request.extend_from_slice(&[mask, preview.threshold_scale]);
            if let Some(limit) = preview.travel_limit {
                request.push(limit);
            }
        }
        request
    }
}

fn encode_side(side: TriggerSide) -> u8 {
    u8::from(side == TriggerSide::Right)
}

fn decode_side(side: u8) -> TriggerSide {
    if side == 0 {
        TriggerSide::Left
    } else {
        TriggerSide::Right
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputTarget {
    Gamepad,
    GyroMouse,
    Both,
    Master,
}

impl OutputTarget {
    const ALL: [Self; 4] = [Self::Gamepad, Self::GyroMouse, Self::Both, Self::Master];

    pub fn from_flag(flag: &str) -> Option<Self> {
        match flag {
            "--set-gamepad" => Some(Self::Gamepad),
            "--set-gyro-mouse" => Some(Self::GyroMouse),
            "--set-emulation" => Some(Self::Both),
            "--set-emulation-master" => Some(Self::Master),
            _ => None,
        }
    }

    pub fn prefix(self) -> &'static [u8] {
        match self {
            Self::Gamepad => SET_GAMEPAD_PREFIX,
            Self::GyroMouse => SET_GYRO_MOUSE_PREFIX,
            Self::Both => SET_EMULATION_PREFIX,
            Self::Master => SET_EMULATION_MASTER_PREFIX,
        }
    }
}

/// Commands that expect a reply from the running bridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlCommand<'a> {
    UnloadProfile,
    Emulation {
        target: OutputTarget,
        enabled: bool,
    },
    OutputsPaused(bool),
    SelectController {
        id: &'a str,
        model: Option<&'a str>,
        transport: Option<&'a str>,
    },
}

impl ControlCommand<'_> {
    pub fn encode(self) -> Vec<u8> {
        match self {
            Self::UnloadProfile => UNLOAD_PROFILE_REQUEST.to_vec(),
            Self::Emulation { target, enabled } => {
                let mut payload = target.prefix().to_vec();
                payload.extend_from_slice(if enabled { b"on" } else { b"off" });
                payload
            }
            Self::OutputsPaused(paused) => {
                let mut payload = SET_OUTPUTS_PREFIX.to_vec();
                payload.extend_from_slice(if paused { b"pause" } else { b"resume" });
                payload
            }
            Self::SelectController {
                id,
                model,
                transport,
            } => {
                let mut payload = SELECT_CONTROLLER_PREFIX.to_vec();
                payload.extend_from_slice(id.as_bytes());
                if model.is_some() || transport.is_some() {
                    payload.push(b'\n');
                    payload.extend_from_slice(model.unwrap_or_default().as_bytes());
                    payload.push(b'\n');
                    payload.extend_from_slice(transport.unwrap_or_default().as_bytes());
                }
                payload
            }
        }
    }

    pub fn operation(self) -> &'static str {
        match self {
            Self::UnloadProfile => "profile unload",
            Self::Emulation { .. } => "emulation update",
            Self::OutputsPaused(_) => "virtual output update",
            Self::SelectController { .. } => "controller selection",
        }
    }
}

/// Datagrams from a client with a reply address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressedCommand<'a> {
    Control(ControlCommand<'a>),
    LoadProfile(InlineProfile<'a>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Profile(&'static str),
    Command(&'static str),
}

impl<'a> AddressedCommand<'a> {
    pub fn decode(data: &'a [u8]) -> Result<Self, DecodeError> {
        let command = if data == UNLOAD_PROFILE_REQUEST {
            ControlCommand::UnloadProfile
        } else if let Some((target, value)) = OutputTarget::ALL.into_iter().find_map(|target| {
            data.strip_prefix(target.prefix())
                .map(|value| (target, value))
        }) {
            let enabled = match value {
                b"on" => true,
                b"off" => false,
                _ => return Err(DecodeError::Command("expected on or off")),
            };
            ControlCommand::Emulation { target, enabled }
        } else if let Some(value) = data.strip_prefix(SET_OUTPUTS_PREFIX) {
            ControlCommand::OutputsPaused(match value {
                b"pause" => true,
                b"resume" => false,
                _ => return Err(DecodeError::Command("expected pause or resume")),
            })
        } else if let Some(value) = data.strip_prefix(SELECT_CONTROLLER_PREFIX) {
            let value = std::str::from_utf8(value)
                .map_err(|_| DecodeError::Command("invalid controller selection"))?;
            let mut fields = value.split('\n');
            let id = fields.next().unwrap_or_default();
            let model = fields.next().filter(|value| !value.is_empty());
            let transport = fields.next().filter(|value| !value.is_empty());
            if fields.next().is_some() {
                return Err(DecodeError::Command(
                    "unexpected controller selection fields",
                ));
            }
            ControlCommand::SelectController {
                id,
                model,
                transport,
            }
        } else if data.starts_with(LOAD_PROFILE_PATH_PREFIX)
            || data.starts_with(LOAD_PROFILE_PREFIX)
            || !data.starts_with(b"\0")
        {
            let profile = InlineProfile::decode(data).map_err(DecodeError::Profile)?;
            if profile.contents.len() > MAX_PROFILE_BYTES {
                return Err(DecodeError::Profile("profile exceeds 64 KiB"));
            }
            return Ok(Self::LoadProfile(profile));
        } else {
            return Err(DecodeError::Command("unknown control command"));
        };
        Ok(Self::Control(command))
    }
}

/// Fire-and-forget datagrams sent by the editor from an unnamed socket.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PreviewCommand {
    Resistance(ResistancePreview),
    Travel(Option<TravelPreview>),
}

impl PreviewCommand {
    pub fn decode(data: &[u8]) -> Result<Self, &'static str> {
        if let Some(payload) = data.strip_prefix(TRIGGER_PREVIEW_PREFIX) {
            return ResistancePreview::parse(payload).map(Self::Resistance);
        }
        if let Some(payload) = data.strip_prefix(TRIGGER_TRAVEL_PREFIX) {
            return TravelPreview::parse(payload)
                .map(Self::Travel)
                .ok_or("invalid trigger travel preview");
        }
        Err("unknown preview command")
    }
}

impl Reply {
    pub fn check(response: &[u8], operation: &str) -> io::Result<()> {
        match Self::parse(response) {
            Some(Self::Applied) => Ok(()),
            Some(Self::Unavailable) => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no startup profile configured; use --load-profile PATH",
            )),
            Some(Self::Rejected) => Err(io::Error::other(format!(
                "{operation} rejected; see the bridge log for details"
            ))),
            None => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{operation} returned no valid result"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addressed_controls_round_trip_with_existing_wire_values() {
        use ControlCommand::*;
        for (command, wire) in [
            (UnloadProfile, &b"\0unload-profile\n"[..]),
            (OutputsPaused(true), b"\0set-outputs\npause"),
            (OutputsPaused(false), b"\0set-outputs\nresume"),
            (
                SelectController {
                    id: "any",
                    model: None,
                    transport: None,
                },
                b"\0select-controller\nany",
            ),
            (
                SelectController {
                    id: "00:11:22:33:44:55",
                    model: None,
                    transport: Some("usb"),
                },
                b"\0select-controller\n00:11:22:33:44:55\n\nusb",
            ),
        ] {
            assert_eq!(command.encode(), wire);
            assert_eq!(
                AddressedCommand::decode(wire),
                Ok(AddressedCommand::Control(command))
            );
        }
        for target in OutputTarget::ALL {
            for enabled in [true, false] {
                let command = Emulation { target, enabled };
                assert_eq!(
                    AddressedCommand::decode(&command.encode()),
                    Ok(AddressedCommand::Control(command))
                );
            }
        }
        assert_eq!(
            AddressedCommand::decode(b"\0select-controller\nany\ndualsense\n"),
            Ok(AddressedCommand::Control(SelectController {
                id: "any",
                model: Some("dualsense"),
                transport: None,
            }))
        );
    }

    #[test]
    fn datagram_decoders_separate_profiles_controls_and_previews() {
        for malformed in [
            &b"\0unknown\n"[..],
            b"\0set-emulation-master\nmaybe",
            b"\0set-outputs\npause\n",
            b"\0select-controller\n\xff",
            b"\0select-controller\nany\n\nusb\nextra",
            b"\0unload-profile\nextra",
            b"\0preview-trigger\n",
            b"\0preview-trigger-travel\n",
        ] {
            assert!(matches!(
                AddressedCommand::decode(malformed),
                Err(DecodeError::Command(_))
            ));
        }
        for malformed in [
            &b"\0load-profile-path\nrelative\0version = 1"[..],
            b"\0load-profile\nmissing separator",
        ] {
            assert!(matches!(
                AddressedCommand::decode(malformed),
                Err(DecodeError::Profile(_))
            ));
        }

        let raw = b"version = 1\n";
        assert!(
            matches!(AddressedCommand::decode(raw), Ok(AddressedCommand::LoadProfile(profile))
            if profile.name.is_none() && profile.path.is_none() && profile.contents == raw)
        );
        let named = b"\0load-profile\nlegacy \xff.toml\0version = 1\n";
        assert!(
            matches!(AddressedCommand::decode(named), Ok(AddressedCommand::LoadProfile(profile))
            if profile.name == Some(OsStr::from_bytes(b"legacy \xff.toml"))
                && profile.path.is_none() && profile.contents == raw)
        );

        let path = PathBuf::from(format!("/{}", "x".repeat(MAX_PROFILE_NAME_BYTES - 1)));
        let max_profile = vec![b' '; MAX_PROFILE_BYTES];
        let mut max_frame = profile_path_payload(&path, &max_profile).unwrap();
        assert_eq!(max_frame.len(), MAX_DATAGRAM_BYTES);
        assert!(
            matches!(AddressedCommand::decode(&max_frame), Ok(AddressedCommand::LoadProfile(profile))
            if profile.path == Some(path.as_path()) && profile.contents == max_profile)
        );
        max_frame.push(b' ');
        assert_eq!(
            AddressedCommand::decode(&max_frame),
            Err(DecodeError::Profile("profile exceeds 64 KiB"))
        );

        assert!(PreviewCommand::decode(raw).is_err());
        assert!(PreviewCommand::decode(&ControlCommand::UnloadProfile.encode()).is_err());
        assert_eq!(
            PreviewCommand::decode(b"\0preview-trigger\n"),
            Ok(PreviewCommand::Resistance(ResistancePreview::Reset))
        );
        assert_eq!(
            PreviewCommand::decode(b"\0preview-trigger-travel\n"),
            Ok(PreviewCommand::Travel(None))
        );
        assert!(PreviewCommand::decode(b"\0preview-trigger\n\x02\x03\x07").is_err());
        assert!(PreviewCommand::decode(b"\0preview-trigger-travel\n\x02").is_err());
    }

    #[test]
    fn travel_codec_accepts_the_existing_layouts() {
        for bytes in [
            &[0, 45, 35, 95, 85][..],
            &[1, 45, 35, 95, 85, 1],
            &[0, 45, 35, 95, 85, 2, 80],
            &[1, 45, 35, 95, 85, 0, 100, 60],
        ] {
            let request = TravelPreview::parse(bytes).unwrap().unwrap();
            let encoded = TravelPreview::encode(Some(request));
            assert_eq!(
                TravelPreview::parse(encoded.strip_prefix(TRIGGER_TRAVEL_PREFIX).unwrap()),
                Some(Some(request))
            );
        }
        assert!(TravelPreview::parse(&[1, 45, 35, 95, 85, 0, 100, 0]).is_none());
        assert!(TravelPreview::parse(&[1, 45, 35, 95, 85, 0]).is_none());
        assert_eq!(TravelPreview::parse(&[]), Some(None));
    }

    #[test]
    fn addressed_profile_response_and_unnamed_preview_wire_values_are_stable() {
        let bytes = profile_path_payload(Path::new("/tmp/profile.toml"), b"version = 1\n").unwrap();
        assert_eq!(
            bytes,
            b"\0load-profile-path\n/tmp/profile.toml\0version = 1\n"
        );
        let decoded = InlineProfile::decode(&bytes).unwrap();
        assert_eq!(decoded.name, Some(OsStr::new("profile.toml")));
        assert_eq!(decoded.contents, b"version = 1\n");
        assert_eq!(InlineProfile::decode(b"version = 1").unwrap().path, None);
        assert!(InlineProfile::decode(b"\0load-profile-path\nrelative\0version = 1").is_err());
        assert_eq!(
            ResistancePreview::parse(&[1, 3, 7]).unwrap().encode(),
            b"\0preview-trigger\n\x01\x03\x07"
        );
        assert_eq!(Reply::parse(b"applied\n"), Some(Reply::Applied));
        assert_eq!(Reply::parse(b"applied"), None);
    }
}
