mod calibration;
mod profile_work;

pub use self::calibration::LeaseAction;

use self::profile_work::{
    Completion, PrepareError, PreparedProfile, ProfileInput, ProfileOrigin, Worker,
};
pub use joyshockrust_ipc::control::OutputTarget;

use joyshockrust_ipc::{
    RELOAD_SOCKET,
    calibration::calibration_socket_path,
    control::{
        AddressedCommand, ControlCommand, DecodeError, MAX_DATAGRAM_BYTES, PreviewCommand, Reply,
        load_socket_path,
    },
    unix::ControlClient,
};

use std::{
    cell::{Cell, RefCell},
    fmt::Display,
    io::{self, Read, Write},
    os::{
        fd::{AsFd, BorrowedFd},
        unix::net::{SocketAddr, UnixDatagram, UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::mpsc::SyncSender,
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
    calibration: Option<calibration::Server>,
    lease: Cell<Option<Lease>>,
    /// The file behind the running profile, if it came from one.
    active: RefCell<Option<PathBuf>>,
}

/// Outputs held paused by a calibration session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Lease {
    session: u64,
    /// The pause state to restore when the session ends.
    restore_paused: bool,
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
        active_profile: Option<PathBuf>,
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
        // Sessions pause outputs, which requires automatic discovery.
        let calibration = socket
            .filter(|_| allow_controller_switch)
            .and_then(|socket| match start_calibration(socket) {
                Ok(server) => Some(server),
                Err(err) => {
                    eprintln!("Calibration sessions unavailable: {err}");
                    None
                }
            });
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
            calibration,
            lease: Cell::new(None),
            active: RefCell::new(active_profile),
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
    Lease(SyncSender<bool>),
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
    Lease(LeaseAction),
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
            Action::Lease(LeaseAction::Acquire(session)) => {
                if self.reload.lease.get().is_some() {
                    return None;
                }
                // Discovery corrects this when outputs were already paused.
                self.reload.lease.set(Some(Lease {
                    session: *session,
                    restore_paused: false,
                }));
                Some(Update::OutputsPaused(true))
            }
            Action::Lease(LeaseAction::Release(session)) => self
                .reload
                .lease
                .get()
                .filter(|lease| lease.session == *session)
                .map(|lease| Update::OutputsPaused(lease.restore_paused)),
            Action::Preview(PreviewCommand::Resistance(preview)) => {
                Some(Update::TriggerPreview((*preview).into()))
            }
            Action::Preview(PreviewCommand::Travel(preview)) => {
                Some(Update::TriggerTravel(preview.map(Into::into)))
            }
        }
    }

    /// Record the pause state seen before applying a resolved update.
    pub fn outputs_were_paused(&self, paused: bool) {
        if let Action::Lease(LeaseAction::Acquire(session)) = self.action
            && let Some(lease) = self.reload.lease.get()
            && lease.session == session
        {
            self.reload.lease.set(Some(Lease {
                restore_paused: paused,
                ..lease
            }));
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
                    Action::OutputsPaused(true) => {
                        // Keep a pause requested during a session after it ends.
                        if let Some(lease) = self.reload.lease.get() {
                            self.reload.lease.set(Some(Lease {
                                restore_paused: true,
                                ..lease
                            }));
                        }
                    }
                    Action::Lease(LeaseAction::Release(_)) => self.reload.lease.set(None),
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
        self.reload.active.replace(path.map(Path::to_path_buf));
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
            Action::Lease(action) => {
                eprintln!("Calibration session lease rejected: {err}");
                // A failed acquire holds nothing; a failed release must not
                // leave outputs locked by a session that has ended.
                if let Some(lease) = self.reload.lease.get()
                    && matches!(action, LeaseAction::Acquire(session) | LeaseAction::Release(session) if session == lease.session)
                {
                    self.reload.lease.set(None);
                }
            }
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
            Some(ReplyTo::Lease(sender)) => {
                let _ = sender.try_send(response == Reply::Applied);
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

    /// Reload the running profile after a calibration session wrote to its
    /// file, so the new values apply without a manual reload.
    fn reload_calibrated(&self, saved: &Path) -> io::Result<()> {
        let Some(active) = self.active.borrow().clone() else {
            return Ok(());
        };
        if !same_file(&active, saved) {
            return Ok(());
        }
        eprintln!(
            "Reloading {} after calibration was saved to it",
            active.display()
        );
        let input = if self.path.as_deref() == Some(active.as_path()) {
            ProfileInput::Startup
        } else {
            ProfileInput::File(active)
        };
        self.queue_profile(input, None)
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
            ControlCommand::OutputsPaused(false) if self.lease.get().is_some() => {
                Err("a calibration session owns the controller")
            }
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

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn start_calibration(reload_socket: &Path) -> io::Result<calibration::Server> {
    let path = calibration_socket_path(reload_socket);
    let listener = UnixListener::bind(&path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("could not bind {}: {err}", path.display()),
        )
    })?;
    calibration::Server::start(
        listener,
        calibration::Config {
            program: std::env::current_exe()?,
            timeouts: calibration::Timeouts::default(),
            store: Box::new(calibration::Files),
        },
    )
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
    let preview_fd = status.and_then(StatusSocket::preview_fd);
    let outcome = reload.map_or("unavailable", Reload::outcome);
    let publish_at = status.and_then(|status| status.publish_updates(false, outcome));
    let preview_at = status.and_then(|status| status.publish_preview(false));
    let deadline = deadline
        .into_iter()
        .chain(publish_at)
        .chain(preview_at)
        .min();
    let listener = reload.and_then(|reload| reload.listener.as_ref());
    let calibration = reload.and_then(|reload| reload.calibration.as_ref());
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
        PollFd::new(preview_fd.unwrap_or(fd), PollFlags::POLLIN),
        PollFd::new(
            calibration.map_or(fd, |server| server.wake.as_fd()),
            PollFlags::POLLIN,
        ),
    ];
    let len = if calibration.is_some() {
        10
    } else if preview_fd.is_some() {
        9
    } else if reload.is_some() {
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
        if preview_fd.is_some()
            && descriptors[8]
                .revents()
                .is_some_and(|flags| flags.contains(PollFlags::POLLIN))
        {
            status.publish_preview(true);
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
    if let (Some(reload), Some(server)) = (reload, calibration) {
        let flags = descriptors[9].revents().unwrap_or_else(PollFlags::empty);
        if flags.contains(PollFlags::POLLIN) {
            if let Some(lease) = server.receive()? {
                if let Some(saved) = &lease.saved_profile {
                    reload.reload_calibrated(saved)?;
                }
                let request = Request {
                    reload,
                    reply: Some(ReplyTo::Lease(lease.reply)),
                    action: Action::Lease(lease.action),
                };
                return Ok((ready, Some(request), feedback_ready));
            }
        } else if flags.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
            return Err(io::Error::other("calibration session descriptor lost"));
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
            calibration: None,
            lease: Cell::new(None),
            active: RefCell::new(None),
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
            calibration: None,
            lease: Cell::new(None),
            active: RefCell::new(None),
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
            calibration: None,
            lease: Cell::new(None),
            active: RefCell::new(None),
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
            calibration: None,
            lease: Cell::new(None),
            active: RefCell::new(None),
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
            calibration: None,
            lease: Cell::new(None),
            active: RefCell::new(None),
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

    mod calibration_sessions {
        use std::{
            fs,
            io::{BufRead, BufReader},
            os::unix::fs::PermissionsExt,
            sync::{Arc, Mutex},
            thread,
            time::Duration,
        };

        use joyshockrust_ipc::calibration::{
            BridgeMessage, ClientMessage, CloseReason, Progress, SessionKind,
        };

        use super::super::calibration::{Config, Server, Store, Timeouts};
        use super::*;

        const WORKER: &str = r#"#!/bin/sh
case "$1:$2" in
    --gyro-calibration:--measure)
        case "${3##*/}" in
            gyro) printf 'ready\nbias 00:11:22:33:44:55 0.25 0 -0.1\n' ;;
            unplugged)
                printf 'ready\n'
                printf 'Error: "Controller disconnected before gyro calibration finished."\n' >&2
                exit 1 ;;
            *) exit 2 ;;
        esac ;;
    --calibrate-game:--measure-stick)
        [ "$3" = 2.5 ] || exit 1
        case "${4##*/}" in
            game) printf 'ready\ncenter-stick\nstarted\nno-motion\ncounts 28800\n' ;;
            slow)
                printf 'ready\n'
                echo $$ > "${0%/*}/slow.pid"
                exec sleep 60 ;;
            *) exit 2 ;;
        esac ;;
    *) exit 3 ;;
