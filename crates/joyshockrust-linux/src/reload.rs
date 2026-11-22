use std::{
    cell::Cell,
    fmt::Display,
    io::{self, Read, Write},
    os::{
        fd::{AsFd, BorrowedFd},
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
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

pub struct Reload {
    path: PathBuf,
    bridge_at_startup: BridgeOptions,
    signal: SignalFd,
    listener: Option<UnixListener>,
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
    client: Option<UnixStream>,
}

impl Request<'_> {
    pub fn load(&self) -> Option<RuntimeOptions> {
        match profile::load(&self.reload.path) {
            Ok(profile) if profile.bridge == self.reload.bridge_at_startup => Some(profile),
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
        eprintln!("Profile reloaded: {}", self.reload.path.display());
        self.reply(b"applied\n");
    }
    pub fn rejected(&self, err: impl Display) {
        self.reload.outcome.set(Outcome::Rejected);
        eprintln!(
            "Profile reload rejected ({}): {err}",
            self.reload.path.display()
        );
        self.reply(b"rejected\n");
    }

    fn reply(&self, response: &[u8]) {
        if let Some(mut client) = self.client.as_ref() {
            let _ = client.write_all(response);
        }
    }
}

pub fn request(path: Option<&Path>) -> io::Result<()> {
    let stream = status::connect(path, "reload.sock")?;
    let mut response = Vec::new();
    stream.take(32).read_to_end(&mut response)?;
    match response.as_slice() {
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

pub fn wait<'a>(
    fd: BorrowedFd<'_>,
    reload: Option<&'a Reload>,
    status: Option<&StatusSocket>,
    feedback: Option<BorrowedFd<'_>>,
) -> io::Result<(PollFlags, Option<Request<'a>>, PollFlags)> {
    let listener = reload.and_then(|reload| reload.listener.as_ref());
    let mut descriptors = [
        PollFd::new(fd, PollFlags::POLLIN),
        PollFd::new(
            reload.map_or(fd, |reload| reload.signal.as_fd()),
            reload.map_or(PollFlags::empty(), |_| PollFlags::POLLIN),
        ),
        PollFd::new(status.map_or(fd, StatusSocket::as_fd), PollFlags::POLLIN),
        PollFd::new(listener.map_or(fd, AsFd::as_fd), PollFlags::POLLIN),
        PollFd::new(feedback.unwrap_or(fd), PollFlags::POLLIN),
    ];
    let len = if feedback.is_some() {
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
        match poll(&mut descriptors[..len], PollTimeout::NONE) {
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
                Ok((stream, _)) if stream.set_nonblocking(true).is_ok() => client = Some(stream),
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
    let request = reload
        .filter(|_| requested || client.is_some())
        .map(|reload| Request { reload, client });
    Ok((
        descriptors[0].revents().unwrap_or_else(PollFlags::empty),
        request,
        descriptors[4].revents().unwrap_or_else(PollFlags::empty),
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

        let (ready, request, _) = wait(input.as_fd(), Some(&reload), None, None).unwrap();
        assert!(ready.is_empty());
        assert!(request.unwrap().load().is_none());
        let mut response = String::new();
        first.read_to_string(&mut response).unwrap();
        assert_eq!(response, "rejected\n");

        let (_, request, _) = wait(input.as_fd(), Some(&reload), None, None).unwrap();
        request.unwrap().applied();
        response.clear();
        second.read_to_string(&mut response).unwrap();
        assert_eq!(response, "applied\n");
    }
}
