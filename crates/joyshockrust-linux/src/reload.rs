use joyshockrust_ipc::{
    RELOAD_SOCKET,
    control::{
        InlineProfile, LOAD_PROFILE_PATH_PREFIX, MAX_PROFILE_BYTES, MAX_PROFILE_NAME_BYTES, Reply,
        SELECT_CONTROLLER_PREFIX, SET_EMULATION_MASTER_PREFIX, SET_EMULATION_PREFIX,
        SET_GAMEPAD_PREFIX, SET_GYRO_MOUSE_PREFIX, SET_OUTPUTS_PREFIX, TRIGGER_PREVIEW_PREFIX,
        TRIGGER_TRAVEL_PREFIX, UNLOAD_PROFILE_REQUEST, load_socket_path, profile_path_payload,
    },
};

use std::{
    cell::Cell,
    fmt::Display,
    fs::OpenOptions,
    io::{self, Read, Write},
    os::{
        fd::{AsFd, BorrowedFd},
        linux::net::SocketAddrExt,
        unix::{
            fs::OpenOptionsExt,
            net::{SocketAddr, UnixDatagram, UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    process,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
    sys::{
        signal::{SigSet, Signal},
        signalfd::{SfdFlags, SignalFd},
    },
};

use crate::{
    adaptive_trigger::Preview,
    discovery::{ControllerFilter, ModelSelection, Selection},
    hidraw::ControllerId,
    profile::{self, BridgeOptions, RuntimeOptions},
    status::{self, ProfileSource, StatusSocket},
    trigger_travel,
};

pub struct Reload {
    path: Option<PathBuf>,
    selection_path: Option<PathBuf>,
    bridge_at_startup: BridgeOptions,
    allow_controller_switch: bool,
    signal: SignalFd,
    listener: Option<UnixListener>,
    load_socket: Option<UnixDatagram>,
    outcome: Cell<Outcome>,
}

#[derive(Clone, Copy)]
enum Outcome {
    NotAttempted,
    Applied,
    Rejected,
}

impl Reload {
    pub fn new(
        path: Option<PathBuf>,
        socket: Option<&Path>,
        bridge_at_startup: BridgeOptions,
        allow_controller_switch: bool,
        selection_path: Option<PathBuf>,
    ) -> io::Result<Self> {
        let listener = socket
            .map(|path| {
                let listener = UnixListener::bind(path).map_err(|err| {
                    io::Error::new(
                        err.kind(),
                        format!("could not bind {}: {err}", path.display()),
                    )
                })?;
                listener.set_nonblocking(true)?;
                Ok::<_, io::Error>(listener)
            })
            .transpose()?;
        let load_socket = socket
            .map(|path| {
                let path = load_socket_path(path);
                let address = match SocketAddr::from_pathname(&path) {
                    Ok(address) => address,
                    Err(err) => {
                        eprintln!(
                            "CLI profile loading unavailable at {}: {err}",
                            path.display()
                        );
                        return Ok(None);
                    }
                };
                let socket = UnixDatagram::bind_addr(&address).map_err(|err| {
                    io::Error::new(
                        err.kind(),
                        format!("could not bind {}: {err}", path.display()),
                    )
                })?;
                socket.set_nonblocking(true)?;
                Ok::<_, io::Error>(Some(socket))
            })
            .transpose()?
            .flatten();
        let mut signals = SigSet::empty();
        signals.add(Signal::SIGHUP);
        signals.thread_block()?;
        let signal =
            SignalFd::with_flags(&signals, SfdFlags::SFD_CLOEXEC | SfdFlags::SFD_NONBLOCK)?;
        Ok(Self {
            path,
            selection_path,
            bridge_at_startup,
            allow_controller_switch,
            signal,
            listener,
            load_socket,
            outcome: Cell::new(Outcome::NotAttempted),
        })
    }

    fn outcome(&self) -> &'static str {
        match self.outcome.get() {
            Outcome::NotAttempted if self.path.is_none() => "unavailable",
            Outcome::NotAttempted => "not_attempted",
            Outcome::Applied => "applied",
            Outcome::Rejected => "rejected",
        }
    }
}

pub struct Request<'a> {
    reload: &'a Reload,
    client: Option<Client>,
}

enum Client {
    Reload(UnixStream),
    Load(SocketAddr, Vec<u8>),
    TriggerPreview(Preview),
    TriggerTravel(Option<trigger_travel::Settings>),
}

pub enum Update {
    Profile(Box<RuntimeOptions>),
    Selection(ControllerFilter),
    Emulation(OutputTarget, bool),
    OutputsPaused(bool),
    TriggerPreview(Preview),
    TriggerTravel(Option<trigger_travel::Settings>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputTarget {
    Gamepad,
    GyroMouse,
    Both,
    Master,
}

impl OutputTarget {
    pub fn from_flag(flag: &str) -> Option<Self> {
        match flag {
            "--set-gamepad" => Some(Self::Gamepad),
            "--set-gyro-mouse" => Some(Self::GyroMouse),
            "--set-emulation" => Some(Self::Both),
            "--set-emulation-master" => Some(Self::Master),
            _ => None,
        }
    }

    fn prefix(self) -> &'static [u8] {
        match self {
            Self::Gamepad => SET_GAMEPAD_PREFIX,
            Self::GyroMouse => SET_GYRO_MOUSE_PREFIX,
            Self::Both => SET_EMULATION_PREFIX,
            Self::Master => SET_EMULATION_MASTER_PREFIX,
        }
    }

    pub fn settings(
        self,
        enabled: bool,
        gamepad: bool,
        gyro: Option<bool>,
    ) -> (bool, Option<bool>) {
        match self {
            Self::Gamepad => (enabled, gyro),
            Self::GyroMouse => (gamepad, Some(enabled)),
            Self::Both => (enabled, Some(enabled)),
            Self::Master => (gamepad, gyro),
        }
    }
}

impl Request<'_> {
    fn is_unload(&self) -> bool {
        matches!(&self.client, Some(Client::Load(_, data)) if data == UNLOAD_PROFILE_REQUEST)
    }

    fn selection_payload(&self) -> Option<&[u8]> {
        self.command_payload(SELECT_CONTROLLER_PREFIX)
    }

    fn command_payload(&self, prefix: &[u8]) -> Option<&[u8]> {
        match &self.client {
            Some(Client::Load(_, data)) => data.strip_prefix(prefix),
            _ => None,
        }
    }

    fn emulation_payload(&self) -> Option<(OutputTarget, &[u8])> {
        [
            OutputTarget::Gamepad,
            OutputTarget::GyroMouse,
            OutputTarget::Both,
            OutputTarget::Master,
        ]
        .into_iter()
        .find_map(|target| {
            self.command_payload(target.prefix())
                .map(|payload| (target, payload))
        })
    }

    fn inline_profile(&self) -> Result<Option<InlineProfile<'_>>, &'static str> {
        match &self.client {
            Some(Client::Load(_, data)) => InlineProfile::decode(data).map(Some),
            _ => Ok(None),
        }
    }

    pub fn resolve(&self, current: ControllerFilter) -> Option<Update> {
        if let Some(Client::TriggerPreview(preview)) = &self.client {
            return Some(Update::TriggerPreview(*preview));
        }
        if let Some(Client::TriggerTravel(settings)) = &self.client {
            return Some(Update::TriggerTravel(*settings));
        }
        if let Some(payload) = self.command_payload(SET_OUTPUTS_PREFIX) {
            if !self.reload.allow_controller_switch {
                self.rejected("pausing outputs requires automatic discovery");
                return None;
            }
            return match payload {
                b"pause" => Some(Update::OutputsPaused(true)),
                b"resume" => Some(Update::OutputsPaused(false)),
                _ => {
                    self.rejected("expected pause or resume");
                    None
                }
            };
        }
        if let Some((target, payload)) = self.emulation_payload() {
            return match payload {
                b"on" => Some(Update::Emulation(target, true)),
                b"off" => Some(Update::Emulation(target, false)),
                _ => {
                    self.rejected("expected on or off");
                    None
                }
            };
        }
        if self.is_unload() {
            return Some(Update::Profile(Box::default()));
        }
        if let Some(payload) = self.selection_payload() {
            if !self.reload.allow_controller_switch {
                self.rejected("controller selection changes require automatic discovery");
                return None;
            }
            return match resolve_selection(payload, current) {
                Ok(filter) => Some(Update::Selection(filter)),
                Err(error) => {
                    self.rejected(error);
                    None
                }
            };
        }
        self.load()
            .map(|profile| Update::Profile(Box::new(profile)))
    }

    fn load(&self) -> Option<RuntimeOptions> {
        let inline = match self.inline_profile() {
            Ok(inline) => inline,
            Err(error) => {
                self.rejected(error);
                return None;
            }
        };
        let profile = if let Some(InlineProfile { contents: data, .. }) = inline {
            if data.len() > MAX_PROFILE_BYTES {
                self.rejected("profile exceeds 64 KiB");
                return None;
            }
            std::str::from_utf8(data)
                .map_err(|err| err.into())
                .and_then(profile::parse)
        } else if let Some(path) = &self.reload.path {
            profile::load(path)
        } else {
            self.reply(Reply::Unavailable.bytes());
            return None;
        };
        match profile {
            Ok(profile) => {
                let next = profile.bridge.unwrap_or_default();
                let startup = self.reload.bridge_at_startup;
                if next.rumble.is_some_and(|value| value != startup.rumble)
                    || next
                        .virtual_gamepad
                        .is_some_and(|value| value != startup.virtual_gamepad)
                {
                    self.rejected(
                        "profile changes to rumble or gamepad enablement require restarting JoyShockRust",
                    );
                    return None;
                }
                Some(profile)
            }
            Err(err) => {
                self.rejected(err);
                None
            }
        }
    }

    pub fn profile_gamepad(&self, profile: &RuntimeOptions) -> crate::gamepad::OutputKind {
        profile
            .bridge
            .and_then(|settings| settings.gamepad)
            .unwrap_or(self.reload.bridge_at_startup.gamepad)
    }

    pub fn applied(self, status: Option<&StatusSocket>) {
        if self.command_payload(SET_OUTPUTS_PREFIX).is_some() {
            self.reply(Reply::Applied.bytes());
            return;
        }
        if self.emulation_payload().is_some() {
            eprintln!("Emulation setting updated from CLI");
            self.reply(Reply::Applied.bytes());
            return;
        }
        if self.selection_payload().is_some() {
            eprintln!("Controller selection updated from CLI");
            self.reply(Reply::Applied.bytes());
            return;
        }
        self.reload.outcome.set(Outcome::Applied);
        let (source, name, path) = if self.is_unload() {
            eprintln!("Profile unloaded from CLI");
            (ProfileSource::Defaults, None, None)
        } else if matches!(self.client, Some(Client::Load(_, _))) {
            eprintln!("Profile loaded from CLI");
            let profile = self.inline_profile().ok().flatten();
            let name = profile.as_ref().and_then(|profile| profile.name);
            let path = profile.and_then(|profile| profile.path);
            (ProfileSource::Loaded, name, path)
        } else {
            let path = self.reload.path.as_deref();
            if let Some(path) = path {
                eprintln!("Profile reloaded: {}", path.display());
            }
            (ProfileSource::Startup, path.and_then(Path::file_name), path)
        };
        if let Some(selection) = self.reload.selection_path.as_deref() {
            let selected = matches!(source, ProfileSource::Loaded)
                .then_some(path)
                .flatten();
            if let Err(error) = profile::remember_selection(
                selection,
                selected,
                matches!(source, ProfileSource::Defaults),
            ) {
                eprintln!(
                    "Could not remember active profile {}: {error}",
                    selection.display()
                );
            }
        }
        if let Some(status) = status {
            status.set_profile(source, name, path);
        }
        self.reply(Reply::Applied.bytes());
    }
    pub fn rejected(&self, err: impl Display) {
        if self.emulation_payload().is_some() {
            eprintln!("Emulation setting rejected: {err}");
            self.reply(Reply::Rejected.bytes());
            return;
        }
        if self.selection_payload().is_some() {
            eprintln!("Controller selection rejected: {err}");
            self.reply(Reply::Rejected.bytes());
            return;
        }
        self.reload.outcome.set(Outcome::Rejected);
        if matches!(self.client, Some(Client::Load(_, _))) {
            eprintln!("CLI profile rejected: {err}");
        } else if let Some(path) = &self.reload.path {
            eprintln!("Profile reload rejected ({}): {err}", path.display());
        }
        self.reply(Reply::Rejected.bytes());
    }

    fn reply(&self, response: &[u8]) {
        match &self.client {
            Some(Client::Reload(stream)) => {
                let mut stream = stream;
                let _ = stream.write_all(response);
            }
            Some(Client::Load(address, _)) => {
                if let Some(socket) = &self.reload.load_socket {
                    let _ = socket.send_to_addr(response, address);
                }
            }
            Some(Client::TriggerPreview(_) | Client::TriggerTravel(_)) | None => {}
        }
    }
}

pub fn request(path: Option<&Path>) -> io::Result<()> {
    let stream = status::connect(path, RELOAD_SOCKET)?;
    let mut response = Vec::new();
    stream.take(32).read_to_end(&mut response)?;
    check_reply(&response, "profile reload")
}

fn check_reply(response: &[u8], operation: &str) -> io::Result<()> {
    match Reply::parse(response) {
        Some(Reply::Applied) => Ok(()),
        Some(Reply::Unavailable) => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no startup profile configured; use --load-profile PATH",
        )),
        Some(Reply::Rejected) => Err(io::Error::other(format!(
            "{operation} rejected; see the bridge log for details"
        ))),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{operation} returned no valid result"),
        )),
    }
}