esac
"#;

        #[derive(Clone, Default)]
        struct Record(Arc<Mutex<Vec<String>>>);

        impl Store for Record {
            fn save_gyro(&self, controller: &str, bias: [f32; 3]) -> Result<(), String> {
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("gyro {controller} {bias:?}"));
                Ok(())
            }

            fn save_game(
                &self,
                profile: &Path,
                counts: i32,
                sensitivity: f64,
                turns: u32,
            ) -> Result<f64, String> {
                if profile.ends_with("fail.toml") {
                    return Err("disk full".into());
                }
                self.0.lock().unwrap().push(format!(
                    "game {} {counts} {sensitivity} {turns}",
                    profile.display()
                ));
                Ok(40.0)
            }
        }

        struct Fixture {
            directory: PathBuf,
            socket: PathBuf,
            reload: Reload,
            record: Record,
            paused: Cell<bool>,
        }

        impl Fixture {
            fn new(name: &str, timeouts: Timeouts) -> Self {
                let directory =
                    std::env::temp_dir().join(format!("jsr-calibration-{name}-{}", process::id()));
                let _ = fs::remove_dir_all(&directory);
                fs::create_dir(&directory).unwrap();
                let program = directory.join("worker");
                fs::write(&program, WORKER).unwrap();
                fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
                let socket = directory.join("calibration");
                let record = Record::default();
                let server = Server::start(
                    UnixListener::bind(&socket).unwrap(),
                    Config {
                        program,
                        timeouts,
                        store: Box::new(record.clone()),
                    },
                )
                .unwrap();
                let reload = Reload {
                    path: None,
                    selection_path: None,
                    bridge_at_startup: BridgeOptions::default(),
                    allow_controller_switch: true,
                    signal: SignalFd::new(&SigSet::empty()).unwrap(),
                    listener: None,
                    load_socket: None,
                    outcome: Cell::new(Outcome::NotAttempted),
                    worker: Worker::new(None, BridgeOptions::default()).unwrap(),
                    calibration: Some(server),
                    lease: Cell::new(None),
                    active: RefCell::new(None),
                };
                Self {
                    directory,
                    socket,
                    reload,
                    record,
                    paused: Cell::new(false),
                }
            }

            /// Serve lease requests like the discovery loop until `client` ends
            /// and no lease remains.
            fn run<T: Send + 'static>(
                &self,
                client: impl FnOnce(PathBuf) -> T + Send + 'static,
            ) -> T {
                let socket = self.socket.clone();
                let client = thread::spawn(move || client(socket));
                let (_sender, idle) = UnixStream::pair().unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                while !client.is_finished() || self.reload.lease.get().is_some() {
                    assert!(Instant::now() < deadline, "calibration session hung");
                    let (_, request, _) = wait(
                        idle.as_fd(),
                        Some(&self.reload),
                        None,
                        None,
                        Some(Instant::now() + Duration::from_millis(20)),
                    )
                    .unwrap();
                    if let Some(request) = request {
                        match request.resolve(ControllerFilter::default()) {
                            Some(Update::OutputsPaused(paused)) => {
                                request.outputs_were_paused(self.paused.get());
                                self.paused.set(paused);
                                request.applied(None).unwrap();
                            }
                            Some(_) => panic!("unexpected update"),
                            None => {}
                        }
                    }
                }
                client.join().unwrap()
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.directory);
            }
        }

        struct Client {
            reader: BufReader<UnixStream>,
            writer: UnixStream,
        }

        impl Client {
            fn connect(socket: &Path) -> Self {
                let writer = UnixStream::connect(socket).unwrap();
                writer
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                Self {
                    reader: BufReader::new(writer.try_clone().unwrap()),
                    writer,
                }
            }

            fn begin(socket: &Path, kind: SessionKind, node: &str) -> Self {
                let mut client = Self::connect(socket);
                client.send(ClientMessage::Begin {
                    kind,
                    node: PathBuf::from("/dev").join(node),
                });
                assert!(matches!(client.receive(), BridgeMessage::Started { .. }));
                client
            }

            fn send(&mut self, message: ClientMessage) {
                self.writer
                    .write_all(message.encode().unwrap().as_bytes())
                    .unwrap();
            }

            fn receive(&mut self) -> BridgeMessage {
                let mut line = String::new();
                assert_ne!(
                    self.reader.read_line(&mut line).unwrap(),
                    0,
                    "session closed"
                );
                BridgeMessage::decode(line.trim_end()).unwrap()
            }

            fn closed(&mut self) -> bool {
                let mut line = String::new();
                self.reader.read_line(&mut line).unwrap() == 0
            }
        }

        fn measured_gyro(client: &mut Client) -> u64 {
            client.send(ClientMessage::MeasureGyro);
            assert_eq!(client.receive(), BridgeMessage::Progress(Progress::Ready));
            match client.receive() {
                BridgeMessage::GyroMeasured {
                    measurement,
                    controller,
                    bias_dps,
                } => {
                    assert_eq!(controller, "00:11:22:33:44:55");
                    assert_eq!(bias_dps, [0.25, 0.0, -0.1]);
                    measurement
                }
                other => panic!("unexpected reply {other:?}"),
            }
        }

        #[test]
        fn gyro_session_saves_the_latest_measurement_and_resumes_outputs() {
            let fixture = Fixture::new("gyro", Timeouts::default());
            fixture.run(|socket| {
                let mut client = Client::begin(&socket, SessionKind::Gyro, "gyro");
                // Only one session may own the controller.
                let mut second = Client::connect(&socket);
                assert_eq!(second.receive(), BridgeMessage::Busy);
                client.send(ClientMessage::SaveGyro { measurement: 1 });
                assert!(matches!(client.receive(), BridgeMessage::Rejected(_)));
                let first = measured_gyro(&mut client);
                let latest = measured_gyro(&mut client);
                assert_ne!(first, latest);
                client.send(ClientMessage::SaveGyro { measurement: first });
                assert!(matches!(client.receive(), BridgeMessage::Rejected(_)));
                client.send(ClientMessage::MeasureGame { sensitivity: 2.5 });
                assert!(matches!(client.receive(), BridgeMessage::Rejected(_)));
                client.send(ClientMessage::SaveGyro {
                    measurement: latest,
                });
                assert_eq!(client.receive(), BridgeMessage::GyroSaved);
                client.send(ClientMessage::End);
                assert!(client.closed());
            });
            assert!(!fixture.paused.get());
            assert_eq!(
                *fixture.record.0.lock().unwrap(),
                ["gyro 00:11:22:33:44:55 [0.25, 0.0, -0.1]"]
            );
        }

        #[test]
        fn game_session_reports_progress_and_save_failures() {
            let fixture = Fixture::new("game", Timeouts::default());
            fixture.run(|socket| {
                let mut client = Client::begin(&socket, SessionKind::Game, "game");
                client.send(ClientMessage::MeasureGame { sensitivity: 2.5 });
                for progress in [
                    Progress::Ready,
                    Progress::CenterStick,
                    Progress::Started,
                    Progress::NoMotion,
                ] {
                    assert_eq!(client.receive(), BridgeMessage::Progress(progress));
                }
                let BridgeMessage::GameMeasured {
                    measurement,
                    counts: 28_800,
                } = client.receive()
                else {
                    panic!("expected a measurement");
                };
                client.send(ClientMessage::SaveGame {
                    measurement,
                    turns: 2,
                    profile: "/profiles/fail.toml".into(),
                });
                assert_eq!(client.receive(), BridgeMessage::Failed("disk full".into()));
                client.send(ClientMessage::SaveGame {
                    measurement,
                    turns: 2,
                    profile: "/profiles/game.toml".into(),
                });
                assert_eq!(
                    client.receive(),
                    BridgeMessage::GameSaved { calibration: 40.0 }
                );
            });
            assert!(!fixture.paused.get());
            assert_eq!(
                *fixture.record.0.lock().unwrap(),
                ["game /profiles/game.toml 28800 2.5 2"]
            );
        }

        /// Measure and save an RWC result to `profile` in one session.
        fn save_game(socket: &Path, profile: PathBuf) {
            let mut client = Client::begin(socket, SessionKind::Game, "game");
            client.send(ClientMessage::MeasureGame { sensitivity: 2.5 });
            let measurement = loop {
                match client.receive() {
                    BridgeMessage::Progress(_) => {}
                    BridgeMessage::GameMeasured { measurement, .. } => break measurement,
                    other => panic!("unexpected reply {other:?}"),
                }
            };
            client.send(ClientMessage::SaveGame {
                measurement,
                turns: 2,
                profile,
            });
            assert_eq!(
                client.receive(),
                BridgeMessage::GameSaved { calibration: 40.0 }
            );
        }

        impl Fixture {
            /// The next profile update the main loop receives within `limit`.
            fn next_profile(&self, limit: Duration) -> Option<Box<RuntimeOptions>> {
                let (_sender, idle) = UnixStream::pair().unwrap();
                let deadline = Instant::now() + limit;
                while Instant::now() < deadline {
                    let (_, request, _) =
                        wait(idle.as_fd(), Some(&self.reload), None, None, Some(deadline)).unwrap();
                    if let Some(request) = request {
                        let Some(Update::Profile(options)) =
                            request.resolve(ControllerFilter::default())
                        else {
                            panic!("unexpected update");
                        };
                        request.applied(None).unwrap();
                        return Some(options);
                    }
                }
                None
            }
        }

        #[test]
        fn saving_rwc_to_the_active_profile_reloads_it() {
            let fixture = Fixture::new("reload-rwc", Timeouts::default());
            let active = fixture.directory.join("active.toml");
            fs::write(
                &active,
                "version = 1\n[gyro]\nreal_world_calibration = 40.0\nin_game_sens = 2.5\n",
            )
            .unwrap();
            let other = fixture.directory.join("other.toml");
            fs::write(&other, "version = 1\n").unwrap();

            // A profile that is not running stays as it is.
            fixture.reload.active.replace(Some(active.clone()));
            fixture.run({
                let other = other.clone();
                move |socket| save_game(&socket, other)
            });
            assert!(fixture.next_profile(Duration::from_millis(200)).is_none());

            // The running profile is read again after the session releases
            // the controller, also when the session names it through a link.
            let alias = fixture.directory.join("alias.toml");
            std::os::unix::fs::symlink(&active, &alias).unwrap();
            fixture.run(move |socket| save_game(&socket, alias));
            assert!(!fixture.paused.get());
            let options = fixture
                .next_profile(Duration::from_secs(5))
                .expect("the active profile was reloaded");
            assert!(options.gyro.is_some());
            assert_eq!(
                fixture.reload.active.borrow().as_deref(),
                Some(active.as_path())
            );
        }

        #[test]
        fn previously_paused_outputs_stay_paused_after_a_failed_measurement() {
            let fixture = Fixture::new("paused", Timeouts::default());
            fixture.paused.set(true);
            fixture.run(|socket| {
                let mut client = Client::begin(&socket, SessionKind::Gyro, "unplugged");
                client.send(ClientMessage::MeasureGyro);
                assert_eq!(client.receive(), BridgeMessage::Progress(Progress::Ready));
                assert_eq!(
                    client.receive(),
                    BridgeMessage::Failed(
                        "Controller disconnected before gyro calibration finished.".into()
                    )
                );
                client.send(ClientMessage::SaveGyro { measurement: 1 });
                assert!(matches!(client.receive(), BridgeMessage::Rejected(_)));
            });
            assert!(fixture.paused.get());
            assert!(fixture.record.0.lock().unwrap().is_empty());
        }

        #[test]
        fn client_disconnect_stops_the_worker_and_restores_outputs() {
            let fixture = Fixture::new("crash", Timeouts::default());
            let pid_file = fixture.directory.join("slow.pid");
            let pid = fixture.run(move |socket| {
                let mut client = Client::begin(&socket, SessionKind::Game, "slow");
                client.send(ClientMessage::MeasureGame { sensitivity: 2.5 });
                assert_eq!(client.receive(), BridgeMessage::Progress(Progress::Ready));
                let deadline = Instant::now() + Duration::from_secs(5);
                let pid = loop {
                    if let Some(pid) = fs::read_to_string(&pid_file)
                        .ok()
                        .and_then(|text| text.trim().parse::<i32>().ok())
                    {
                        break pid;
                    }
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(10));
                };
                // Simulate the window crashing mid-measurement.
                drop(client);
                pid
            });
            assert!(!fixture.paused.get());
            assert!(!Path::new(&format!("/proc/{pid}")).exists());
        }

        #[test]
        fn cancel_and_timeouts_are_bounded() {
            let fixture = Fixture::new(
                "timeouts",
                Timeouts {
                    game: Duration::from_millis(300),
                    idle: Duration::from_millis(600),
                    ..Timeouts::default()
                },
            );
            fixture.run(|socket| {
                let mut client = Client::begin(&socket, SessionKind::Game, "slow");
                client.send(ClientMessage::Cancel);
                assert!(matches!(client.receive(), BridgeMessage::Rejected(_)));
                client.send(ClientMessage::MeasureGame { sensitivity: 2.5 });
                assert_eq!(client.receive(), BridgeMessage::Progress(Progress::Ready));
                client.send(ClientMessage::Cancel);
                assert_eq!(client.receive(), BridgeMessage::Cancelled);
                client.send(ClientMessage::MeasureGame { sensitivity: 2.5 });
                assert_eq!(client.receive(), BridgeMessage::Progress(Progress::Ready));
                assert_eq!(
                    client.receive(),
                    BridgeMessage::Failed("The measurement timed out.".into())
                );
                assert_eq!(client.receive(), BridgeMessage::Closed(CloseReason::Idle));
                assert!(client.closed());
            });
            assert!(!fixture.paused.get());
        }

        #[test]
        fn sessions_require_begin_and_handshake_in_time() {
            let fixture = Fixture::new(
                "handshake",
                Timeouts {
                    handshake: Duration::from_millis(200),
                    ..Timeouts::default()
                },
            );
            fixture.run(|socket| {
                let mut client = Client::connect(&socket);
                client.send(ClientMessage::MeasureGyro);
                assert!(matches!(client.receive(), BridgeMessage::Rejected(_)));
                assert!(client.closed());
                let mut silent = Client::connect(&socket);
                assert!(silent.closed());
            });
            assert!(!fixture.paused.get());
        }

        #[test]
        fn a_session_lease_blocks_resume_and_keeps_a_requested_pause() {
            let fixture = Fixture::new("lease", Timeouts::default());
            let reload = &fixture.reload;
            reload.lease.set(Some(Lease {
                session: 9,
                restore_paused: false,
            }));
            let address = SocketAddr::from_abstract_name("joyshockrust-lease-test").unwrap();
            let command = |paused| {
                reload
                    .addressed_request(
                        address.clone(),
                        &ControlCommand::OutputsPaused(paused).encode(),
                    )
                    .unwrap()
            };
            assert!(command(false).is_none());
            let pause = command(true).unwrap();
            assert!(matches!(
                pause.resolve(ControllerFilter::default()),
                Some(Update::OutputsPaused(true))
            ));
            pause.applied(None).unwrap();
            assert_eq!(
                reload.lease.get(),
                Some(Lease {
                    session: 9,
                    restore_paused: true
                })
            );
            let release = Request {
                reload,
                reply: None,
                action: Action::Lease(LeaseAction::Release(8)),
            };
            assert!(release.resolve(ControllerFilter::default()).is_none());
            let release = Request {
                reload,
                reply: None,
                action: Action::Lease(LeaseAction::Release(9)),
            };
            assert!(matches!(
                release.resolve(ControllerFilter::default()),
                Some(Update::OutputsPaused(true))
            ));
            release.applied(None).unwrap();
            assert!(reload.lease.get().is_none());
            assert!(command(false).is_some());
        }
    }
}
