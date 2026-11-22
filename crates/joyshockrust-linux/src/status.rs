use std::{
    cell::Cell,
    env,
    fmt::Display,
    io::{self, Read, Write},
    os::{
        fd::{AsFd, BorrowedFd},
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::Duration,
};

use joyshockrust_input::{BatteryReading, BatteryState, DeviceModel, Transport};

use crate::hidraw::Identity;

pub struct StatusSocket {
    listener: UnixListener,
    snapshot: Cell<Snapshot>,
}

#[derive(Clone, Copy, Default)]
struct Snapshot {
    identity: Option<Identity>,
    gyro_configured: bool,
    battery: Option<BatteryReading>,
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
        let snapshot = self.snapshot.get();
        self.snapshot.set(Snapshot {
            identity,
            gyro_configured,
            battery: if snapshot.identity == identity {
                snapshot.battery
            } else {
                None
            },
        });
    }

    pub fn observe_battery(&self, battery: BatteryReading) {
        let mut snapshot = self.snapshot.get();
        if snapshot.identity.is_some() && snapshot.battery != Some(battery) {
            snapshot.battery = Some(battery);
            self.snapshot.set(snapshot);
        }
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
        let battery_percent = snapshot.battery.and_then(|reading| reading.percent);
        let percent: &dyn Display = match &battery_percent {
            Some(percent) => percent,
            None => &"none",
        };
        let battery_state = match snapshot.battery.map(|reading| reading.state) {
            None => "none",
            Some(BatteryState::Discharging) => "discharging",
            Some(BatteryState::Charging) => "charging",
            Some(BatteryState::Full) => "full",
            Some(BatteryState::NotCharging) => "not_charging",
            Some(BatteryState::Unknown) => "unknown",
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
                        "version=1\ncontroller={controller}\ngyro_configured={}\nlast_reload={last_reload}\ntransport={transport}\nmodel={model}\nbattery_state={battery_state}\nbattery_percent={percent}\n",
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

        for (identity, gyro, battery, expected) in [
            (
                None,
                true,
                None,
                "version=1\ncontroller=waiting\ngyro_configured=true\nlast_reload=unavailable\ntransport=none\nmodel=none\nbattery_state=none\nbattery_percent=none\n",
            ),
            (
                Some(Identity {
                    model: DeviceModel::DualSense,
                    transport: Transport::Bluetooth,
                }),
                false,
                Some(BatteryReading {
                    percent: Some(65),
                    state: BatteryState::Charging,
                }),
                "version=1\ncontroller=connected\ngyro_configured=false\nlast_reload=unavailable\ntransport=bluetooth\nmodel=dualsense\nbattery_state=charging\nbattery_percent=65\n",
            ),
            (
                Some(Identity {
                    model: DeviceModel::DualSenseEdge,
                    transport: Transport::Usb,
                }),
                true,
                None,
                "version=1\ncontroller=connected\ngyro_configured=true\nlast_reload=unavailable\ntransport=usb\nmodel=dualsense_edge\nbattery_state=none\nbattery_percent=none\n",
            ),
            (
                Some(Identity {
                    model: DeviceModel::DualSenseEdge,
                    transport: Transport::Usb,
                }),
                true,
                Some(BatteryReading {
                    percent: None,
                    state: BatteryState::NotCharging,
                }),
                "version=1\ncontroller=connected\ngyro_configured=true\nlast_reload=unavailable\ntransport=usb\nmodel=dualsense_edge\nbattery_state=not_charging\nbattery_percent=none\n",
            ),
            (
                None,
                true,
                None,
                "version=1\ncontroller=waiting\ngyro_configured=true\nlast_reload=unavailable\ntransport=none\nmodel=none\nbattery_state=none\nbattery_percent=none\n",
            ),
        ] {
            status.update(identity, !gyro);
            if let Some(reading) = battery {
                status.observe_battery(reading);
            }
            status.update(identity, gyro);
            let mut client = UnixStream::connect_addr(&address).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let (ready, requested, _) =
                crate::reload::wait(input.as_fd(), None, Some(&status), None).unwrap();
            assert!(ready.is_empty());
            assert!(requested.is_none());
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            assert_eq!(response, expected);
        }
    }
}