pub fn load_profile(source: &Path, reload_socket: Option<&Path>) -> io::Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(source)
        .map_err(|err| io::Error::new(err.kind(), format!("{}: {err}", source.display())))?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular profile file",
        ));
    }
    let mut profile = Vec::new();
    file.take(MAX_PROFILE_BYTES as u64 + 1)
        .read_to_end(&mut profile)?;
    if profile.len() > MAX_PROFILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "profile exceeds 64 KiB",
        ));
    }

    let payload = profile_path_payload(&std::path::absolute(source)?, &profile)?;
    send_update(&payload, reload_socket, "profile reload")
}

pub fn unload_profile(reload_socket: Option<&Path>) -> io::Result<()> {
    send_update(UNLOAD_PROFILE_REQUEST, reload_socket, "profile unload")
}

pub fn set_emulation(
    target: OutputTarget,
    enabled: bool,
    reload_socket: Option<&Path>,
) -> io::Result<()> {
    let mut payload = target.prefix().to_vec();
    payload.extend_from_slice(if enabled { b"on" } else { b"off" });
    send_update(&payload, reload_socket, "emulation update")
}

pub fn set_outputs_paused(paused: bool, reload_socket: Option<&Path>) -> io::Result<()> {
    let mut payload = SET_OUTPUTS_PREFIX.to_vec();
    payload.extend_from_slice(if paused { b"pause" } else { b"resume" });
    send_update(&payload, reload_socket, "virtual output update")
}

