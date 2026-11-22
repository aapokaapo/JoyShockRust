use std::{
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

use crate::profile::{self, RuntimeOptions};

pub struct Reload {
    path: PathBuf,
    signal: SignalFd,
}

impl Reload {
    pub fn new(path: PathBuf) -> io::Result<Self> {
        let mut signals = SigSet::empty();
        signals.add(Signal::SIGHUP);
        signals.thread_block()?;
        let signal =
            SignalFd::with_flags(&signals, SfdFlags::SFD_CLOEXEC | SfdFlags::SFD_NONBLOCK)?;
        Ok(Self { path, signal })
    }

    pub fn load(&self) -> Option<RuntimeOptions> {
        match profile::load(&self.path) {
            Ok(profile) => Some(profile),
            Err(err) => {
                eprintln!("Profile reload rejected ({}): {err}", self.path.display());
                None
            }
        }
    }

    pub fn applied(&self) {
        eprintln!("Profile reloaded: {}", self.path.display());
    }
}

pub fn wait(fd: BorrowedFd<'_>, reload: Option<&Reload>) -> io::Result<(PollFlags, bool)> {
    let mut fds = [
        PollFd::new(fd, PollFlags::POLLIN),
        PollFd::new(
            reload.map_or(fd, |reload| reload.signal.as_fd()),
            PollFlags::POLLIN,
        ),
    ];
    let len = if reload.is_some() { 2 } else { 1 };
    loop {
        match poll(&mut fds[..len], PollTimeout::NONE) {
            Ok(_) => break,
            Err(Errno::EINTR) => continue,
            Err(err) => return Err(err.into()),
        }
    }
    let mut requested = false;
    if let Some(reload) = reload {
        let flags = fds[1].revents().unwrap_or_else(PollFlags::empty);
        if flags.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
            return Err(io::Error::other("profile reload signal descriptor lost"));
        }
        if flags.contains(PollFlags::POLLIN) {
            requested = reload.signal.read_signal()?.is_some();
        }
    }
    Ok((fds[0].revents().unwrap_or_else(PollFlags::empty), requested))
}
