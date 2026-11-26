//! Publishes status between report batches while desktop clients are subscribed.

use std::{
    cell::Cell,
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
    time::{Duration, Instant},
};

use joyshockrust_ipc::stream::{STATUS_LIMIT, Stamp, Topic, encode_status};

use crate::subscriptions::{self, Worker};

const INTERVAL: Duration = Duration::from_millis(250);

pub struct Publisher {
    socket: UnixDatagram,
    demand: Arc<AtomicU64>,
    dirty: Cell<bool>,
    next_publish: Cell<Option<Instant>>,
    last_reload: Cell<&'static str>,
    _worker: Worker,
}

impl Publisher {
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
                let result = subscriptions::serve(
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
            dirty: Cell::new(true),
            next_publish: Cell::new(None),
            last_reload: Cell::new(""),
            _worker: worker,
        })
    }

    pub fn wake_fd(&self) -> BorrowedFd<'_> {
        self.socket.as_fd()
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
            for _ in 0..8 {
                if self.socket.recv(&mut [0]).is_err() {
                    break;
                }
            }
        }
        let generation = self.demand.load(Ordering::Acquire);
        if generation.is_multiple_of(2)
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
        let snapshot = render();
        if snapshot.len() > STATUS_LIMIT {
            // The legacy snapshot remains available for larger payloads.
            self.dirty.set(false);
            return None;
        }
        let mut message = Vec::with_capacity(8 + snapshot.len());
        message.extend(generation.to_le_bytes());
        message.extend(snapshot.as_bytes());
        let result = self.socket.send(&message);
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

impl subscriptions::Snapshot for String {
    const TOPIC: Topic = Topic::Status;
    const ON_DEMAND: bool = true;

    fn frame(&self, request: u32, stamp: Stamp) -> io::Result<Vec<u8>> {
        encode_status(request, stamp, self)
    }
}
