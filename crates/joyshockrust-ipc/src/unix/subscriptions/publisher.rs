//! Demand-driven Unix transport for a status snapshot source.

use std::{
    io,
    os::{
        fd::{AsFd, BorrowedFd},
        unix::net::UnixDatagram,
    },
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use super::server::{self, Worker};
use crate::stream::{STATUS_LIMIT, Topic};

pub struct StatusPublisher {
    socket: UnixDatagram,
    demand: Arc<AtomicU64>,
    _worker: Worker,
}

impl StatusPublisher {
    pub fn start(status: &Path) -> io::Result<Self> {
        let (socket, destination) = UnixDatagram::pair()?;
        socket.set_nonblocking(true)?;
        destination.set_nonblocking(true)?;
        // Odd generations have subscribers. A datagram queued for a previous
        // generation must not initialize a new subscription.
        let demand = Arc::new(AtomicU64::new(0));
        let worker_demand = demand.clone();
        let worker = Worker::start(
            "status",
            Topic::Status.socket_path(status),
            move |listener, shutdown| {
                let result = server::serve(
                    listener,
                    shutdown,
                    destination.as_fd(),
                    None,
                    || receive(&destination, worker_demand.load(Ordering::Acquire)),
                    |active| {
                        worker_demand.fetch_add(1, Ordering::AcqRel);
                        if active {
                            let _ = destination.send(&[1]);
                        }
                    },
                );
                worker_demand.store(0, Ordering::Release);
                result
            },
        )?;
        Ok(Self {
            socket,
            demand,
            _worker: worker,
        })
    }

    pub fn wake_fd(&self) -> BorrowedFd<'_> {
        self.socket.as_fd()
    }

    pub fn drain_wake(&self) {
        for _ in 0..8 {
            if self.socket.recv(&mut [0]).is_err() {
                break;
            }
        }
    }

    pub fn has_subscribers(&self) -> bool {
        !self.demand.load(Ordering::Acquire).is_multiple_of(2)
    }

    pub fn send(&self, snapshot: impl FnOnce() -> String) -> io::Result<()> {
        let generation = self.demand.load(Ordering::Acquire);
        if generation.is_multiple_of(2) {
            return Ok(());
        }
        let snapshot = snapshot();
        if snapshot.len() > STATUS_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "status snapshot too large",
            ));
        }
        let mut message = Vec::with_capacity(8 + snapshot.len());
        message.extend(generation.to_le_bytes());
        message.extend(snapshot.as_bytes());
        self.socket.send(&message).map(|_| ())
    }
}

fn receive(socket: &UnixDatagram, generation: u64) -> io::Result<Option<String>> {
    let mut bytes = [0; 8 + STATUS_LIMIT + 1];
    let len = match socket.recv(&mut bytes) {
        Ok(len) => len,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let message = &bytes[..len];
    if !message.starts_with(&generation.to_le_bytes()) {
        return Ok(None);
    }
    let body = &message[8..];
    if body.len() > STATUS_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "status snapshot too large",
        ));
    }
    String::from_utf8(body.to_vec())
        .map(Some)
        .map_err(io::Error::other)
}
