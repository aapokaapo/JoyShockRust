//! Demand-driven Unix transport for bridge snapshot sources.

use std::{
    io,
    marker::PhantomData,
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

use super::server::{self, Snapshot, Worker};
use crate::{
    stream::STATUS_LIMIT,
    telemetry::preview::{self, History},
};

pub type StatusPublisher = Publisher<String>;
pub type PreviewPublisher = Publisher<History>;

pub trait SnapshotPayload: Snapshot + Send + 'static {
    const LIMIT: usize;
    fn body_len(&self) -> usize;
    fn write_body(&self, target: &mut Vec<u8>) -> io::Result<()>;
    fn read_body(body: &[u8]) -> io::Result<Self>
    where
        Self: Sized;
}

impl SnapshotPayload for String {
    const LIMIT: usize = STATUS_LIMIT;

    fn body_len(&self) -> usize {
        self.len()
    }

    fn write_body(&self, target: &mut Vec<u8>) -> io::Result<()> {
        target.extend(self.as_bytes());
        Ok(())
    }

    fn read_body(body: &[u8]) -> io::Result<Self> {
        String::from_utf8(body.to_vec()).map_err(io::Error::other)
    }
}

impl SnapshotPayload for History {
    const LIMIT: usize = preview::BODY_LIMIT;

    fn body_len(&self) -> usize {
        History::body_len(self)
    }

    fn write_body(&self, target: &mut Vec<u8>) -> io::Result<()> {
        History::write_body(self, target)
    }

    fn read_body(body: &[u8]) -> io::Result<Self> {
        History::read_body(body)
    }
}

pub struct Publisher<T> {
    socket: UnixDatagram,
    demand: Arc<AtomicU64>,
    _worker: Worker,
    snapshot: PhantomData<T>,
}

impl<T: SnapshotPayload> Publisher<T> {
    pub fn start(label: &str, status: &Path) -> io::Result<Self> {
        let (socket, destination) = UnixDatagram::pair()?;
        socket.set_nonblocking(true)?;
        destination.set_nonblocking(true)?;
        // Odd generations have subscribers. A datagram queued for a previous
        // generation must not initialize a new subscription.
        let demand = Arc::new(AtomicU64::new(0));
        let worker_demand = demand.clone();
        let worker = Worker::start(
            label,
            T::TOPIC.socket_path(status),
            move |listener, shutdown| {
                let mut buffer = vec![0; 8 + T::LIMIT + 1];
                let result = server::serve(
                    listener,
                    shutdown,
                    destination.as_fd(),
                    None,
                    || {
                        receive::<T>(
                            &destination,
                            worker_demand.load(Ordering::Acquire),
                            &mut buffer,
                        )
                    },
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
            snapshot: PhantomData,
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

    pub fn generation(&self) -> Option<u64> {
        let generation = self.demand.load(Ordering::Acquire);
        (!generation.is_multiple_of(2)).then_some(generation)
    }

    pub fn has_subscribers(&self) -> bool {
        self.generation().is_some()
    }

    pub fn send(&self, snapshot: impl FnOnce() -> T) -> io::Result<()> {
        let Some(generation) = self.generation() else {
            return Ok(());
        };
        self.send_for(generation, snapshot)
    }

    pub fn send_for(&self, generation: u64, snapshot: impl FnOnce() -> T) -> io::Result<()> {
        if self.generation() != Some(generation) {
            return Ok(());
        }
        let snapshot = snapshot();
        let mut message = Vec::with_capacity(8 + snapshot.body_len().min(T::LIMIT));
        message.extend(generation.to_le_bytes());
        snapshot.write_body(&mut message)?;
        if message.len() - 8 > T::LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "subscription snapshot too large",
            ));
        }
        self.socket.send(&message).map(|_| ())
    }
}

fn receive<T: SnapshotPayload>(
    socket: &UnixDatagram,
    generation: u64,
    bytes: &mut [u8],
) -> io::Result<Option<T>> {
    let len = match socket.recv(bytes) {
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
    if body.len() > T::LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "subscription snapshot too large",
        ));
    }
    T::read_body(body).map(Some)
}
