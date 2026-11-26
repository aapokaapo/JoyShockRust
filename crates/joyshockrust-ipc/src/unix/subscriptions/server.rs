//! Bounded, nonblocking transport for private desktop streams.

use crate::{
    inventory::{Controller, Snapshot as InventorySnapshot},
    stream::{HEADER_LEN, Stamp, Topic, encode_status},
};
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
    sys::socket::{getsockopt, sockopt::PeerCredentials},
    unistd::geteuid,
};
use std::{
    fs,
    io::{self, ErrorKind, Read, Write},
    net::Shutdown,
    os::{
        fd::{AsFd, BorrowedFd},
        unix::{
            fs::{MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_SUBSCRIBERS: usize = 8;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);
const READY_ERRORS: PollFlags = PollFlags::POLLERR
    .union(PollFlags::POLLHUP)
    .union(PollFlags::POLLNVAL);

pub trait Snapshot: PartialEq {
    const TOPIC: Topic;
    const ON_DEMAND: bool = false;
    fn frame(&self, request: u32, stamp: Stamp) -> io::Result<Vec<u8>>;
}

impl Snapshot for Option<Vec<Controller>> {
    const TOPIC: Topic = Topic::Inventory;

    fn frame(&self, request: u32, stamp: Stamp) -> io::Result<Vec<u8>> {
        InventorySnapshot {
            instance: stamp.instance,
            revision: stamp.revision,
            controllers: self.clone(),
        }
        .encode(request)
    }
}

impl Snapshot for String {
    const TOPIC: Topic = Topic::Status;
    const ON_DEMAND: bool = true;

    fn frame(&self, request: u32, stamp: Stamp) -> io::Result<Vec<u8>> {
        encode_status(request, stamp, self)
    }
}

impl Snapshot for crate::telemetry::preview::History {
    const TOPIC: Topic = Topic::Preview;
    const ON_DEMAND: bool = true;

    fn frame(&self, request: u32, stamp: Stamp) -> io::Result<Vec<u8>> {
        self.frame(request, stamp)
    }
}

pub struct Worker {
    stop: UnixStream,
    thread: Option<thread::JoinHandle<()>>,
}

impl Worker {
    pub fn start(
        label: &str,
        path: PathBuf,
        run: impl FnOnce(&UnixListener, BorrowedFd<'_>) -> io::Result<()> + Send + 'static,
    ) -> io::Result<Self> {
        let listener = UnixListener::bind(&path)?;
        let metadata = fs::symlink_metadata(&path)?;
        let socket = SocketFile {
            path,
            identity: (metadata.dev(), metadata.ino()),
        };
        fs::set_permissions(&socket.path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let (stop, shutdown) = UnixStream::pair()?;
        let thread = thread::Builder::new().name(format!("jsr-{label}")).spawn({
            let label = label.to_owned();
            move || {
                let _socket = socket;
                if let Err(error) = run(&listener, shutdown.as_fd()) {
                    eprintln!("{label} subscription stopped: {error}");
                }
            }
        })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.stop.shutdown(Shutdown::Both);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct SocketFile {
    path: PathBuf,
    identity: (u64, u64),
}

impl Drop for SocketFile {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|meta| (meta.dev(), meta.ino()) == self.identity)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct Subscriber {
    stream: UnixStream,
    request: [u8; HEADER_LEN],
    received: usize,
    id: Option<u32>,
    reply: Vec<u8>,
    sent: usize,
    deadline: Option<Instant>,
}

impl Subscriber {
    fn new(stream: UnixStream, now: Instant) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            request: [0; HEADER_LEN],
            received: 0,
            id: None,
            reply: Vec::new(),
            sent: 0,
            deadline: Some(now + CLIENT_TIMEOUT),
        })
    }

    fn interest(&self) -> PollFlags {
        if self.reply.is_empty() {
            PollFlags::POLLIN
        } else {
            PollFlags::POLLIN | PollFlags::POLLOUT
        }
    }

    fn queue(&mut self, snapshot: &impl Snapshot, stamp: Stamp, now: Instant) -> io::Result<()> {
        if let Some(id) = self.id {
            if !self.reply.is_empty() && self.sent != 0 {
                return Err(io::Error::other("subscription client fell behind"));
            }
            if self.reply.is_empty() {
                self.deadline = Some(now + CLIENT_TIMEOUT);
            }
            self.reply = snapshot.frame(id, stamp)?;
            self.sent = 0;
        }
        Ok(())
    }

    fn service<T: Snapshot>(
        &mut self,
        ready: PollFlags,
        snapshot: Option<&T>,
        stamp: Stamp,
        now: Instant,
    ) -> io::Result<()> {
        if ready.intersects(READY_ERRORS) || self.deadline.is_some_and(|deadline| deadline <= now) {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        if ready.contains(PollFlags::POLLIN) {
            // Once subscribed, unexpected input or EOF closes the stream.
            let mut extra = [0];
            let buffer = if self.id.is_some() {
                &mut extra
            } else {
                &mut self.request[self.received..]
            };
            match self.stream.read(buffer) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(_) if self.id.is_some() => return Err(io::ErrorKind::InvalidData.into()),
                Ok(count) => {
                    self.received += count;
                    if self.received == HEADER_LEN {
                        self.id = Some(T::TOPIC.parse_subscription(&self.request)?);
                        if let Some(snapshot) = snapshot {
                            self.queue(snapshot, stamp, now)?;
                        }
                    }
                }
                Err(error) if retry(&error) => {}
                Err(error) => return Err(error),
            }
        }
        if ready.contains(PollFlags::POLLOUT) && !self.reply.is_empty() {
            match self.stream.write(&self.reply[self.sent..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => {
                    self.sent += count;
                    if self.sent == self.reply.len() {
                        self.reply.clear();
                        self.deadline = None;
                    }
                }
                Err(error) if retry(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

pub fn serve<T: Snapshot>(
    listener: &UnixListener,
    shutdown: BorrowedFd<'_>,
    changes: BorrowedFd<'_>,
    mut snapshot: Option<T>,
    mut refresh: impl FnMut() -> io::Result<Option<T>>,
    mut demand_changed: impl FnMut(bool),
) -> io::Result<()> {
    let mut clients = Vec::<Subscriber>::new();
    let mut demand = false;
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let mut stamp = Stamp {
        instance: (started.as_nanos() << 32) | u128::from(std::process::id()),
        revision: 1,
    };
    loop {
        let mut descriptors = [
            PollFd::new(shutdown, PollFlags::POLLIN),
            PollFd::new(changes, PollFlags::POLLIN),
            PollFd::new(listener.as_fd(), PollFlags::POLLIN),
        ]
        .into_iter()
        .chain(
            clients
                .iter()
                .map(|client| PollFd::new(client.stream.as_fd(), client.interest())),
        )
        .collect::<Vec<_>>();
        let timeout = clients
            .iter()
            .filter_map(|client| client.deadline)
            .min()
            .map_or(PollTimeout::NONE, |deadline| {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let remaining = if remaining.is_zero() {
                    remaining
                } else {
                    remaining.max(Duration::from_millis(1))
                };
                PollTimeout::try_from(remaining).unwrap_or(PollTimeout::MAX)
            });
        match poll(&mut descriptors, timeout) {
            Ok(_) => {}
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(error.into()),
        }
        let ready: Vec<_> = descriptors
            .iter()
            .map(|fd| fd.revents().unwrap_or_else(PollFlags::empty))
            .collect();
        if !ready[0].is_empty() {
            return Ok(());
        }
        if ready[1..3]
            .iter()
            .any(|flags| flags.intersects(READY_ERRORS))
        {
            return Err(io::Error::other("subscription source or listener lost"));
        }
        let now = Instant::now();
        let mut flags = ready[3..].iter().copied();
        clients.retain_mut(|client| {
            client
                .service(flags.next().unwrap(), snapshot.as_ref(), stamp, now)
                .is_ok()
        });
        let subscribed = clients.iter().any(|client| client.id.is_some());
        if subscribed != demand {
            demand = subscribed;
            if T::ON_DEMAND && !demand {
                snapshot = None;
            }
            demand_changed(demand);
        }
        if ready[1].contains(PollFlags::POLLIN)
            && let Some(updated) = refresh()?
            && (!T::ON_DEMAND || demand)
            && snapshot.as_ref() != Some(&updated)
        {
            stamp.revision += 1;
            let now = Instant::now();
            clients.retain_mut(|client| client.queue(&updated, stamp, now).is_ok());
            snapshot = Some(updated);
        }
        if demand && !clients.iter().any(|client| client.id.is_some()) {
            demand = false;
            if T::ON_DEMAND {
                snapshot = None;
            }
            demand_changed(false);
        }
        if ready[2].contains(PollFlags::POLLIN) {
            for _ in 0..MAX_SUBSCRIBERS {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if clients.len() < MAX_SUBSCRIBERS
                            && getsockopt(&stream, PeerCredentials)
                                .is_ok_and(|peer| peer.uid() == geteuid().as_raw())
                            && let Ok(client) = Subscriber::new(stream, Instant::now())
                        {
                            clients.push(client);
                        }
                    }
                    Err(error) if retry(&error) => break,
                    Err(error) => return Err(error),
                }
            }
        }
    }
}

fn retry(error: &io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted)
}

#[cfg(test)]
mod tests {
    use crate::stream::read_status;

    use super::*;

    #[test]
    fn subscriber_coalesces_unsent_updates_and_limits_incomplete_io() {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        let now = Instant::now();
        let mut client = Subscriber::new(receiver, now).unwrap();
        let mut stamp = Stamp {
            instance: 7,
            revision: 1,
        };
        let status = String::from("version=1\ncontroller=waiting\n");
        let request = Topic::Status.subscribe(11);
        sender.write_all(&request[..4]).unwrap();
        client
            .service(PollFlags::POLLIN, Some(&status), stamp, now)
            .unwrap();
        assert_eq!(client.received, 4);
        assert!(client.id.is_none());
        assert!(
            client
                .service(
                    PollFlags::empty(),
                    Some(&status),
                    stamp,
                    now + CLIENT_TIMEOUT
                )
                .is_err()
        );
        sender.write_all(&request[4..]).unwrap();
        client
            .service(PollFlags::POLLIN, Some(&status), stamp, now)
            .unwrap();
        assert_eq!(client.id, Some(11));

        stamp.revision = 2;
        client.queue(&status, stamp, now).unwrap();
        assert_eq!(
            read_status(&mut client.reply.as_slice(), 11).unwrap(),
            (stamp, status.clone())
        );
        client.sent = 1;
        assert!(client.queue(&status, stamp, now).is_err());
    }
}