pub fn parse_controller_selection(value: &str) -> Result<Option<ControllerId>, &'static str> {
    if value == "any" {
        Ok(None)
    } else {
        ControllerId::parse(value)
            .map(Some)
            .ok_or("expected a controller HID ID or any")
    }
}

fn resolve_selection(
    payload: &[u8],
    current: ControllerFilter,
) -> Result<ControllerFilter, &'static str> {
    let payload = std::str::from_utf8(payload).map_err(|_| "invalid controller selection")?;
    let mut fields = payload.split('\n');
    let controller_id = parse_controller_selection(fields.next().unwrap_or_default())?;
    let model = fields
        .next()
        .filter(|value| !value.is_empty())
        .map(ModelSelection::parse)
        .transpose()?;
    let transport = fields
        .next()
        .filter(|value| !value.is_empty())
        .map(Selection::parse)
        .transpose()?;
    if fields.next().is_some() {
        return Err("unexpected controller selection fields");
    }
    Ok(ControllerFilter {
        controller_id,
        model: model.unwrap_or(current.model),
        transport: transport.unwrap_or(current.transport),
    })
}

pub fn select_controller(
    id: Option<ControllerId>,
    model: Option<ModelSelection>,
    transport: Option<Selection>,
    reload_socket: Option<&Path>,
) -> io::Result<()> {
    let mut payload = SELECT_CONTROLLER_PREFIX.to_vec();
    if let Some(id) = id {
        write!(payload, "{id}")?;
    } else {
        payload.extend_from_slice(b"any");
    }
    if model.is_some() || transport.is_some() {
        write!(
            payload,
            "\n{}\n{}",
            model.map_or("", ModelSelection::as_str),
            transport.map_or("", Selection::as_str)
        )?;
    }
    send_update(&payload, reload_socket, "controller selection")
}

