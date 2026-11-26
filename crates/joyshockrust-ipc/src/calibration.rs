//! Line protocol for bridge-owned calibration sessions.
//!
//! A client opens one stream connection per session. The connection is the
//! session's lifetime: when it closes for any reason, the bridge cancels any
//! running measurement and restores the output state it saw before the
//! session started. Each message is one UTF-8 line terminated by `\n`.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

/// Suffix appended to the reload socket path to form the session socket.
pub const CALIBRATION_SUFFIX: &str = ".calibration";

/// Longest accepted line, including the newline.
pub const MAX_LINE_BYTES: usize = 8 * 1024;

pub fn calibration_socket_path(reload_socket: &Path) -> PathBuf {
    let mut path = reload_socket.as_os_str().to_owned();
    path.push(CALIBRATION_SUFFIX);
    path.into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
    /// Stationary gyro bias measurement for one controller.
    Gyro,
    /// Real-world calibration (RWC) stick sweep saved to a profile.
    Game,
}

impl SessionKind {
    fn token(self) -> &'static str {
        match self {
            Self::Gyro => "gyro",
            Self::Game => "game",
        }
    }

    fn parse(token: &str) -> Option<Self> {
        match token {
            "gyro" => Some(Self::Gyro),
            "game" => Some(Self::Game),
            _ => None,
        }
    }
}

/// Measurement progress reported by the bridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    Ready,
    Started,
    CenterStick,
    NoMotion,
}

impl Progress {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Started => "started",
            Self::CenterStick => "center-stick",
            Self::NoMotion => "no-motion",
        }
    }

    pub fn parse(token: &str) -> Option<Self> {
        [
            Self::Ready,
            Self::Started,
            Self::CenterStick,
            Self::NoMotion,
        ]
        .into_iter()
        .find(|progress| progress.token() == token)
    }
}

/// Why the bridge ended a session on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// No client request arrived within the idle limit.
    Idle,
}

impl CloseReason {
    fn token(self) -> &'static str {
        match self {
            Self::Idle => "idle",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClientMessage {
    /// First message: request exclusive use of the controller at `node`.
    Begin {
        kind: SessionKind,
        node: PathBuf,
    },
    MeasureGyro,
    MeasureGame {
        sensitivity: f64,
    },
    /// Save the gyro bias from the named measurement.
    SaveGyro {
        measurement: u64,
    },
    /// Save the RWC result from the named measurement to `profile`.
    SaveGame {
        measurement: u64,
        turns: u32,
        profile: PathBuf,
    },
    /// Stop a running measurement. The session stays open.
    Cancel,
    /// End the session and restore outputs.
    End,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BridgeMessage {
    /// The session owns the controller. Outputs are paused.
    Started {
        session: u64,
    },
    /// Another session owns the controller.
    Busy,
    /// The request was invalid in the current state. The session stays open.
    Rejected(String),
    Progress(Progress),
    GyroMeasured {
        measurement: u64,
        controller: String,
        bias_dps: [f32; 3],
    },
    GameMeasured {
        measurement: u64,
        counts: i32,
    },
    GyroSaved,
    GameSaved {
        calibration: f64,
    },
    /// A measurement or save failed. The session stays open.
    Failed(String),
    /// A running measurement was stopped.
    Cancelled,
    /// The bridge ended the session and restored outputs.
    Closed(CloseReason),
}

fn single_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

fn path_text(path: &Path) -> Result<&str, &'static str> {
    let text = path.to_str().ok_or("path is not UTF-8")?;
    if !path.is_absolute() || text.contains(['\n', '\r']) {
        return Err("expected an absolute single-line path");
    }
    Ok(text)
}

fn number<T: std::str::FromStr>(text: Option<&str>) -> Result<T, &'static str> {
    text.and_then(|text| text.parse().ok())
        .ok_or("invalid number")
}

impl ClientMessage {
    pub fn encode(&self) -> Result<String, &'static str> {
        let line = match self {
            Self::Begin { kind, node } => format!("begin {} {}", kind.token(), path_text(node)?),
            Self::MeasureGyro => "measure-gyro".to_owned(),
            Self::MeasureGame { sensitivity } => {
                if !(sensitivity.is_finite() && *sensitivity > 0.0) {
                    return Err("sensitivity must be finite and positive");
                }
                format!("measure-game {sensitivity:?}")
            }
            Self::SaveGyro { measurement } => format!("save-gyro {measurement}"),
            Self::SaveGame {
                measurement,
                turns,
                profile,
            } => format!("save-game {measurement} {turns} {}", path_text(profile)?),
            Self::Cancel => "cancel".to_owned(),
            Self::End => "end".to_owned(),
        };
        if line.len() >= MAX_LINE_BYTES {
            return Err("message too long");
        }
        Ok(line + "\n")
    }

