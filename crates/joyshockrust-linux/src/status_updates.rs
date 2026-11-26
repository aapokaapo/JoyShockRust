//! Schedules desktop snapshots at report boundaries in the bridge.

use std::{
    cell::Cell,
    io,
    os::fd::BorrowedFd,
    path::Path,
    time::{Duration, Instant},
};

use joyshockrust_ipc::unix::subscriptions::publisher::StatusPublisher;

const INTERVAL: Duration = Duration::from_millis(250);

pub struct Publisher {
    transport: StatusPublisher,
    dirty: Cell<bool>,
    next_publish: Cell<Option<Instant>>,
    last_reload: Cell<&'static str>,
}

impl Publisher {
    pub fn start(status: &Path) -> io::Result<Self> {
        Ok(Self {
            transport: StatusPublisher::start("status", status)?,
            dirty: Cell::new(true),
            next_publish: Cell::new(None),
            last_reload: Cell::new(""),
        })
    }

    pub fn wake_fd(&self) -> BorrowedFd<'_> {
        self.transport.wake_fd()
    }

    pub fn changed(&self) {
        self.dirty.set(true);
    }

    pub fn publish(
        &self,
        wake: bool,
        last_reload: &'static str,
        render: impl FnOnce() -> String,
    ) -> Option<Instant> {
        if wake {
            self.transport.drain_wake();
        }
        if !self.transport.has_subscribers()
            || (!wake && !self.dirty.get() && self.last_reload.get() == last_reload)
        {
            return None;
        }
        let now = Instant::now();
        if !wake && let Some(next) = self.next_publish.get().filter(|&next| next > now) {
            return Some(next);
        }
        let next = now + INTERVAL;
        self.next_publish.set(Some(next));
        self.last_reload.set(last_reload);
        let result = self.transport.send(render);
        let retry = result.is_err_and(|error| {
            matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            )
        });
        self.dirty.set(retry);
        retry.then_some(next)
    }
}