fn send_update(payload: &[u8], reload_socket: Option<&Path>, operation: &str) -> io::Result<()> {
    let path = load_socket_path(&status::socket_path(reload_socket, RELOAD_SOCKET)?);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = format!("joyshockrust-profile-{}-{nonce}", process::id());
    let socket = UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(id.as_bytes())?)?;
    socket.connect(&path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("could not connect to {}: {err}", path.display()),
        )
    })?;
    let timeout = Some(Duration::from_secs(2));
    socket.set_write_timeout(timeout)?;
    socket.set_read_timeout(timeout)?;
    socket.send(payload)?;
    let mut response = [0; 32];
    let len = socket.recv(&mut response)?;
    check_reply(&response[..len], operation)
}

pub fn wait<'a>(
    fd: BorrowedFd<'_>,
    reload: Option<&'a Reload>,
    status: Option<&StatusSocket>,
    feedback: Option<BorrowedFd<'_>>,
    deadline: Option<Instant>,
) -> io::Result<(PollFlags, Option<Request<'a>>, PollFlags)> {
    let listener = reload.and_then(|reload| reload.listener.as_ref());
    let load_socket = reload.and_then(|reload| reload.load_socket.as_ref());
    let mut descriptors = [
        PollFd::new(fd, PollFlags::POLLIN),
        PollFd::new(
            reload.map_or(fd, |reload| reload.signal.as_fd()),
            reload.map_or(PollFlags::empty(), |_| PollFlags::POLLIN),
        ),
        PollFd::new(status.map_or(fd, StatusSocket::as_fd), PollFlags::POLLIN),
        PollFd::new(listener.map_or(fd, AsFd::as_fd), PollFlags::POLLIN),
        PollFd::new(feedback.unwrap_or(fd), PollFlags::POLLIN),
        PollFd::new(load_socket.map_or(fd, AsFd::as_fd), PollFlags::POLLIN),
    ];
    let len = if load_socket.is_some() {
        6
    } else if feedback.is_some() {
        5
    } else if listener.is_some() {
        4
    } else if status.is_some() {
        3
    } else if reload.is_some() {
        2
    } else {
        1
    };
    loop {
        let timeout = if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok((PollFlags::empty(), None, PollFlags::empty()));
            }
            PollTimeout::try_from(remaining.as_nanos().div_ceil(1_000_000))
                .unwrap_or(PollTimeout::MAX)
        } else {
            PollTimeout::NONE
        };
        match poll(&mut descriptors[..len], timeout) {
            Ok(_) => break,
            Err(Errno::EINTR) => continue,
            Err(err) => return Err(err.into()),
        }
    }
    let mut requested = false;
    if let Some(reload) = reload {
        let flags = descriptors[1].revents().unwrap_or_else(PollFlags::empty);
        if flags.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
            return Err(io::Error::other("profile reload signal descriptor lost"));
        }
        if flags.contains(PollFlags::POLLIN) {
            requested = reload.signal.read_signal()?.is_some();
        }
    }
    if let Some(status) = status {
        let flags = descriptors[2].revents().unwrap_or_else(PollFlags::empty);
        if flags.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
            return Err(io::Error::other("status socket lost"));
        }
        if flags.contains(PollFlags::POLLIN) {
            status.respond(reload.map_or("unavailable", Reload::outcome))?;
        }
    }
    let mut client = None;
    if let Some(listener) = listener {
        let flags = descriptors[3].revents().unwrap_or_else(PollFlags::empty);
        if flags.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
            return Err(io::Error::other("profile reload socket lost"));
        }
        if flags.contains(PollFlags::POLLIN) {
            // One connection per wake leaves report processing a chance to run.
            match listener.accept() {
                Ok((stream, _)) if stream.set_nonblocking(true).is_ok() => {
                    client = Some(Client::Reload(stream))
                }
                Ok(_) => {}
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                            | io::ErrorKind::ConnectionAborted
                    ) => {}
                Err(err) => return Err(err),
            }
        }
    }
    if let Some(socket) = load_socket {
        let flags = descriptors[5].revents().unwrap_or_else(PollFlags::empty);
        if flags.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
            return Err(io::Error::other("profile load socket lost"));
        }
        if !requested && client.is_none() && flags.contains(PollFlags::POLLIN) {
            let mut data = vec![
                0;
                LOAD_PROFILE_PATH_PREFIX.len()
                    + MAX_PROFILE_NAME_BYTES
                    + 1
                    + MAX_PROFILE_BYTES
                    + 1
            ];
            match socket.recv_from(&mut data) {
                Ok((len, address)) if address.is_unnamed() => {
                    if let Some(payload) = data[..len].strip_prefix(TRIGGER_PREVIEW_PREFIX)
                        && let Ok(preview) = Preview::parse(payload)
                    {
                        client = Some(Client::TriggerPreview(preview));
                    } else if let Some(settings) = data[..len]
                        .strip_prefix(TRIGGER_TRAVEL_PREFIX)
                        .and_then(trigger_travel::Settings::parse)
                    {
                        client = Some(Client::TriggerTravel(settings));
                    }
                }
                Ok((len, address)) => {
                    data.truncate(len);
                    client = Some(Client::Load(address, data));
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(err) => return Err(err),
            }
        }
    }
    let request = reload
        .filter(|_| requested || client.is_some())
        .map(|reload| Request { reload, client });
    Ok((
        descriptors[0].revents().unwrap_or_else(PollFlags::empty),
        request,
        feedback
            .and_then(|_| descriptors[4].revents())
            .unwrap_or_else(PollFlags::empty),
    ))
}

