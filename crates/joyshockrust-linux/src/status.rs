use std::{
    cell::Cell,
    env,
    io::{self, Read, Write},
    os::{
        fd::{AsFd, BorrowedFd},
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::Duration,
};

use joyshockrust_input::{DeviceModel, Transport};

use crate::hidraw::Identity;

pub struct StatusSocket {
    listener: UnixListener,
    snapshot: Cell<Snapshot>,
}

#[derive(Clone, Copy, Default)]
struct Snapshot {
    identity: Option<Identity>,
    gyro_configured: bool,
}

impl StatusSocket {
    pub fn bind(path: &Path) -> io::Result<Self> {
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            snapshot: Cell::default(),
        })
    }

    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.listener.as_fd()
    }

    pub fn update(&self, identity: Option<Identity>, gyro_configured: bool) {
        self.snapshot.set(Snapshot {
            identity,
            gyro_configured,
        });
    }

    pub fn respond(&self, last_reload: &str) -> io::Result<()> {
        let snapshot = self.snapshot.get();
        let controller = if snapshot.identity.is_some() {
            "connected"
        } else {
            "waiting"
        };
        let transport = match snapshot.identity.map(|identity| identity.transport) {
            Some(Transport::Usb) => "usb",
            Some(Transport::Bluetooth) => "bluetooth",
            Some(Transport::Other) => "unknown",
            None => "none",
        };
        let model = match snapshot.identity.map(|identity| identity.model) {
            Some(DeviceModel::DualSense) => "dualsense",
            Some(DeviceModel::DualSenseEdge) => "dualsense_edge",
            Some(DeviceModel::Other { .. }) => "unknown",
            None => "none",
        };
        // Bound work per wakeup. Clients receive one snapshot and EOF.
        for _ in 0..8 {
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    if stream.set_nonblocking(true).is_err() {
                        continue;
                    }
                    // Disconnected or slow clients must not interrupt input.
                    let _ = write!(
                        stream,
                        "version=1\ncontroller={controller}\ngyro_configured={}\nlast_reload={last_reload}\ntransport={transport}\nmodel={model}\n",
                        snapshot.gyro_configured
                    );
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted
                    ) => {}
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
}

pub fn query(path: Option<&Path>, mut output: impl Write) -> io::Result<()> {
    let stream = connect(path, "status.sock")?;
    io::copy(&mut stream.take(512), &mut output)?;
    Ok(())
}

pub(crate) fn connect(path: Option<&Path>, socket_name: &str) -> io::Result<UnixStream> {
    let default_path;
    let path = if let Some(path) = path {
        path
    } else {
        default_path = PathBuf::from(
            env::var_os("XDG_RUNTIME_DIR")
                .filter(|dir| !dir.is_empty())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is unset")
                })?,
        )
        .join("joyshockrust")
        .join(socket_name);
        &default_path
    };
    let stream = UnixStream::connect(path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("could not connect to {}: {err}", path.display()),
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use std::{
        os::{linux::net::SocketAddrExt, unix::net::SocketAddr},
        process,
    };

    use super::*;

    #[test]
    fn status_is_served_while_input_is_idle() {
        let name = format!("joyshockrust-status-test-{}", process::id());
        let address = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let listener = UnixListener::bind_addr(&address).unwrap();
        listener.set_nonblocking(true).unwrap();
        let status = StatusSocket {
            listener,
            snapshot: Cell::default(),
        };
        let (_sender, input) = UnixStream::pair().unwrap();

        for (identity, gyro, expected) in [
            (
                None,
                true,
                "version=1\ncontroller=waiting\ngyro_configured=true\nlast_reload=unavailable\ntransport=none\nmodel=none\n",
            ),
            (
                Some(Identity {
                    model: DeviceModel::DualSense,
                    transport: Transport::Bluetooth,
                }),
                false,
                "version=1\ncontroller=connected\ngyro_configured=false\nlast_reload=unavailable\ntransport=bluetooth\nmodel=dualsense\n",
            ),
            (
                Some(Identity {
                    model: DeviceModel::DualSenseEdge,
                    transport: Transport::Usb,
                }),
                true,
                "version=1\ncontroller=connected\ngyro_configured=true\nlast_reload=unavailable\ntransport=usb\nmodel=dualsense_edge\n",
            ),
        ] {
            status.update(identity, gyro);
            let mut client = UnixStream::connect_addr(&address).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let (ready, requested) =
                crate::reload::wait(input.as_fd(), None, Some(&status)).unwrap();
            assert!(ready.is_empty());
            assert!(requested.is_none());
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            assert_eq!(response, expected);
        }
    }
}