    /// Decode one line without its trailing newline.
    pub fn decode(line: &str) -> Result<Self, &'static str> {
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        let mut fields = rest.split_ascii_whitespace();
        let message = match command {
            "begin" => {
                let (kind, node) = rest.split_once(' ').ok_or("expected kind and node")?;
                let kind = SessionKind::parse(kind).ok_or("unknown session kind")?;
                let node = PathBuf::from(node);
                path_text(&node)?;
                return Ok(Self::Begin { kind, node });
            }
            "measure-gyro" if rest.is_empty() => Self::MeasureGyro,
            "measure-game" => {
                let sensitivity: f64 = number(fields.next())?;
                if !(sensitivity.is_finite() && sensitivity > 0.0) {
                    return Err("sensitivity must be finite and positive");
                }
                Self::MeasureGame { sensitivity }
            }
            "save-gyro" => Self::SaveGyro {
                measurement: number(fields.next())?,
            },
            "save-game" => {
                let mut parts = rest.splitn(3, ' ');
                let measurement = number(parts.next())?;
                let turns = number(parts.next())?;
                let profile = PathBuf::from(parts.next().ok_or("expected a profile path")?);
                path_text(&profile)?;
                return Ok(Self::SaveGame {
                    measurement,
                    turns,
                    profile,
                });
            }
            "cancel" if rest.is_empty() => Self::Cancel,
            "end" if rest.is_empty() => Self::End,
            _ => return Err("unknown calibration request"),
        };
        if fields.next().is_some() {
            return Err("unexpected request fields");
        }
        Ok(message)
    }
}

impl BridgeMessage {
    pub fn encode(&self) -> String {
        let mut line = String::new();
        // Writing to a String cannot fail.
        let _ = match self {
            Self::Started { session } => write!(line, "started {session}"),
            Self::Busy => write!(line, "busy"),
            Self::Rejected(reason) => write!(line, "rejected {}", single_line(reason)),
            Self::Progress(progress) => write!(line, "progress {}", progress.token()),
            Self::GyroMeasured {
                measurement,
                controller,
                bias_dps: [x, y, z],
            } => write!(
                line,
                "gyro-measured {measurement} {} {x:?} {y:?} {z:?}",
                single_line(controller).replace(' ', "_")
            ),
            Self::GameMeasured {
                measurement,
                counts,
            } => write!(line, "game-measured {measurement} {counts}"),
            Self::GyroSaved => write!(line, "gyro-saved"),
            Self::GameSaved { calibration } => write!(line, "game-saved {calibration:?}"),
            Self::Failed(reason) => write!(line, "failed {}", single_line(reason)),
            Self::Cancelled => write!(line, "cancelled"),
            Self::Closed(reason) => write!(line, "closed {}", reason.token()),
        };
        line.truncate(MAX_LINE_BYTES - 1);
        line.push('\n');
        line
    }

    /// Decode one line without its trailing newline.
    pub fn decode(line: &str) -> Result<Self, &'static str> {
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        let mut fields = rest.split_ascii_whitespace();
        let message = match command {
            "started" => Self::Started {
                session: number(fields.next())?,
            },
            "busy" if rest.is_empty() => Self::Busy,
            "rejected" => return Ok(Self::Rejected(rest.to_owned())),
            "failed" => return Ok(Self::Failed(rest.to_owned())),
            "progress" => Self::Progress(
                fields
                    .next()
                    .and_then(Progress::parse)
                    .ok_or("unknown progress")?,
            ),
            "gyro-measured" => Self::GyroMeasured {
                measurement: number(fields.next())?,
                controller: fields
                    .next()
                    .filter(|id| !id.is_empty())
                    .ok_or("expected a controller ID")?
                    .to_owned(),
                bias_dps: [
                    number(fields.next())?,
                    number(fields.next())?,
                    number(fields.next())?,
                ],
            },
            "game-measured" => Self::GameMeasured {
                measurement: number(fields.next())?,
                counts: number(fields.next())?,
            },
            "gyro-saved" if rest.is_empty() => Self::GyroSaved,
            "game-saved" => Self::GameSaved {
                calibration: number(fields.next())?,
            },
            "cancelled" if rest.is_empty() => Self::Cancelled,
            "closed" => match fields.next() {
                Some("idle") => Self::Closed(CloseReason::Idle),
                _ => return Err("unknown close reason"),
            },
            _ => return Err("unknown calibration reply"),
        };
        if fields.next().is_some() {
            return Err("unexpected reply fields");
        }
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_client(message: ClientMessage) {
        let line = message.encode().unwrap();
        assert!(line.ends_with('\n'));
        assert_eq!(
            ClientMessage::decode(line.trim_end_matches('\n')),
            Ok(message)
        );
    }

