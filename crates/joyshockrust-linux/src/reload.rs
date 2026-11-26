mod profile_work;

use self::profile_work::{
    Completion, PrepareError, PreparedProfile, ProfileInput, ProfileOrigin, Worker,
};
pub use joyshockrust_ipc::control::OutputTarget;

use joyshockrust_ipc::{
    RELOAD_SOCKET,
    control::{
        AddressedCommand, ControlCommand, DecodeError, MAX_DATAGRAM_BYTES, PreviewCommand, Reply,
        load_socket_path,
    },
    unix::ControlClient,
};

use std::{
    cell::Cell,
    fmt::Display,
    io::{self, Read, Write},
    os::{
        fd::{AsFd, BorrowedFd},
        unix::net::{SocketAddr, UnixDatagram, UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::Instant,
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
    profile::{BridgeOptions, RuntimeOptions},
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
    worker: Worker,
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
        let worker = Worker::new(path.clone(), bridge_at_startup)?;
        Ok(Self {
            path,
            selection_path,
            bridge_at_startup,
            allow_controller_switch,
            signal,
            listener,
            load_socket,
            outcome: Cell::new(Outcome::NotAttempted),
            worker,
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
    reply: Option<ReplyTo>,
    action: Action,
}

enum ReplyTo {
    Reload(UnixStream),
    Datagram(SocketAddr),
}

enum Action {
    Profile {
        options: Cell<Option<Box<RuntimeOptions>>>,
        origin: ProfileOrigin,
    },
    Selection {
        controller_id: Option<ControllerId>,
        model: Option<ModelSelection>,
        transport: Option<Selection>,
    },
    Emulation(OutputTarget, bool),
    OutputsPaused(bool),
    Preview(PreviewCommand),
}

pub enum Update {
    Profile(Box<RuntimeOptions>),
    Selection(ControllerFilter),
    Emulation(OutputTarget, bool),
    OutputsPaused(bool),
    TriggerPreview(Preview),
    TriggerTravel(Option<trigger_travel::Settings>),
}

impl Request<'_> {
    pub fn resolve(&self, current: ControllerFilter) -> Option<Update> {
        match &self.action {
            Action::Profile { options, .. } => options.take().map(Update::Profile),
            Action::Selection {
                controller_id,
                model,
                transport,
            } => Some(Update::Selection(ControllerFilter {
                controller_id: *controller_id,
                model: model.unwrap_or(current.model),
                transport: transport.unwrap_or(current.transport),
            })),
            Action::Emulation(target, enabled) => Some(Update::Emulation(*target, *enabled)),
            Action::OutputsPaused(paused) => Some(Update::OutputsPaused(*paused)),
            Action::Preview(PreviewCommand::Resistance(preview)) => {
                Some(Update::TriggerPreview((*preview).into()))
            }
            Action::Preview(PreviewCommand::Travel(preview)) => {
                Some(Update::TriggerTravel(preview.map(Into::into)))
            }
        }
    }

    pub fn profile_gamepad(&self, profile: &RuntimeOptions) -> crate::gamepad::OutputKind {
        profile
            .bridge
            .and_then(|settings| settings.gamepad)
            .unwrap_or(self.reload.bridge_at_startup.gamepad)
    }

    pub fn applied(self, status: Option<&StatusSocket>) -> io::Result<()> {
        let origin = match self.action {
            Action::Profile { origin, .. } => origin,
            action => {
                match action {
                    Action::Emulation(..) => eprintln!("Emulation setting updated from CLI"),
                    Action::Selection { .. } => eprintln!("Controller selection updated from CLI"),
                    _ => {}
                }
                self.reload.respond(self.reply.as_ref(), Reply::Applied);
                return Ok(());
            }
        };

        let (source, name, path) = match &origin {
            ProfileOrigin::Defaults => {
                eprintln!("Profile unloaded from CLI");
                (ProfileSource::Defaults, None, None)
            }
            ProfileOrigin::Inline { name, path } => {
                eprintln!("Profile loaded from CLI");
                (ProfileSource::Loaded, name.as_deref(), path.as_deref())
            }
            ProfileOrigin::Startup => {
                let path = self.reload.path.as_deref();
                if let Some(path) = path {
                    eprintln!("Profile reloaded: {}", path.display());
                }
                (ProfileSource::Startup, path.and_then(Path::file_name), path)
            }
        };
        if let Some(status) = status {
            status.set_profile(source, name, path);
        }
        if let Some(selection) = &self.reload.selection_path {
            let selected = match origin {
                ProfileOrigin::Inline { path, .. } => path,
                _ => None,
            };
            self.reload.worker.remember(
                self.reply,
                selection.clone(),
                selected,
                matches!(source, ProfileSource::Defaults),
            )
        } else {
            self.reload.confirm_profile(self.reply.as_ref());
            Ok(())
        }
    }

    pub fn rejected(&self, err: impl Display) {
        match self.action {
            Action::Profile { .. } => {
                self.reload.reject_profile(self.reply.as_ref(), err);
                self.reload.worker.finish();
                return;
            }
            Action::Emulation(..) => eprintln!("Emulation setting rejected: {err}"),
            Action::Selection { .. } => eprintln!("Controller selection rejected: {err}"),
            Action::OutputsPaused(_) => eprintln!("Virtual output update rejected: {err}"),
            Action::Preview(_) => eprintln!("Preview rejected: {err}"),
        }
        self.reload.respond(self.reply.as_ref(), Reply::Rejected);
    }
}

impl Reload {
    fn respond(&self, reply: Option<&ReplyTo>, response: Reply) {
        match reply {
            Some(ReplyTo::Reload(stream)) => {
                let mut stream = stream;
                let _ = stream.write_all(response.bytes());
            }
            Some(ReplyTo::Datagram(address)) => {
                if let Some(socket) = &self.load_socket {
                    let _ = socket.send_to_addr(response.bytes(), address);
                }
            }
            None => {}
        }
    }

    fn confirm_profile(&self, reply: Option<&ReplyTo>) {
        self.outcome.set(Outcome::Applied);
        self.respond(reply, Reply::Applied);
        self.worker.finish();
    }

    fn reject_profile(&self, reply: Option<&ReplyTo>, err: impl Display) {
        self.outcome.set(Outcome::Rejected);
        if matches!(reply, Some(ReplyTo::Datagram(_))) {
            eprintln!("CLI profile rejected: {err}");
        } else if let Some(path) = &self.path {
            eprintln!("Profile reload rejected ({}): {err}", path.display());
        }
        self.respond(reply, Reply::Rejected);
    }

    fn queue_profile(&self, input: ProfileInput, mut reply: Option<ReplyTo>) -> io::Result<()> {
        if !self.worker.try_queue(input, &mut reply) {
            eprintln!("Profile request rejected: the profile worker queue is full");
            self.respond(reply.as_ref(), Reply::Rejected);
        }
        self.worker.start_next()
    }

    fn selection_action(
        &self,
        id: &str,
        model: Option<&str>,
        transport: Option<&str>,
    ) -> Result<Action, &'static str> {
        if !self.allow_controller_switch {
            return Err("controller selection changes require automatic discovery");
        }
        Ok(Action::Selection {
            controller_id: parse_controller_selection(id)?,
            model: model.map(ModelSelection::parse).transpose()?,
            transport: transport.map(Selection::parse).transpose()?,
        })
    }

    fn addressed_request(
        &self,
        address: SocketAddr,
        data: &[u8],
    ) -> io::Result<Option<Request<'_>>> {
        let reply = Some(ReplyTo::Datagram(address));
        let command = match AddressedCommand::decode(data) {
            Ok(AddressedCommand::LoadProfile(profile)) => {
                self.queue_profile(
                    ProfileInput::Inline {
                        name: profile.name.map(ToOwned::to_owned),
                        path: profile.path.map(Path::to_path_buf),
                        contents: profile.contents.to_vec(),
                    },
                    reply,
                )?;
                return Ok(None);
            }
            Ok(AddressedCommand::Control(command)) => command,
            Err(DecodeError::Profile(error)) => {
                self.queue_profile(ProfileInput::Invalid(error), reply)?;
                return Ok(None);
            }
            Err(DecodeError::Command(error)) => {
                eprintln!("Control command rejected: {error}");
                self.respond(reply.as_ref(), Reply::Rejected);
                return Ok(None);
            }
        };
        let action = match command {
            ControlCommand::UnloadProfile => {
                self.queue_profile(ProfileInput::Defaults, reply)?;
                return Ok(None);
            }
            ControlCommand::Emulation { target, enabled } => Ok(Action::Emulation(target, enabled)),
            ControlCommand::OutputsPaused(paused) if self.allow_controller_switch => {
                Ok(Action::OutputsPaused(paused))
            }
            ControlCommand::OutputsPaused(_) => Err("pausing outputs requires automatic discovery"),
            ControlCommand::SelectController {
                id,
                model,
                transport,
            } => self.selection_action(id, model, transport),
        };
        match action {
            Ok(action) => Ok(Some(Request {
                reload: self,
                reply,
                action,
            })),
            Err(error) => {
                eprintln!("{} rejected: {error}", command.operation());
                self.respond(reply.as_ref(), Reply::Rejected);
                Ok(None)
            }
        }
    }
}

pub fn request(path: Option<&Path>) -> io::Result<()> {
    let stream = status::connect(path, RELOAD_SOCKET)?;
    let mut response = Vec::new();
    stream.take(32).read_to_end(&mut response)?;
    Reply::check(&response, "profile reload")
}

pub fn load_profile(source: &Path, reload_socket: Option<&Path>) -> io::Result<()> {
    control_client(reload_socket)?.load_profile(source)
}

pub fn unload_profile(reload_socket: Option<&Path>) -> io::Result<()> {
    control_client(reload_socket)?.send(ControlCommand::UnloadProfile)
}

pub fn set_emulation(
    target: OutputTarget,
    enabled: bool,
    reload_socket: Option<&Path>,
) -> io::Result<()> {
    control_client(reload_socket)?.send(ControlCommand::Emulation { target, enabled })
}

pub fn set_outputs_paused(paused: bool, reload_socket: Option<&Path>) -> io::Result<()> {
    control_client(reload_socket)?.send(ControlCommand::OutputsPaused(paused))
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

pub fn select_controller(
    id: Option<ControllerId>,
    model: Option<ModelSelection>,
    transport: Option<Selection>,
    reload_socket: Option<&Path>,
) -> io::Result<()> {
    let id = id.map(|id| id.to_string());
    control_client(reload_socket)?.send(ControlCommand::SelectController {
        id: id.as_deref().unwrap_or("any"),
        model: model.map(ModelSelection::as_str),
        transport: transport.map(Selection::as_str),
    })
}

fn control_client(reload_socket: Option<&Path>) -> io::Result<ControlClient> {
    status::socket_path(reload_socket, RELOAD_SOCKET).map(ControlClient::new)
}

pub fn wait<'a>(
    fd: BorrowedFd<'_>,
    reload: Option<&'a Reload>,
    status: Option<&StatusSocket>,
    feedback: Option<BorrowedFd<'_>>,
    deadline: Option<Instant>,
) -> io::Result<(PollFlags, Option<Request<'a>>, PollFlags)> {
    if let Some(reload) = reload {
        reload.worker.start_next()?;
    }
    let updates_fd = status.and_then(StatusSocket::updates_fd);
    let outcome = reload.map_or("unavailable", Reload::outcome);
    let publish_at = status.and_then(|status| status.publish_updates(false, outcome));
    let deadline = deadline.into_iter().chain(publish_at).min();
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
        PollFd::new(updates_fd.unwrap_or(fd), PollFlags::POLLIN),
        PollFd::new(
            reload.map_or(fd, |reload| reload.worker.wake.as_fd()),
            PollFlags::POLLIN,
        ),
    ];
    let len = if reload.is_some() {
        8
    } else if updates_fd.is_some() {
        7
    } else if feedback.is_some() {
        5
    } else if status.is_some() {
        3
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
    if let Some(status) = status {
        let flags = descriptors[2].revents().unwrap_or_else(PollFlags::empty);
        if flags.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
            return Err(io::Error::other("status socket lost"));
        }
        if flags.contains(PollFlags::POLLIN) {
            status.respond(outcome)?;
        }
        if updates_fd.is_some()
            && descriptors[6]
                .revents()
                .unwrap_or_else(PollFlags::empty)
                .contains(PollFlags::POLLIN)
        {
            status.publish_updates(true, outcome);
        }
    }
    let ready = descriptors[0].revents().unwrap_or_else(PollFlags::empty);
    let feedback_ready = feedback
        .and_then(|_| descriptors[4].revents())
        .unwrap_or_else(PollFlags::empty);
    if let Some(reload) = reload {
        let flags = descriptors[7].revents().unwrap_or_else(PollFlags::empty);
        if flags.contains(PollFlags::POLLIN) {
            match reload.worker.receive()? {
                Completion::Prepared(reply, Ok(PreparedProfile { options, origin })) => {
                    let request = Request {
                        reload,
                        reply,
                        action: Action::Profile {
                            options: Cell::new(Some(options)),
                            origin,
                        },
                    };
                    return Ok((ready, Some(request), feedback_ready));
                }
                Completion::Prepared(reply, Err(error)) => {
                    match error {
                        PrepareError::Unavailable => {
                            reload.respond(reply.as_ref(), Reply::Unavailable)
                        }
                        PrepareError::Rejected(err) => reload.reject_profile(reply.as_ref(), err),
                    }
                    reload.worker.finish();
                }
                Completion::Remembered(reply) => reload.confirm_profile(reply.as_ref()),
            }
        } else if flags.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
            return Err(io::Error::other("profile worker descriptor lost"));
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
    let mut reply = None;
    if let Some(listener) = listener {
        let flags = descriptors[3].revents().unwrap_or_else(PollFlags::empty);
        if flags.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
            return Err(io::Error::other("profile reload socket lost"));
        }
        if flags.contains(PollFlags::POLLIN) {
            // One connection per wake leaves report processing a chance to run.
            match listener.accept() {
                Ok((stream, _)) if stream.set_nonblocking(true).is_ok() => {
                    reply = Some(ReplyTo::Reload(stream))
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
    let Some(reload) = reload else {
        return Ok((ready, None, feedback_ready));
    };
    if requested || reply.is_some() {
        reload.queue_profile(ProfileInput::Startup, reply)?;
        return Ok((ready, None, feedback_ready));
    }

    let mut request = None;
    if let Some(socket) = load_socket {
        let flags = descriptors[5].revents().unwrap_or_else(PollFlags::empty);
        if flags.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
            return Err(io::Error::other("profile load socket lost"));
        }
        if flags.contains(PollFlags::POLLIN) {
            // The extra byte distinguishes the largest supported message from an oversized one.
            let mut data = vec![0; MAX_DATAGRAM_BYTES + 1];
            match socket.recv_from(&mut data) {
                Ok((len, address)) if address.is_unnamed() => {
                    if let Ok(preview) = PreviewCommand::decode(&data[..len]) {
                        request = Some(Request {
                            reload,
                            reply: None,
                            action: Action::Preview(preview),
                        });
                    }
                }
                Ok((len, address)) => request = reload.addressed_request(address, &data[..len])?,
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(err) => return Err(err),
            }
        }
    }
    Ok((ready, request, feedback_ready))
}

#[cfg(test)]
mod tests {
    use std::{
        os::{linux::net::SocketAddrExt, unix::net::SocketAddr},
        time::Duration,
    };

    use super::*;
    use std::process;

    fn wait_for_request<'a>(input: &UnixStream, reload: &'a Reload) -> Request<'a> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let (_, request, _) =
                wait(input.as_fd(), Some(reload), None, None, Some(deadline)).unwrap();
            if let Some(request) = request {
                return request;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for a prepared profile"
            );
        }
    }

    #[test]
    fn reload_rejects_changes_to_bridge_settings() {
        let startup =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/gyro-except-l2.toml");
        let bridge = BridgeOptions {
            rumble: true,
            ..BridgeOptions::default()
        };
        assert!(profile_work::prepare(ProfileInput::Startup, Some(&startup), bridge).is_ok());

        let inline = |data: &[u8]| ProfileInput::Inline {
            name: None,
            path: None,
            contents: data.to_vec(),
        };
        assert!(matches!(
            profile_work::prepare(
                inline(b"version = 1\n[bridge]\nrumble = false\n"),
                Some(&startup),
                bridge,
            ),
            Err(PrepareError::Rejected(reason)) if reason.contains("require restarting")
        ));
        assert!(
            profile_work::prepare(
                inline(b"version = 1\n[bridge]\nrumble = true\nmodel = 'dualsense-edge'\n"),
                Some(&startup),
                bridge,
            )
            .is_ok()
        );
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
            worker: Worker::new(None, BridgeOptions::default()).unwrap(),
        };
        let current = ControllerFilter {
            transport: Selection::Bluetooth,
            model: crate::discovery::ModelSelection::DualSenseEdge,
            controller_id: ControllerId::parse("00:11:22:33:44:55"),
        };
        fn request<'a>(server: &'a Reload, payload: &[u8]) -> Option<Request<'a>> {
            let address = SocketAddr::from_abstract_name("joyshockrust-select-test").unwrap();
            server
                .addressed_request(address, &[b"\0select-controller\n", payload].concat())
                .unwrap()
        }
        let change = request(&reload, b"aa:bb:cc:dd:ee:ff").unwrap();
        let Some(Update::Selection(filter)) = change.resolve(current) else {
            panic!("expected controller selection");
        };
        assert_eq!(
            filter.controller_id,
            ControllerId::parse("aa:bb:cc:dd:ee:ff")
        );
        assert_eq!(filter.transport, current.transport);
        assert_eq!(filter.model, current.model);
        change.applied(None).unwrap();
        let Some(Update::Selection(cleared)) =
            request(&reload, b"any\nany\nauto").unwrap().resolve(filter)
        else {
            panic!("expected controller selection");
        };
        assert!(cleared.controller_id.is_none());
        assert_eq!(cleared.transport, Selection::Auto);
        assert_eq!(cleared.model, ModelSelection::Any);
        assert!(request(&reload, b"any\ninvalid\n").is_none());
        assert!(request(&reload, b"any\n\nusb\nextra").is_none());
        assert!(request(&reload, b"invalid").is_none());
        assert_eq!(reload.outcome(), "not_attempted");

        reload.allow_controller_switch = false;
        assert!(request(&reload, b"any").is_none());
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
            worker: Worker::new(
                None,
                BridgeOptions {
                    rumble: true,
                    ..BridgeOptions::default()
                },
            )
            .unwrap(),
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
        let request = wait_for_request(&input, &reload);
        let current = ControllerFilter {
            transport: Selection::Bluetooth,
            model: crate::discovery::ModelSelection::DualSense,
            controller_id: None,
        };
        let Some(Update::Profile(next)) = request.resolve(current) else {
            panic!("expected profile update");
        };
        assert!(next.gyro.is_some());
        request.applied(None).unwrap();
        let mut reply = [0; 32];
        let len = client.recv(&mut reply).unwrap();
        assert_eq!(&reply[..len], b"applied\n");
        assert_eq!(reload.path.as_deref(), Some(Path::new("/dev/null")));

        client
            .send(b"version = 1\n[bridge]\ntransport = 'usb'\nmodel = 'dualsense-edge'\ncontroller_id = '02:00:00:00:00:01'\nrumble = true\n")
            .unwrap();
        let request = wait_for_request(&input, &reload);
        assert!(matches!(request.resolve(current), Some(Update::Profile(_))));
        request.applied(None).unwrap();
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
            worker: Worker::new(None, BridgeOptions::default()).unwrap(),
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
        assert!(read(b"\0set-gamepad\noff").is_none());
        assert_eq!(reload.outcome(), "applied");
    }

    #[test]
    fn reload_socket_replies_with_the_result_for_each_client() {
        let address =
            SocketAddr::from_abstract_name(format!("joyshockrust-reload-{}", process::id()))
                .unwrap();
        let listener = UnixListener::bind_addr(&address).unwrap();
        listener.set_nonblocking(true).unwrap();
        let startup =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/gyro-except-l2.toml");
        let reload = Reload {
            selection_path: None,
            path: Some(startup.clone()),
            bridge_at_startup: BridgeOptions::default(),
            allow_controller_switch: false,
            signal: SignalFd::new(&SigSet::empty()).unwrap(),
            listener: Some(listener),
            load_socket: None,
            outcome: Cell::new(Outcome::NotAttempted),
            worker: Worker::new(Some(startup), BridgeOptions::default()).unwrap(),
        };
        let (_sender, input) = UnixStream::pair().unwrap();
        let mut first = UnixStream::connect_addr(&address).unwrap();
        let mut second = UnixStream::connect_addr(&address).unwrap();
        for client in [&first, &second] {
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
        }

        for client in [&mut first, &mut second] {
            let request = wait_for_request(&input, &reload);
            assert!(matches!(
                request.resolve(ControllerFilter::default()),
                Some(Update::Profile(_))
            ));
            request.applied(None).unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            assert_eq!(response, "applied\n");
        }
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
            worker: Worker::new(None, BridgeOptions::default()).unwrap(),
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
        client.send(b"\0set-gamepad\non").unwrap();

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
                    request.applied(None).unwrap();
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