#[cfg(test)]
mod tests {
    use std::{
        os::{linux::net::SocketAddrExt, unix::net::SocketAddr},
        process,
        time::Duration,
    };

    use super::*;

    #[test]
    fn reload_rejects_changes_to_bridge_settings() {
        let reload = Reload {
            selection_path: None,
            path: Some(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/gyro-except-l2.toml"),
            ),
            bridge_at_startup: BridgeOptions {
                rumble: true,
                ..BridgeOptions::default()
            },
            allow_controller_switch: false,
            signal: SignalFd::new(&SigSet::empty()).unwrap(),
            listener: None,
            load_socket: None,
            outcome: Cell::new(Outcome::NotAttempted),
        };
        let request = Request {
            reload: &reload,
            client: None,
        };
        let address = SocketAddr::from_abstract_name("joyshockrust-reload-options-test").unwrap();
        assert!(matches!(
            request.resolve(ControllerFilter::default()),
            Some(Update::Profile(_))
        ));

        let request = Request {
            reload: &reload,
            client: Some(Client::Load(
                address.clone(),
                b"version = 1\n[bridge]\nrumble = false\n".to_vec(),
            )),
        };
        assert!(request.load().is_none());
        assert_eq!(reload.outcome(), "rejected");

        let request = Request {
            reload: &reload,
            client: Some(Client::Load(
                address,
                b"version = 1\n[bridge]\nrumble = true\nmodel = 'dualsense-edge'\n".to_vec(),
            )),
        };
        assert!(matches!(
            request.resolve(ControllerFilter::default()),
            Some(Update::Profile(_))
        ));
    }

