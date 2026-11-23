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
    discovery::{ControllerFilter, ModelSelection, Selection},
    hidraw::ControllerId,
    profile::{self, BridgeOptions, RuntimeOptions},
    status::{self, StatusSocket},
};

const MAX_PROFILE_BYTES: usize = 64 * 1024;
const SELECT_CONTROLLER_PREFIX: &[u8] = b"\0select-controller\n";

pub struct Reload {
    path: Option<PathBuf>,
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
}

pub enum Update {
    Profile(Box<RuntimeOptions>),
    Selection(ControllerFilter),
}

impl Request<'_> {
    fn selection_payload(&self) -> Option<&[u8]> {
        match &self.client {
            Some(Client::Load(_, data)) => data.strip_prefix(SELECT_CONTROLLER_PREFIX),
            _ => None,
        }
    }

    pub fn resolve(&self, current: ControllerFilter) -> Option<Update> {
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
        let inline = match &self.client {
            Some(Client::Load(_, data)) => Some(data.as_slice()),
            _ => None,
        };
        let profile = if let Some(data) = inline {
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
            self.reply(b"unavailable\n");
            return None;
        };
        match profile {
            Ok(profile) => {
                if profile.bridge.is_none() {
                    return Some(profile);
                }
                let next = profile.bridge.unwrap_or_default();
                let startup = self.reload.bridge_at_startup;
                if next.gamepad != startup.gamepad
                    || next.rumble != startup.rumble
                    || next.virtual_gamepad != startup.virtual_gamepad
                {
                    self.rejected(
                        "virtual gamepad and rumble changes require restarting JoyShockRust",
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

    pub fn applied(self) {
        if self.selection_payload().is_some() {
            eprintln!("Controller selection updated from CLI");
            self.reply(b"applied\n");
            return;
        }
        self.reload.outcome.set(Outcome::Applied);
        if matches!(self.client, Some(Client::Load(_, _))) {
            eprintln!("Profile loaded from CLI");
        } else if let Some(path) = &self.reload.path {
            eprintln!("Profile reloaded: {}", path.display());
        }
        self.reply(b"applied\n");
    }
    pub fn rejected(&self, err: impl Display) {
        if self.selection_payload().is_some() {
            eprintln!("Controller selection rejected: {err}");
            self.reply(b"rejected\n");
            return;
        }
        self.reload.outcome.set(Outcome::Rejected);
        if matches!(self.client, Some(Client::Load(_, _))) {
            eprintln!("CLI profile rejected: {err}");
        } else if let Some(path) = &self.reload.path {
            eprintln!("Profile reload rejected ({}): {err}", path.display());
        }
        self.reply(b"rejected\n");
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
            None => {}
        }
    }
}

pub fn request(path: Option<&Path>) -> io::Result<()> {
    let stream = status::connect(path, "reload.sock")?;
    let mut response = Vec::new();
    stream.take(32).read_to_end(&mut response)?;
    check_reply(&response, "profile reload")
}

fn check_reply(response: &[u8], operation: &str) -> io::Result<()> {
    match response {
        b"applied\n" => Ok(()),
        b"unavailable\n" => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no startup profile configured; use --load-profile PATH",
        )),
        b"rejected\n" => Err(io::Error::other(format!(
            "{operation} rejected; see the bridge log for details"
        ))),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{operation} returned no valid result"),
        )),
    }
}

fn load_socket_path(reload_socket: &Path) -> PathBuf {
    let mut path = reload_socket.as_os_str().to_owned();
    path.push(".load");
    path.into()
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

    send_update(&profile, reload_socket, "profile reload")
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
    let path = load_socket_path(&status::socket_path(reload_socket, "reload.sock")?);
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
            let mut data = vec![0; MAX_PROFILE_BYTES + 1];
            match socket.recv_from(&mut data) {
                Ok((len, address)) if !address.is_unnamed() => {
                    data.truncate(len);
                    client = Some(Client::Load(address, data));
                }
                Ok(_) => {}
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
        change.applied();
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
        request.applied();
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
        request.applied();
        let len = client.recv(&mut reply).unwrap();
        assert_eq!(&reply[..len], b"applied\n");
    }

    #[test]
    fn reload_socket_replies_with_the_result_for_each_client() {
        let address =
            SocketAddr::from_abstract_name(format!("joyshockrust-reload-{}", process::id()))
                .unwrap();
        let listener = UnixListener::bind_addr(&address).unwrap();
        listener.set_nonblocking(true).unwrap();
        let reload = Reload {
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
        request.unwrap().applied();
        response.clear();
        second.read_to_string(&mut response).unwrap();
        assert_eq!(response, "applied\n");
    }
}
