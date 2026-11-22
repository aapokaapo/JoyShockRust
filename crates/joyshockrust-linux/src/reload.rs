use std::{
    cell::Cell,
    fmt::Display,
    io,
    os::fd::{AsFd, BorrowedFd},
    path::PathBuf,
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
    profile::{self, RuntimeOptions},
    status::StatusSocket,
};

pub struct Reload {
    path: PathBuf,
    signal: SignalFd,
    outcome: Cell<Outcome>,
}

#[derive(Clone, Copy)]
enum Outcome {
    NotAttempted,
    Applied,
    Rejected,
}

impl Reload {
    pub fn new(path: PathBuf) -> io::Result<Self> {
        let mut signals = SigSet::empty();
        signals.add(Signal::SIGHUP);
        signals.thread_block()?;
        let signal =
            SignalFd::with_flags(&signals, SfdFlags::SFD_CLOEXEC | SfdFlags::SFD_NONBLOCK)?;
        Ok(Self {
            path,
            signal,
            outcome: Cell::new(Outcome::NotAttempted),
        })
    }

    pub fn load(&self) -> Option<RuntimeOptions> {
        match profile::load(&self.path) {
            Ok(profile) => Some(profile),
            Err(err) => {
                self.rejected(err);
                None
            }
        }
    }

    pub fn applied(&self) {
        self.outcome.set(Outcome::Applied);
        eprintln!("Profile reloaded: {}", self.path.display());
    }
    pub fn rejected(&self, err: impl Display) {
        self.outcome.set(Outcome::Rejected);
        eprintln!("Profile reload rejected ({}): {err}", self.path.display());
    }

    fn outcome(&self) -> &'static str {
        match self.outcome.get() {
            Outcome::NotAttempted => "not_attempted",
            Outcome::Applied => "applied",
            Outcome::Rejected => "rejected",
        }
    }
}

pub fn wait(
    fd: BorrowedFd<'_>,
    reload: Option<&Reload>,
    status: Option<&StatusSocket>,
) -> io::Result<(PollFlags, bool)> {
    let mut descriptors = [
        PollFd::new(fd, PollFlags::POLLIN),
        PollFd::new(
            reload.map_or(fd, |reload| reload.signal.as_fd()),
            reload.map_or(PollFlags::empty(), |_| PollFlags::POLLIN),
        ),
        PollFd::new(status.map_or(fd, StatusSocket::as_fd), PollFlags::POLLIN),
    ];
    let len = if status.is_some() {
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
    Ok((
        descriptors[0].revents().unwrap_or_else(PollFlags::empty),
        requested,
    ))
}
