//! The inventory worker owns udev enumeration and subscriber I/O.

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
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use joyshockrust_input::{DeviceModel, Transport};
use joyshockrust_ipc::{
    inventory::{self, Access, Controller, MAX_CONTROLLERS, REQUEST_LEN, Snapshot},
    status::hidraw_device_name,
};
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
    sys::socket::{getsockopt, sockopt::PeerCredentials},
    unistd::geteuid,
};

use crate::{
    discovery::{self, ControllerFilter, ModelSelection, Selection},
    hidraw,
};

const MAX_SUBSCRIBERS: usize = 8;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);
const READY_ERRORS: PollFlags = PollFlags::POLLERR
    .union(PollFlags::POLLHUP)
    .union(PollFlags::POLLNVAL);
const ALL_CONTROLLERS: ControllerFilter = ControllerFilter {
    transport: Selection::Auto,
    model: ModelSelection::Any,
    controller_id: None,
};

pub struct Worker {
    stop: UnixStream,
    thread: Option<thread::JoinHandle<()>>,
}

impl Worker {
    pub fn start(status_path: &Path) -> io::Result<Self> {
        let path = inventory::socket_path(status_path);
        let listener = UnixListener::bind(&path)?;
        let metadata = fs::symlink_metadata(&path)?;
        let socket = SocketFile {
            path,
            identity: (metadata.dev(), metadata.ino()),
        };
        fs::set_permissions(&socket.path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let (stop, shutdown) = UnixStream::pair()?;
        let thread = thread::Builder::new()
            .name("jsr-inventory".into())
            .spawn(move || {
                let _socket = socket;
                if let Err(error) = watch(&listener, shutdown.as_fd()) {
                    eprintln!("Controller inventory stopped: {error}");
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

fn watch(listener: &UnixListener, shutdown: BorrowedFd<'_>) -> io::Result<()> {
    // Listen before scanning so hotplug during the first scan is observed.
    let monitor = udev::MonitorBuilder::new()?
        .match_subsystem("hidraw")?
        .listen()?;
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let snapshot = Snapshot {
        instance: (started.as_nanos() << 32) | u128::from(std::process::id()),
        revision: 1,
        controllers: scan(),
    };
    serve(listener, shutdown, monitor.as_fd(), snapshot, || {
        for _ in monitor.iter().take(64) {}
        scan()
    })
}

fn scan() -> Option<Vec<Controller>> {
    let mut controllers = Vec::new();
    for node in discovery::controller_nodes(ALL_CONTROLLERS).ok()? {
        let Some(hidraw) = node
            .path
            .to_str()
            .filter(|path| hidraw_device_name(path))
            .and_then(|path| path.strip_prefix("/dev/hidraw")?.parse().ok())
        else {
            continue;
        };
        let model = match node.identity.model {
            DeviceModel::DualSense => ModelSelection::DualSense,
            DeviceModel::DualSenseEdge => ModelSelection::DualSenseEdge,
            DeviceModel::Other { .. } => continue,
        };
        let transport = match node.identity.transport {
            Transport::Usb => Selection::Usb,
            Transport::Bluetooth => Selection::Bluetooth,
            Transport::Other => continue,
        };
        let access = match node.open(ALL_CONTROLLERS) {
            Ok(file) => Access::Readable(hidraw::controller_id(&file).ok().flatten()),
            Err(_) => Access::Unreadable,
        };
        if controllers.len() == MAX_CONTROLLERS {
            return None;
        }
        controllers.push(Controller {
            hidraw,
            model,
            transport,
            access,
        });
    }
    controllers.sort_by_key(|controller| controller.hidraw);
    Some(controllers)
}

struct Subscriber {
    stream: UnixStream,
    request: [u8; REQUEST_LEN],
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
            request: [0; REQUEST_LEN],
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

    fn queue(&mut self, snapshot: &Snapshot, now: Instant) -> io::Result<()> {
        if let Some(id) = self.id {
            if !self.reply.is_empty() && self.sent != 0 {
                return Err(io::Error::other("inventory subscriber fell behind"));
            }
            if self.reply.is_empty() {
                self.deadline = Some(now + CLIENT_TIMEOUT);
            }
            self.reply = snapshot.encode(id)?;
            self.sent = 0;
        }
        Ok(())
    }

    fn service(&mut self, ready: PollFlags, snapshot: &Snapshot, now: Instant) -> io::Result<()> {
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
                    if self.received == REQUEST_LEN {
                        self.id = Some(inventory::parse_subscription(&self.request)?);
                        self.queue(snapshot, now)?;
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

fn serve(
    listener: &UnixListener,
    shutdown: BorrowedFd<'_>,
    changes: BorrowedFd<'_>,
    mut snapshot: Snapshot,
    mut refresh: impl FnMut() -> Option<Vec<Controller>>,
) -> io::Result<()> {
    let mut clients = Vec::<Subscriber>::new();
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
            return Err(io::Error::other("inventory monitor or listener lost"));
        }
        let now = Instant::now();
        let mut flags = ready[3..].iter().copied();
        clients.retain_mut(|client| {
            client
                .service(flags.next().unwrap(), &snapshot, now)
                .is_ok()
        });
        if ready[1].contains(PollFlags::POLLIN) {
            let controllers = refresh();
            if snapshot.controllers != controllers {
                snapshot.controllers = controllers;
                snapshot.revision += 1;
                let now = Instant::now();
                clients.retain_mut(|client| client.queue(&snapshot, now).is_ok());
            }
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
    use std::os::{linux::net::SocketAddrExt, unix::net::SocketAddr};

    use super::*;

    #[test]
    fn subscribers_receive_changes_with_a_partial_client_and_recoverable_scan_error() {
        let address =
            SocketAddr::from_abstract_name(format!("jsr-inventory-test-{}", std::process::id()))
                .unwrap();
        let listener = UnixListener::bind_addr(&address).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (mut signal, changes) = UnixStream::pair().unwrap();
        let (stop, shutdown) = UnixStream::pair().unwrap();
        let connect = || {
            let client = UnixStream::connect_addr(&address).unwrap();
            client.set_read_timeout(Some(CLIENT_TIMEOUT)).unwrap();
            client
        };
        // Keep one request incomplete so a second client has to progress independently.
        let mut slow = connect();
        slow.write_all(&inventory::subscribe(8)[..3]).unwrap();
        let mut client = connect();
        client.write_all(&inventory::subscribe(9)).unwrap();
        let mut rejected = connect();
        let mut invalid = inventory::subscribe(10);
        invalid[4] = u8::MAX;
        rejected.write_all(&invalid).unwrap();

        let unreadable = Controller {
            hidraw: 5,
            model: ModelSelection::DualSenseEdge,
            transport: Selection::Usb,
            access: Access::Unreadable,
        };
        let worker = thread::spawn(move || {
            serve(
                &listener,
                shutdown.as_fd(),
                changes.as_fd(),
                Snapshot {
                    instance: 19,
                    revision: 1,
                    controllers: Some(Vec::new()),
                },
                || {
                    let mut change = [0];
                    (&changes).read_exact(&mut change).unwrap();
                    match change[0] {
                        1 => Some(vec![unreadable]),
                        2 => None,
                        _ => Some(Vec::new()),
                    }
                },
            )
        });

        let mut test = || -> io::Result<()> {
            let first = Snapshot::read(&mut client, 9)?;
            assert_eq!(first.controllers, Some(Vec::new()));
            assert_eq!(first.revision, 1);
            assert_eq!(rejected.read(&mut [0])?, 0);

            for (signal_code, expected) in [
                (1, Some(vec![unreadable])),
                (2, None),
                (3, Some(Vec::new())),
            ] {
                signal.write_all(&[signal_code])?;
                let update = Snapshot::read(&mut client, 9)?;
                assert_eq!(update.controllers, expected);
                assert_eq!(update.instance, first.instance);
                assert_eq!(update.revision, u64::from(signal_code) + 1);
            }
            Ok(())
        };

        let result = test();
        drop(stop);
        worker.join().unwrap().unwrap();
        result.unwrap();
        assert_eq!(client.read(&mut [0]).unwrap(), 0);
    }

    #[test]
    fn subscriber_coalesces_unsent_updates_and_limits_incomplete_io() {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        let now = Instant::now();
        let mut client = Subscriber::new(receiver, now).unwrap();
        let mut snapshot = Snapshot {
            instance: 7,
            revision: 1,
            controllers: None,
        };
        let request = inventory::subscribe(11);
        sender.write_all(&request[..4]).unwrap();
        client.service(PollFlags::POLLIN, &snapshot, now).unwrap();
        assert_eq!(client.received, 4);
        assert!(client.id.is_none());
        assert!(
            client
                .service(PollFlags::empty(), &snapshot, now + CLIENT_TIMEOUT)
                .is_err()
        );
        sender.write_all(&request[4..]).unwrap();
        client.service(PollFlags::POLLIN, &snapshot, now).unwrap();
        assert_eq!(client.id, Some(11));

        snapshot.revision = 2;
        client.queue(&snapshot, now).unwrap();
        assert_eq!(
            Snapshot::read(&mut client.reply.as_slice(), 11).unwrap(),
            snapshot
        );
        client.sent = 1;
        assert!(client.queue(&snapshot, now).is_err());
    }
}
