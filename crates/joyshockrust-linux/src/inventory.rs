//! The inventory worker owns udev enumeration and subscriber I/O.

use crate::{
    discovery::{self, ControllerFilter, ModelSelection, Selection},
    hidraw,
    subscriptions::{self, Worker},
};
use joyshockrust_input::{DeviceModel, Transport};
use joyshockrust_ipc::{
    inventory::{self, Access, Controller, MAX_CONTROLLERS},
    status::hidraw_device_name,
    stream::{Stamp, Topic},
};
use std::{
    io,
    os::{
        fd::{AsFd, BorrowedFd},
        unix::net::UnixListener,
    },
    path::Path,
};

const ALL_CONTROLLERS: ControllerFilter = ControllerFilter {
    transport: Selection::Auto,
    model: ModelSelection::Any,
    controller_id: None,
};

pub fn start(status_path: &Path) -> io::Result<Worker> {
    Worker::start(
        "inventory",
        Topic::Inventory.socket_path(status_path),
        watch,
    )
}

impl subscriptions::Snapshot for Option<Vec<Controller>> {
    const TOPIC: Topic = Topic::Inventory;

    fn frame(&self, request: u32, stamp: Stamp) -> io::Result<Vec<u8>> {
        inventory::Snapshot {
            instance: stamp.instance,
            revision: stamp.revision,
            controllers: self.clone(),
        }
        .encode(request)
    }
}

fn watch(listener: &UnixListener, shutdown: BorrowedFd<'_>) -> io::Result<()> {
    // Listen before scanning so hotplug during the first scan is observed.
    let monitor = udev::MonitorBuilder::new()?
        .match_subsystem("hidraw")?
        .listen()?;
    subscriptions::serve(
        listener,
        shutdown,
        monitor.as_fd(),
        Some(scan()),
        || {
            for _ in monitor.iter().take(64) {}
            Ok(Some(scan()))
        },
        |_| {},
    )
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

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        os::{
            linux::net::SocketAddrExt,
            unix::net::{SocketAddr, UnixStream},
        },
        thread,
        time::Duration,
    };
    const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);

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
        slow.write_all(&Topic::Inventory.subscribe(8)[..3]).unwrap();
        let mut client = connect();
        client.write_all(&Topic::Inventory.subscribe(9)).unwrap();
        let mut rejected = connect();
        let mut invalid = Topic::Inventory.subscribe(10);
        invalid[4] = u8::MAX;
        rejected.write_all(&invalid).unwrap();

        let unreadable = Controller {
            hidraw: 5,
            model: ModelSelection::DualSenseEdge,
            transport: Selection::Usb,
            access: Access::Unreadable,
        };
        let worker = thread::spawn(move || {
            subscriptions::serve(
                &listener,
                shutdown.as_fd(),
                changes.as_fd(),
                Some(Some(Vec::new())),
                || {
                    let mut change = [0];
                    (&changes).read_exact(&mut change).unwrap();
                    Ok(Some(match change[0] {
                        1 => Some(vec![unreadable]),
                        2 => None,
                        _ => Some(Vec::new()),
                    }))
                },
                |_| {},
            )
        });

        let mut test = || -> io::Result<()> {
            let first = inventory::Snapshot::read(&mut client, 9)?;
            assert_eq!(first.controllers, Some(Vec::new()));
            assert_eq!(first.revision, 1);
            assert_eq!(rejected.read(&mut [0])?, 0);

            for (signal_code, expected) in [
                (1, Some(vec![unreadable])),
                (2, None),
                (3, Some(Vec::new())),
            ] {
                signal.write_all(&[signal_code])?;
                let update = inventory::Snapshot::read(&mut client, 9)?;
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
}
