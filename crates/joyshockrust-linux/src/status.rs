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
    time::{Duration, Instant},
};

use joyshockrust_input::{BatteryReading, BatteryState, DeviceModel, Transport};

use crate::hidraw::Identity;

pub struct StatusSocket {
    listener: UnixListener,
    snapshot: Cell<Snapshot>,
    reports: Cell<ReportStats>,
}

#[derive(Clone, Copy, Default)]
struct Snapshot {
    identity: Option<Identity>,
    gyro_configured: bool,
    battery: Option<BatteryReading>,
}

#[derive(Clone, Copy, Default)]
struct ReportStats {
    decoded: u64,
    rejected: u64,
    baseline: Option<(Instant, u64)>,
}

impl ReportStats {
    fn rate_hz(self, now: Instant) -> Option<f64> {
        let (start, initial_count) = self.baseline?;
        let elapsed = now.duration_since(start);
        (elapsed >= Duration::from_secs(1) && self.decoded > initial_count)
            .then(|| (self.decoded - initial_count) as f64 / elapsed.as_secs_f64())
    }
}

impl StatusSocket {
    pub fn bind(path: &Path) -> io::Result<Self> {
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            snapshot: Cell::default(),
            reports: Cell::default(),
        })
    }

    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.listener.as_fd()
    }

    pub fn update(&self, identity: Option<Identity>, gyro_configured: bool) {
        let mut snapshot = self.snapshot.get();
        if snapshot.identity != identity {
            snapshot = Snapshot::default();
            self.reports.set(ReportStats::default());
        }
        snapshot.identity = identity;
        snapshot.gyro_configured = gyro_configured;
        self.snapshot.set(snapshot);
    }

    pub fn observe_reports(&self, decoded: u64, rejected: u64) {
        let mut reports = self.reports.get();
        reports.decoded += decoded;
        reports.rejected += rejected;
        if decoded > 0 && reports.baseline.is_none() {
            reports.baseline = Some((Instant::now(), reports.decoded));
        }
        self.reports.set(reports);
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
        let reports = self.reports.get();
        let rate = reports
            .rate_hz(Instant::now())
            .map(|rate| format!("{rate:.1}"));
        let rate = rate.as_deref().unwrap_or("none");
        let response = format!(
            "version=1\ncontroller={controller}\ngyro_configured={}\nlast_reload={last_reload}\ntransport={transport}\nmodel={model}\nbattery_state={battery_state}\nbattery_percent={percent}\nreports_decoded={}\nreports_rejected={}\nreport_rate_hz={rate}\n",
            snapshot.gyro_configured, reports.decoded, reports.rejected
        );
        // Bound work per wakeup. Clients receive one snapshot and EOF.
        for _ in 0..8 {
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    if stream.set_nonblocking(true).is_err() {
                        continue;
                    }
                    // Disconnected or slow clients must not interrupt input.
                    let _ = stream.write_all(response.as_bytes());
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

pub(crate) fn socket_path(path: Option<&Path>, socket_name: &str) -> io::Result<PathBuf> {
    if let Some(path) = path {
        return Ok(path.to_path_buf());
    }
    let runtime = env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is unset"))?;
    Ok(PathBuf::from(runtime)
        .join("joyshockrust")
        .join(socket_name))
}

pub(crate) fn connect(path: Option<&Path>, socket_name: &str) -> io::Result<UnixStream> {
    let path = socket_path(path, socket_name)?;
    let stream = UnixStream::connect(&path).map_err(|err| {
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
        fs::File,
        os::{
            fd::OwnedFd,
            linux::net::SocketAddrExt,
            unix::net::{SocketAddr, UnixDatagram},
        },
        process,
    };

    use super::*;

    #[test]
    fn report_stats_follow_the_active_session() {
        let name = format!("joyshockrust-report-stats-test-{}", process::id());
        let address = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let listener = UnixListener::bind_addr(&address).unwrap();
        listener.set_nonblocking(true).unwrap();
        let status = StatusSocket {
            listener,
            snapshot: Cell::default(),
            reports: Cell::default(),
        };
        let identity = Identity {
            model: DeviceModel::DualSenseEdge,
            transport: Transport::Usb,
        };
        status.update(Some(identity), false);

        let (sender, receiver) = UnixDatagram::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        sender.send(&[1; 65]).unwrap();
        let mut frame = [0; 64];
        frame[0] = 1;
        sender.send(&frame).unwrap();
        sender.send(&frame).unwrap();
        let mut input = File::from(OwnedFd::from(receiver));
        let (feedback_sender, feedback) = UnixDatagram::pair().unwrap();
        let mut client = None;
        crate::forward_reports(
            &mut input,
            identity,
            None,
            Some(&status),
            Some(feedback.as_fd()),
            None,
            |event| match event {
                crate::SessionEvent::Report(_) => {
                    // Query on the next wake, after the current batch is counted.
                    if client.is_none() {
                        client = Some(UnixStream::connect_addr(&address)?);
                        feedback_sender.send(b"stop")?;
                    }
                    Ok(std::ops::ControlFlow::Continue(()))
                }
                crate::SessionEvent::Feedback => Ok(std::ops::ControlFlow::Break(())),
                crate::SessionEvent::Reload(_) => unreachable!(),
            },
        )
        .unwrap();
        let mut client = client.unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.ends_with("reports_decoded=2\nreports_rejected=1\nreport_rate_hz=none\n"));
        let reports = status.reports.get();
        let (start, initial_count) = reports.baseline.unwrap();
        assert_eq!(initial_count, 2);
        assert_eq!(reports.rate_hz(start + Duration::from_secs(2)), None);

        status.observe_reports(500, 0);
        status.update(Some(identity), true);
        let reports = status.reports.get();
        assert_eq!(reports.rate_hz(start + Duration::from_millis(999)), None);
        assert_eq!(reports.rate_hz(start + Duration::from_secs(2)), Some(250.0));
        assert_eq!(reports.rejected, 1);

        status.update(None, true);
        status.update(Some(identity), true);
        let reports = status.reports.get();
        assert_eq!(reports.decoded, 0);
        assert_eq!(reports.rejected, 0);
        assert_eq!(reports.baseline, None);
    }

    #[test]
    fn status_is_served_while_input_is_idle() {
        let name = format!("joyshockrust-status-test-{}", process::id());
        let address = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let listener = UnixListener::bind_addr(&address).unwrap();
        listener.set_nonblocking(true).unwrap();
        let status = StatusSocket {
            listener,
            snapshot: Cell::default(),
            reports: Cell::default(),
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
            assert_eq!(
                response
                    .strip_suffix("reports_decoded=0\nreports_rejected=0\nreport_rate_hz=none\n"),
                Some(expected)
            );
        }
    }
}
