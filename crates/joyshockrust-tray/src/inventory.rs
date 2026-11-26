use std::{
    io::{self, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use joyshockrust_ipc::inventory::{self, Controller, Snapshot};

const RETRY: Duration = Duration::from_secs(1);
const REQUEST_ID: u32 = 1;

#[derive(Default)]
struct State {
    stream: Option<UnixStream>,
    controllers: Option<Vec<Controller>>,
    notified: bool,
    closed: bool,
}

pub struct Subscription(Arc<Mutex<State>>);

impl Subscription {
    pub fn start(status_socket: &Path, notify: impl Fn() + Send + 'static) -> io::Result<Self> {
        let state = Arc::new(Mutex::new(State::default()));
        let worker = Arc::clone(&state);
        let path = inventory::socket_path(status_socket);
        thread::Builder::new()
            .name("jsr-tray-inventory".into())
            .spawn(move || watch(worker, path, notify))?;
        Ok(Self(state))
    }

    pub fn controllers(&self) -> Option<Vec<Controller>> {
        let mut state = self.0.lock().unwrap();
        state.notified = false;
        state.controllers.clone()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap();
        state.closed = true;
        if let Some(stream) = state.stream.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

fn watch(state: Arc<Mutex<State>>, path: PathBuf, notify: impl Fn()) {
    while !state.lock().unwrap().closed {
        let _ = follow(&path, &state, |controllers| {
            publish(&state, controllers, &notify)
        });
        state.lock().unwrap().stream = None;
        publish(&state, None, &notify);
        thread::sleep(RETRY);
    }
}

fn follow(
    path: &Path,
    state: &Mutex<State>,
    mut update: impl FnMut(Option<Vec<Controller>>),
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
    stream.write_all(&inventory::subscribe(REQUEST_ID))?;
    let initial = Snapshot::read(&mut stream, REQUEST_ID)?;
    stream.set_read_timeout(None)?;
    let mut revision = initial.revision;
    update(initial.controllers);
    loop {
        let next = Snapshot::read(&mut stream, REQUEST_ID)?;
        if next.instance != initial.instance || next.revision <= revision {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid inventory revision",
            ));
        }
        revision = next.revision;
        update(next.controllers);
    }
}

fn publish(state: &Mutex<State>, controllers: Option<Vec<Controller>>, notify: &impl Fn()) {
    let mut state = state.lock().unwrap();
    if state.closed || state.controllers == controllers {
        return;
    }
    state.controllers = controllers;
    if !state.notified {
        state.notified = true;
        drop(state);
        notify();
    }
}

#[cfg(test)]
mod tests {
    use std::{os::unix::net::UnixListener, sync::mpsc, time::Instant};

    use joyshockrust_config::controller::{ModelSelection, Selection};
    use joyshockrust_ipc::inventory::{Access, REQUEST_LEN, parse_subscription};

    use super::*;
    use std::io::Read;

    #[test]
    fn tray_clears_stale_inventory_and_subscribes_to_a_restarted_bridge() {
        let directory = tempfile::tempdir().unwrap();
        let status = directory.path().join("status.sock");
        let path = inventory::socket_path(&status);
        let listener = UnixListener::bind(&path).unwrap();
        let (updated, messages) = mpsc::channel();
        let subscription = Subscription::start(&status, move || {
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
                        assert!(Instant::now() < deadline, "tray did not subscribe");
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_read_timeout(Some(RETRY)).unwrap();
            let mut request = [0; REQUEST_LEN];
            stream.read_exact(&mut request).unwrap();
            let id = parse_subscription(&request).unwrap();
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
        assert_eq!(subscription.controllers(), Some(vec![controller]));
        drop(stream);
        drop(listener);
        messages.recv_timeout(RETRY).unwrap();
        assert_eq!(subscription.controllers(), None);

        std::fs::remove_file(&path).unwrap();
        let listener = UnixListener::bind(&path).unwrap();
        let _stream = server(&listener, 2, Some(Vec::new()));
        messages.recv_timeout(RETRY).unwrap();
        assert_eq!(subscription.controllers(), Some(Vec::new()));
    }
}
