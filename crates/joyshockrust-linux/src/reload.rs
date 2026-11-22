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
    profile::{self, BridgeOptions, RuntimeOptions},
    status::{self, StatusSocket},
};

const MAX_PROFILE_BYTES: usize = 64 * 1024;

pub struct Reload {
    path: PathBuf,
    bridge_at_startup: BridgeOptions,
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
        path: PathBuf,
        socket: Option<&Path>,
        bridge_at_startup: BridgeOptions,
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
            signal,
            listener,
            load_socket,
            outcome: Cell::new(Outcome::NotAttempted),
        })
    }

    fn outcome(&self) -> &'static str {
        match self.outcome.get() {
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

impl Request<'_> {
    pub fn load(&self) -> Option<RuntimeOptions> {
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
        } else {
            profile::load(&self.reload.path)
        };
        match profile {
            Ok(profile)
                if (inline.is_some() && profile.bridge.is_none())
                    || profile.bridge.unwrap_or_default() == self.reload.bridge_at_startup =>
            {
                Some(profile)
            }
            Ok(_) => {
                self.rejected("changes to [bridge] require restarting JoyShockRust");
                None
            }
            Err(err) => {
                self.rejected(err);
                None
            }
        }
    }

    pub fn applied(self) {
        self.reload.outcome.set(Outcome::Applied);
        if matches!(self.client, Some(Client::Load(_, _))) {
            eprintln!("Profile loaded from CLI");
        } else {
            eprintln!("Profile reloaded: {}", self.reload.path.display());
        }
        self.reply(b"applied\n");
    }
    pub fn rejected(&self, err: impl Display) {
        self.reload.outcome.set(Outcome::Rejected);
        if matches!(self.client, Some(Client::Load(_, _))) {
            eprintln!("CLI profile rejected: {err}");
        } else {
            eprintln!(
                "Profile reload rejected ({}): {err}",
                self.reload.path.display()
            );
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
    check_reply(&response)
}

fn check_reply(response: &[u8]) -> io::Result<()> {
    match response {
        b"applied\n" => Ok(()),
        b"rejected\n" => Err(io::Error::other(
            "profile reload rejected; see the bridge log for details",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "profile reload returned no valid result",
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
    socket.send(&profile)?;
    let mut response = [0; 32];
    let len = socket.recv(&mut response)?;
    check_reply(&response[..len])
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
        let mut reload = Reload {
            path: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/gyro-except-l2.toml"),
            bridge_at_startup: BridgeOptions {
                rumble: true,
                ..BridgeOptions::default()
            },
            signal: SignalFd::new(&SigSet::empty()).unwrap(),
            listener: None,
            load_socket: None,
            outcome: Cell::new(Outcome::NotAttempted),
        };
        assert!(
            Request {
                reload: &reload,
                client: None
            }
            .load()
            .is_none()
        );
        assert_eq!(reload.outcome(), "rejected");

        reload.bridge_at_startup = BridgeOptions {
            model: crate::discovery::ModelSelection::DualSenseEdge,
            ..BridgeOptions::default()
        };
        assert!(
            Request {
                reload: &reload,
                client: None
            }
            .load()
            .is_none()
        );
        assert_eq!(reload.outcome(), "rejected");

        reload.bridge_at_startup = BridgeOptions::default();
        assert!(
            Request {
                reload: &reload,
                client: None
            }
            .load()
            .is_some()
        );
    }

    #[test]
    fn live_profile_accepts_omitted_bridge_and_keeps_the_startup_file() {
        let base = format!("joyshockrust-live-profile-{}", process::id());
        let address = SocketAddr::from_abstract_name(format!("{base}-server")).unwrap();
        let load_socket = UnixDatagram::bind_addr(&address).unwrap();
        load_socket.set_nonblocking(true).unwrap();
        let reload = Reload {
            path: PathBuf::from("/dev/null"),
            bridge_at_startup: BridgeOptions {
                rumble: true,
                ..BridgeOptions::default()
            },
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
        assert!(request.load().unwrap().gyro.is_some());
        request.applied();
        let mut reply = [0; 32];
        let len = client.recv(&mut reply).unwrap();
        assert_eq!(&reply[..len], b"applied\n");
        assert_eq!(reload.path, Path::new("/dev/null"));
    }

    #[test]
    fn reload_socket_replies_with_the_result_for_each_client() {
        let address =
            SocketAddr::from_abstract_name(format!("joyshockrust-reload-{}", process::id()))
                .unwrap();
        let listener = UnixListener::bind_addr(&address).unwrap();
        listener.set_nonblocking(true).unwrap();
        let reload = Reload {
            path: PathBuf::from("/dev/null"),
            bridge_at_startup: BridgeOptions::default(),
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