    fn round_trip_bridge(message: BridgeMessage) {
        let line = message.encode();
        assert_eq!(line.matches('\n').count(), 1);
        assert_eq!(
            BridgeMessage::decode(line.trim_end_matches('\n')),
            Ok(message)
        );
    }

    #[test]
    fn client_messages_round_trip() {
        round_trip_client(ClientMessage::Begin {
            kind: SessionKind::Gyro,
            node: "/dev/hidraw3".into(),
        });
        round_trip_client(ClientMessage::Begin {
            kind: SessionKind::Game,
            node: "/dev/hidraw10".into(),
        });
        round_trip_client(ClientMessage::MeasureGyro);
        round_trip_client(ClientMessage::MeasureGame { sensitivity: 2.25 });
        round_trip_client(ClientMessage::SaveGyro { measurement: 7 });
        round_trip_client(ClientMessage::SaveGame {
            measurement: 3,
            turns: 4,
            profile: "/home/user/My Profiles/game one.toml".into(),
        });
        round_trip_client(ClientMessage::Cancel);
        round_trip_client(ClientMessage::End);
    }

    #[test]
    fn bridge_messages_round_trip() {
        round_trip_bridge(BridgeMessage::Started { session: 42 });
        round_trip_bridge(BridgeMessage::Busy);
        round_trip_bridge(BridgeMessage::Rejected("no measurement".into()));
        for progress in [
            Progress::Ready,
            Progress::Started,
            Progress::CenterStick,
            Progress::NoMotion,
        ] {
            round_trip_bridge(BridgeMessage::Progress(progress));
        }
        round_trip_bridge(BridgeMessage::GyroMeasured {
            measurement: 1,
            controller: "00:11:22:33:44:55".into(),
            bias_dps: [0.25, 0.0, -0.1],
        });
        round_trip_bridge(BridgeMessage::GameMeasured {
            measurement: 2,
            counts: 28_800,
        });
        round_trip_bridge(BridgeMessage::GyroSaved);
        round_trip_bridge(BridgeMessage::GameSaved { calibration: 40.0 });
        round_trip_bridge(BridgeMessage::Failed("Controller disconnected.".into()));
        round_trip_bridge(BridgeMessage::Cancelled);
        round_trip_bridge(BridgeMessage::Closed(CloseReason::Idle));
    }

    #[test]
    fn free_text_stays_on_one_line() {
        assert_eq!(
            BridgeMessage::Failed("first\nsecond".into()).encode(),
            "failed first second\n"
        );
        let long = BridgeMessage::Failed("x".repeat(MAX_LINE_BYTES * 2)).encode();
        assert_eq!(long.len(), MAX_LINE_BYTES);
        assert!(long.ends_with('\n'));
    }

    #[test]
    fn invalid_requests_are_rejected() {
        for line in [
            "",
            "begin",
            "begin mouse /dev/hidraw0",
            "begin gyro relative/node",
            "measure-gyro now",
            "measure-game 0",
            "measure-game -1",
            "measure-game NaN",
            "measure-game inf",
            "save-gyro",
            "save-gyro x",
            "save-gyro 1 2",
            "save-game 1 0x2 /p.toml",
            "save-game 1 2",
            "save-game 1 2 relative.toml",
            "cancel please",
            "unknown",
        ] {
            assert!(ClientMessage::decode(line).is_err(), "{line:?}");
        }
        assert!(
            ClientMessage::Begin {
                kind: SessionKind::Gyro,
                node: "relative".into()
            }
            .encode()
            .is_err()
        );
        assert!(
            ClientMessage::MeasureGame {
                sensitivity: f64::NAN
            }
            .encode()
            .is_err()
        );
        for line in ["busy now", "progress later", "closed", "started x", "what"] {
            assert!(BridgeMessage::decode(line).is_err(), "{line:?}");
        }
    }

    #[test]
    fn socket_sits_next_to_the_reload_socket() {
        assert_eq!(
            calibration_socket_path(Path::new("/run/user/1000/joyshockrust/reload.sock")),
            Path::new("/run/user/1000/joyshockrust/reload.sock.calibration")
        );
    }
}