    #[test]
    fn direct_controller_selection_preserves_the_profile_and_other_filters() {
        let mut reload = Reload {
            selection_path: None,
            path: Some(PathBuf::from("/dev/null")),
            bridge_at_startup: BridgeOptions::default(),
            allow_controller_switch: true,
            signal: SignalFd::new(&SigSet::empty()).unwrap(),
            listener: None,
            load_socket: None,
            outcome: Cell::new(Outcome::NotAttempted),
        };
        let current = ControllerFilter {
            transport: Selection::Bluetooth,
            model: crate::discovery::ModelSelection::DualSenseEdge,
            controller_id: ControllerId::parse("00:11:22:33:44:55"),
        };
        fn request<'a>(server: &'a Reload, payload: &[u8]) -> Request<'a> {
            Request {
                reload: server,
                client: Some(Client::Load(
                    SocketAddr::from_abstract_name("joyshockrust-select-test").unwrap(),
                    [SELECT_CONTROLLER_PREFIX, payload].concat(),
                )),
            }
        }
        let change = request(&reload, b"aa:bb:cc:dd:ee:ff");
        let Some(Update::Selection(filter)) = change.resolve(current) else {
            panic!("expected controller selection");
        };
        assert_eq!(
            filter.controller_id,
            ControllerId::parse("aa:bb:cc:dd:ee:ff")
        );
        assert_eq!(filter.transport, current.transport);
        assert_eq!(filter.model, current.model);
        change.applied(None);
        let Some(Update::Selection(cleared)) = request(&reload, b"any\nany\nauto").resolve(filter)
        else {
            panic!("expected controller selection");
        };
        assert!(cleared.controller_id.is_none());
        assert_eq!(cleared.transport, Selection::Auto);
        assert_eq!(cleared.model, ModelSelection::Any);
        assert!(
            request(&reload, b"any\ninvalid\n")
                .resolve(current)
                .is_none()
        );
        assert!(
            request(&reload, b"any\n\nusb\nextra")
                .resolve(current)
                .is_none()
        );
        assert!(request(&reload, b"invalid").resolve(current).is_none());
        assert_eq!(reload.outcome(), "not_attempted");

        reload.allow_controller_switch = false;
        assert!(request(&reload, b"any").resolve(current).is_none());
    }

    #[test]
    fn live_profile_accepts_omitted_bridge_and_keeps_the_startup_file() {
        let base = format!("joyshockrust-live-profile-{}", process::id());
        let address = SocketAddr::from_abstract_name(format!("{base}-server")).unwrap();
        let load_socket = UnixDatagram::bind_addr(&address).unwrap();
        load_socket.set_nonblocking(true).unwrap();
        let reload = Reload {
            selection_path: None,
            path: Some(PathBuf::from("/dev/null")),
            bridge_at_startup: BridgeOptions {
                rumble: true,
                ..BridgeOptions::default()
            },
            allow_controller_switch: true,
            signal: SignalFd::new(&SigSet::empty()).unwrap(),
            listener: None,
            load_socket: Some(load_socket),
            outcome: Cell::new(Outcome::NotAttempted),
        };
        let client = UnixDatagram::bind_addr(
            &SocketAddr::from_abstract_name(format!("{base}-client")).unwrap(),
        )
        .unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client.connect_addr(&address).unwrap();
        let (_sender, input) = UnixStream::pair().unwrap();

        client
            .send(b"version = 1\n[gyro]\nsensitivity_y = 2\n")
            .unwrap();
        let (_, request, feedback) = wait(input.as_fd(), Some(&reload), None, None, None).unwrap();
        assert!(feedback.is_empty());
        let request = request.unwrap();
        let current = ControllerFilter {
            transport: Selection::Bluetooth,
            model: crate::discovery::ModelSelection::DualSense,
            controller_id: None,
        };
        let Some(Update::Profile(next)) = request.resolve(current) else {
            panic!("expected profile update");
        };
        assert!(next.gyro.is_some());
        request.applied(None);
        let mut reply = [0; 32];
        let len = client.recv(&mut reply).unwrap();
        assert_eq!(&reply[..len], b"applied\n");
        assert_eq!(reload.path.as_deref(), Some(Path::new("/dev/null")));

        client
            .send(b"version = 1\n[bridge]\ntransport = 'usb'\nmodel = 'dualsense-edge'\ncontroller_id = '02:00:00:00:00:01'\nrumble = true\n")
            .unwrap();
        let (_, request, _) = wait(input.as_fd(), Some(&reload), None, None, None).unwrap();
        let request = request.unwrap();
        assert!(matches!(request.resolve(current), Some(Update::Profile(_))));
        request.applied(None);
        let len = client.recv(&mut reply).unwrap();
        assert_eq!(&reply[..len], b"applied\n");
    }

    #[test]
    fn trigger_preview_is_received_without_a_profile_reload() {
        use crate::adaptive_trigger::Effect;

        let address =
            SocketAddr::from_abstract_name(format!("joyshockrust-preview-{}", process::id()))
                .unwrap();
        let load_socket = UnixDatagram::bind_addr(&address).unwrap();
        let reload = Reload {
            path: None,
            selection_path: None,
            bridge_at_startup: BridgeOptions::default(),
            allow_controller_switch: true,
            signal: SignalFd::new(&SigSet::empty()).unwrap(),
            listener: None,
            load_socket: Some(load_socket),
            outcome: Cell::new(Outcome::Applied),
        };
        let client = UnixDatagram::unbound().unwrap();
        let (_sender, input) = UnixStream::pair().unwrap();
        let read = |message: &[u8]| {
            client.send_to_addr(message, &address).unwrap();
            let (_, request, _) = wait(
                input.as_fd(),
                Some(&reload),
                None,
                None,
                Some(Instant::now() + Duration::from_secs(1)),
            )
            .unwrap();
            request.and_then(|request| request.resolve(ControllerFilter::default()))
        };
        assert!(matches!(
            read(b"\0preview-trigger\n\x01\x03\x07"),
            Some(Update::TriggerPreview(Preview::Set {
                side: 1,
                effect: Effect::Resistance {
                    start: 3,
                    strength: 7
                },
            }))
        ));
        assert!(matches!(
            read(b"\0preview-trigger\n\x00\x00\x00"),
            Some(Update::TriggerPreview(Preview::Set {
                side: 0,
                effect: Effect::Off
            }))
        ));
        assert!(matches!(
            read(b"\0preview-trigger\n"),
            Some(Update::TriggerPreview(Preview::Reset))
        ));
        assert!(read(b"\0preview-trigger\n\x00\x0a\x07").is_none());
        assert!(matches!(
            read(b"\0preview-trigger-travel\n\x01\x2d\x23\x5f\x55"),
            Some(Update::TriggerTravel(Some(_)))
        ));
        assert!(matches!(
            read(b"\0preview-trigger-travel\n\x01\x2d\x23\x5f\x55\x00\x64\x3c"),
            Some(Update::TriggerTravel(Some(_)))
        ));
        assert!(matches!(
            read(b"\0preview-trigger-travel\n"),
            Some(Update::TriggerTravel(None))
        ));
        assert!(read(b"\0preview-trigger-travel\n\x02\x2d\x23\x5f\x55").is_none());
        assert!(read(b"version = 1\n").is_none());
        assert_eq!(reload.outcome(), "applied");
    }

    #[test]
    fn reload_socket_replies_with_the_result_for_each_client() {
        let address =
            SocketAddr::from_abstract_name(format!("joyshockrust-reload-{}", process::id()))
                .unwrap();
        let listener = UnixListener::bind_addr(&address).unwrap();
        listener.set_nonblocking(true).unwrap();
        let reload = Reload {
            selection_path: None,
            path: Some(PathBuf::from("/dev/null")),
            bridge_at_startup: BridgeOptions::default(),
            allow_controller_switch: false,
            signal: SignalFd::new(&SigSet::empty()).unwrap(),
            listener: Some(listener),
            load_socket: None,
            outcome: Cell::new(Outcome::NotAttempted),
        };
        let (_sender, input) = UnixStream::pair().unwrap();
        let mut first = UnixStream::connect_addr(&address).unwrap();
        let mut second = UnixStream::connect_addr(&address).unwrap();
        for client in [&first, &second] {
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
        }

        let (ready, request, _) = wait(input.as_fd(), Some(&reload), None, None, None).unwrap();
        assert!(ready.is_empty());
        assert!(request.unwrap().load().is_none());
        let mut response = String::new();
        first.read_to_string(&mut response).unwrap();
        assert_eq!(response, "rejected\n");

        let (_, request, _) = wait(input.as_fd(), Some(&reload), None, None, None).unwrap();
        request.unwrap().applied(None);
        response.clear();
        second.read_to_string(&mut response).unwrap();
        assert_eq!(response, "applied\n");
    }

    #[test]
    fn gamepad_command_replaces_feedback_after_a_ready_descriptor_is_closed() {
        use joyshockrust_input::{DeviceModel, Transport};
        use std::{cell::RefCell, fs::File, os::fd::OwnedFd};

        let address =
            SocketAddr::from_abstract_name(format!("joyshockrust-feedback-{}", process::id()))
                .unwrap();
        let load_socket = UnixDatagram::bind_addr(&address).unwrap();
        load_socket.set_nonblocking(true).unwrap();
        let reload = Reload {
            selection_path: None,
            path: None,
            bridge_at_startup: BridgeOptions::default(),
            allow_controller_switch: false,
            signal: SignalFd::new(&SigSet::empty()).unwrap(),
            listener: None,
            load_socket: Some(load_socket),
            outcome: Cell::new(Outcome::NotAttempted),
        };
        let client = UnixDatagram::bind_addr(
            &SocketAddr::from_abstract_name(format!(
                "joyshockrust-feedback-client-{}",
                process::id()
            ))
            .unwrap(),
        )
        .unwrap();
        client.connect_addr(&address).unwrap();
        client.send(&[SET_GAMEPAD_PREFIX, b"on"].concat()).unwrap();

        let (old_peer, old_feedback) = UnixStream::pair().unwrap();
        let feedback = RefCell::new(Some(OwnedFd::from(old_feedback)));
        drop(old_peer); // The original fd now reports HUP in the same poll as the command.
        let (new_peer, new_feedback) = UnixDatagram::pair().unwrap();
        let mut new_feedback = Some(OwnedFd::from(new_feedback));
        new_peer.send(b"ready").unwrap();
        let (_sender, receiver) = UnixDatagram::pair().unwrap();
        let mut input = File::from(OwnedFd::from(receiver));
        let mut measurement = crate::benchmark::Benchmark::new(2, Duration::from_secs(1));
        let mut observed_feedback = false;
        crate::forward_reports(
            &mut input,
            crate::hidraw::Identity {
                model: DeviceModel::DualSense,
                transport: Transport::Usb,
            },
            Some(&reload),
            None,
            Some(&feedback),
            crate::ReportTimers {
                measurement: Some(&mut measurement),
                ..Default::default()
            },
            |event| match event {
                crate::SessionEvent::Reload(request) => {
                    assert!(matches!(
                        request.resolve(ControllerFilter::default()),
                        Some(Update::Emulation(OutputTarget::Gamepad, true))
                    ));
                    feedback.replace(new_feedback.take());
                    request.applied(None);
                    Ok(std::ops::ControlFlow::Continue(()))
                }
                crate::SessionEvent::Feedback => {
                    assert!(new_feedback.is_none());
                    observed_feedback = true;
                    Ok(std::ops::ControlFlow::Break(()))
                }
                _ => unreachable!(),
            },
        )
        .unwrap();
        assert!(observed_feedback);
        assert_eq!(reload.outcome(), "unavailable");
    }
}
