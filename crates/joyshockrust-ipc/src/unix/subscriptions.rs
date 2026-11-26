//! Unix desktop subscription client and optional bridge transport.

use std::{
    io::{self, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use crate::{
    inventory::{Controller, Snapshot},
    stream::{Stamp, Topic},
};

#[cfg(feature = "server")]
pub mod publisher;
#[cfg(feature = "server")]
pub mod server;

const RETRY: Duration = Duration::from_secs(1);
const REQUEST_ID: u32 = 1;

struct State<T> {
    stream: Option<UnixStream>,
    latest: Option<T>,
    notified: bool,
    closed: bool,
}

pub struct Subscription<T>(Arc<Mutex<State<T>>>);

type Decoder<T> = fn(&mut UnixStream, u32) -> io::Result<(Stamp, Option<T>)>;

pub fn read_inventory(
    stream: &mut UnixStream,
    id: u32,
) -> io::Result<(Stamp, Option<Vec<Controller>>)> {
    let Snapshot {
        instance,
        revision,
        controllers,
    } = Snapshot::read(stream, id)?;
    Ok((Stamp { instance, revision }, controllers))
}

pub fn read_preview(
    stream: &mut UnixStream,
    id: u32,
) -> io::Result<(Stamp, Option<crate::telemetry::preview::Preview>)> {
    crate::telemetry::preview::Preview::read(stream, id)
        .map(|(stamp, preview)| (stamp, Some(preview)))
}

impl<T: Clone + PartialEq + Send + 'static> Subscription<T> {
    pub fn start(
        status_socket: &Path,
        topic: Topic,
        read: Decoder<T>,
        notify: impl Fn() + Send + 'static,
    ) -> io::Result<Self> {
        let state = Arc::new(Mutex::new(State {
            stream: None,
            latest: None,
            notified: false,
            closed: false,
        }));
        let worker = Arc::clone(&state);
        let path = topic.socket_path(status_socket);
        thread::Builder::new()
            .name("jsr-subscription".into())
            .spawn(move || watch(worker, path, topic, read, notify))?;
        Ok(Self(state))
    }

    pub fn latest(&self) -> Option<T> {
        let mut state = self.0.lock().unwrap();
        state.notified = false;
        state.latest.clone()
    }
}

impl<T> Drop for Subscription<T> {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap();
        state.closed = true;
        if let Some(stream) = state.stream.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

fn watch<T: PartialEq>(
    state: Arc<Mutex<State<T>>>,
    path: PathBuf,
    topic: Topic,
    read: Decoder<T>,
    notify: impl Fn(),
) {
    while !state.lock().unwrap().closed {
        let _ = follow(&path, &state, topic, read, |latest| {
            publish(&state, latest, &notify)
        });
        state.lock().unwrap().stream = None;
        publish(&state, None, &notify);
        thread::sleep(RETRY);
    }
}

fn follow<T>(
    path: &Path,
    state: &Mutex<State<T>>,
    topic: Topic,
    read: Decoder<T>,
    mut update: impl FnMut(Option<T>),
) -> io::Result<()> {
    let mut stream = UnixStream::connect(path)?;
    {
        let mut state = state.lock().unwrap();
        if state.closed {
            return Ok(());
        }
        state.stream = Some(stream.try_clone()?);
    }
    stream.set_read_timeout(Some(RETRY))?;
    stream.set_write_timeout(Some(RETRY))?;
    stream.write_all(&topic.subscribe(REQUEST_ID))?;
    let (initial, data) = read(&mut stream, REQUEST_ID)?;
    stream.set_read_timeout(None)?;
    let mut revision = initial.revision;
    update(data);
    loop {
        let (next, data) = read(&mut stream, REQUEST_ID)?;
        if next.instance != initial.instance || next.revision <= revision {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid subscription revision",
            ));
        }
        revision = next.revision;
        update(data);
    }
}

fn publish<T: PartialEq>(state: &Mutex<State<T>>, latest: Option<T>, notify: &impl Fn()) {
    let mut state = state.lock().unwrap();
    if state.closed || state.latest == latest {
        return;
    }
    state.latest = latest;
    if !state.notified {
        state.notified = true;
        drop(state);
        notify();
    }
}

#[cfg(test)]
mod tests {
    use std::{os::unix::net::UnixListener, sync::mpsc, time::Instant};

    use crate::{inventory::Access, stream::HEADER_LEN};
    use joyshockrust_config::controller::{ModelSelection, Selection};

    use super::*;
    use std::io::Read;

    #[test]
    fn client_clears_stale_inventory_and_subscribes_to_a_restarted_bridge() {
        let directory = tempfile::tempdir().unwrap();
        let status = directory.path().join("status.sock");
        let path = Topic::Inventory.socket_path(&status);
        let listener = UnixListener::bind(&path).unwrap();
        let (updated, messages) = mpsc::channel();
        let subscription =
            Subscription::start(&status, Topic::Inventory, read_inventory, move || {
                let _ = updated.send(());
            })
            .unwrap();

        let controller = Controller {
            hidraw: 6,
            model: ModelSelection::DualSenseEdge,
            transport: Selection::Usb,
            access: Access::Unreadable,
        };
        let server = |listener: &UnixListener, instance, controllers| {
            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + RETRY * 5;
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(client) => break client,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "client did not subscribe");
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_read_timeout(Some(RETRY)).unwrap();
            let mut request = [0; HEADER_LEN];
            stream.read_exact(&mut request).unwrap();
            let id = Topic::Inventory.parse_subscription(&request).unwrap();
            stream
                .write_all(
                    &Snapshot {
                        instance,
                        revision: 1,
                        controllers,
                    }
                    .encode(id)
                    .unwrap(),
                )
                .unwrap();
            stream
        };
        let stream = server(&listener, 1, Some(vec![controller]));
        messages.recv_timeout(RETRY).unwrap();
        assert_eq!(subscription.latest(), Some(vec![controller]));
        drop(stream);
        drop(listener);
        messages.recv_timeout(RETRY).unwrap();
        assert_eq!(subscription.latest(), None);

        std::fs::remove_file(&path).unwrap();
        let listener = UnixListener::bind(&path).unwrap();
        let _stream = server(&listener, 2, Some(Vec::new()));
        messages.recv_timeout(RETRY).unwrap();
        assert_eq!(subscription.latest(), Some(Vec::new()));
    }
}
