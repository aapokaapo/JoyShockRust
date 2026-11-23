use std::{
    cell::{Cell, RefCell},
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

use crate::{discovery::ControllerFilter, hidraw::Identity};

pub struct StatusSocket {
    listener: UnixListener,
    snapshot: Cell<Snapshot>,
    reports: Cell<ReportStats>,
    sessions: Cell<SessionCounts>,
    filter: Cell<ControllerFilter>,
    profile_source: Cell<ProfileSource>,
    active_hidraw: RefCell<Option<String>>,
    gamepad_enabled: Cell<bool>,
    gamepad_sysfs: RefCell<Option<PathBuf>>,
    touchpad_grabbed: Cell<bool>,
}

#[derive(Clone, Copy, Default)]
pub enum ProfileSource {
    #[default]
    Defaults,
    CommandLine,
    Startup,
    Loaded,
}

impl ProfileSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Defaults => "defaults",
            Self::CommandLine => "command_line",
            Self::Startup => "startup",
            Self::Loaded => "loaded",
        }
    }
}

#[derive(Clone, Copy, Default)]
struct SessionCounts {
    started: u64,
    ended: u64,
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
    full_batches: u64,
    gamepad_batches: u64,
    mouse_batches: u64,
    keyboard_batches: u64,
    mouse_button_batches: u64,
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
            sessions: Cell::default(),
            filter: Cell::default(),
            profile_source: Cell::default(),
            active_hidraw: RefCell::default(),
            gamepad_enabled: Cell::new(true),
            gamepad_sysfs: RefCell::default(),
            touchpad_grabbed: Cell::default(),
        })
    }

    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.listener.as_fd()
    }

    pub fn set_filter(&self, filter: ControllerFilter) {
        self.filter.set(filter);
    }

    pub fn set_profile_source(&self, source: ProfileSource) {
        self.profile_source.set(source);
    }

    pub fn set_active_hidraw(&self, path: &Path) {
        // Preserve a single key/value line even when a manual device path includes control characters.
        self.active_hidraw
            .replace(Some(path.to_string_lossy().escape_debug().to_string()));
    }

    pub fn set_gamepad_enabled(&self, enabled: bool) {
        self.gamepad_enabled.set(enabled);
    }

    pub fn set_gamepad_device(&self, device: Option<&mut evdev::uinput::VirtualDevice>) {
        self.gamepad_sysfs
            .replace(device.and_then(|device| device.get_syspath().ok()));
    }

    pub fn set_touchpad_grabbed(&self, grabbed: bool) {
        self.touchpad_grabbed.set(grabbed);
    }

    pub fn update(&self, identity: Option<Identity>, gyro_configured: bool) {
        if identity.is_none() {
            self.active_hidraw.take();
            self.gamepad_sysfs.take();
            self.touchpad_grabbed.set(false);
        }
        let mut snapshot = self.snapshot.get();
        if snapshot.identity != identity {
            let mut sessions = self.sessions.get();
            sessions.ended += u64::from(snapshot.identity.is_some());
            sessions.started += u64::from(identity.is_some());
            self.sessions.set(sessions);
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
        reports.full_batches += u64::from(decoded + rejected == crate::REPORT_BATCH_LIMIT);
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

    pub fn observe_outputs(&self, gamepad: bool, mouse: bool) {
        let mut reports = self.reports.get();
        reports.gamepad_batches += u64::from(gamepad);
        reports.mouse_batches += u64::from(mouse);
        self.reports.set(reports);
    }

    pub fn observe_mapping_outputs(&self, keyboard: bool, mouse_buttons: bool) {
        let mut reports = self.reports.get();
        reports.keyboard_batches += u64::from(keyboard);
        reports.mouse_button_batches += u64::from(mouse_buttons);
        self.reports.set(reports);
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
        let sessions = self.sessions.get();
        let filter = self.filter.get();
        let selected_transport = filter.transport.as_str();
        let selected_model = filter.model.as_str();
        let profile_source = self.profile_source.get().as_str();
        let active_hidraw = self.active_hidraw.borrow();
        let active_hidraw = active_hidraw.as_deref().unwrap_or("none");
        let gamepad_enabled = self.gamepad_enabled.get();
        let touchpad_grabbed = self.touchpad_grabbed.get();
        let gamepad_sysfs = self.gamepad_sysfs.borrow();
        let gamepad_sysfs = gamepad_sysfs
            .as_deref()
            .map_or(std::borrow::Cow::Borrowed("none"), Path::to_string_lossy);
        let response = format!(
            "version=1\ncontroller={controller}\ngyro_configured={}\nlast_reload={last_reload}\ntransport={transport}\nmodel={model}\nbattery_state={battery_state}\nbattery_percent={percent}\nreports_decoded={}\nreports_rejected={}\nreport_rate_hz={rate}\nreport_full_batches={}\nuinput_gamepad_batches={}\nuinput_mouse_batches={}\ncontroller_sessions_started={}\ncontroller_sessions_ended={}\nselected_transport={selected_transport}\nselected_model={selected_model}\ncontroller_id_configured={}\nuinput_keyboard_batches={}\nuinput_mouse_button_batches={}\nprofile_source={profile_source}\nactive_hidraw={active_hidraw}\nvirtual_gamepad_enabled={gamepad_enabled}\nvirtual_gamepad_sysfs={gamepad_sysfs}\nnative_touchpad_grabbed={touchpad_grabbed}\n",
            snapshot.gyro_configured,
            reports.decoded,
            reports.rejected,
            reports.full_batches,
            reports.gamepad_batches,
            reports.mouse_batches,
            sessions.started,
            sessions.ended,
            filter.controller_id.is_some(),
            reports.keyboard_batches,
            reports.mouse_button_batches,
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
    // Allow a full escaped Linux path in the active_hidraw field.
    io::copy(&mut stream.take(64 * 1024), &mut output)?;
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
            sessions: Cell::default(),
            filter: Cell::default(),
            profile_source: Cell::default(),
            active_hidraw: RefCell::default(),
            gamepad_enabled: Cell::new(true),
            gamepad_sysfs: RefCell::default(),
            touchpad_grabbed: Cell::default(),
        };
        let identity = Identity {
            model: DeviceModel::DualSenseEdge,
            transport: Transport::Usb,
        };
        status.update(Some(identity), false);
        status.set_active_hidraw(Path::new("/dev/hidraw6"));
        status.set_touchpad_grabbed(true);

        let (sender, receiver) = UnixDatagram::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        sender.send(&[1; 65]).unwrap();
        let mut frame = [0; 64];
        frame[0] = 1;
        sender.send(&frame).unwrap();
        sender.send(&frame).unwrap();
        let mut input = File::from(OwnedFd::from(receiver));
        let (feedback_sender, feedback) = UnixDatagram::pair().unwrap();
        let feedback = std::cell::RefCell::new(Some(feedback.into()));
        let mut client = None;
        crate::forward_reports(
            &mut input,
            identity,
            None,
            Some(&status),
            Some(&feedback),
            crate::ReportTimers::default(),
            |event| match event {
                crate::SessionEvent::Report(_) => {
                    // Query on the next wake, after the current batch is counted.
                    if client.is_none() {
                        status.observe_outputs(true, true);
                        status.observe_mapping_outputs(true, true);
                        client = Some(UnixStream::connect_addr(&address)?);
                        feedback_sender.send(b"stop")?;
                    } else {
                        status.observe_outputs(true, false);
                        status.observe_mapping_outputs(false, true);
                    }
                    Ok(std::ops::ControlFlow::Continue(()))
                }
                crate::SessionEvent::Feedback => Ok(std::ops::ControlFlow::Break(())),
                crate::SessionEvent::Reload(_) | crate::SessionEvent::KeyboardDeadline(_) => {
                    unreachable!()
                }
            },
        )
        .unwrap();
        let mut client = client.unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.ends_with(
            "reports_decoded=2\nreports_rejected=1\nreport_rate_hz=none\nreport_full_batches=0\nuinput_gamepad_batches=2\nuinput_mouse_batches=1\ncontroller_sessions_started=1\ncontroller_sessions_ended=0\nselected_transport=usb\nselected_model=any\ncontroller_id_configured=false\nuinput_keyboard_batches=1\nuinput_mouse_button_batches=2\nprofile_source=defaults\nactive_hidraw=/dev/hidraw6\nvirtual_gamepad_enabled=true\nvirtual_gamepad_sysfs=none\nnative_touchpad_grabbed=true\n"
        ));
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
        assert_eq!(reports.gamepad_batches, 2);
        assert_eq!(reports.mouse_batches, 1);
        assert_eq!(reports.keyboard_batches, 1);
        assert_eq!(reports.mouse_button_batches, 2);

        status.update(None, true);
        assert!(status.active_hidraw.borrow().is_none());
        assert!(!status.touchpad_grabbed.get());
        status.update(Some(identity), true);
        let reports = status.reports.get();
        assert_eq!(reports.decoded, 0);
        assert_eq!(reports.rejected, 0);
        assert_eq!(reports.baseline, None);
        assert_eq!(reports.gamepad_batches, 0);
        assert_eq!(reports.mouse_batches, 0);
        assert_eq!(reports.keyboard_batches, 0);
        assert_eq!(reports.mouse_button_batches, 0);
        assert_eq!(status.sessions.get().started, 2);
        assert_eq!(status.sessions.get().ended, 1);
    }

    #[test]
    fn status_queries_are_served_between_full_report_batches() {
        let name = format!("joyshockrust-full-batches-test-{}", process::id());
        let address = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let listener = UnixListener::bind_addr(&address).unwrap();
        listener.set_nonblocking(true).unwrap();
        let status = StatusSocket {
            listener,
            snapshot: Cell::default(),
            reports: Cell::default(),
            sessions: Cell::default(),
            filter: Cell::default(),
            profile_source: Cell::default(),
            active_hidraw: RefCell::default(),
            gamepad_enabled: Cell::new(true),
            gamepad_sysfs: RefCell::default(),
            touchpad_grabbed: Cell::default(),
        };
        let identity = Identity {
            model: DeviceModel::DualSenseEdge,
            transport: Transport::Usb,
        };
        status.update(Some(identity), false);

        const VALID_PER_BATCH: u8 = crate::REPORT_BATCH_LIMIT as u8 - 1;
        const VALID_TOTAL: u8 = VALID_PER_BATCH * 3 + 4;
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        sender.set_nonblocking(true).unwrap();
        receiver.set_nonblocking(true).unwrap();
        let mut frame = [0; 64];
        frame[0] = 1;
        for counter in 0..VALID_TOTAL {
            if counter < VALID_PER_BATCH * 3 && counter.is_multiple_of(VALID_PER_BATCH) {
                sender.send(&[1; 65]).unwrap();
            }
            frame[7] = counter;
            sender.send(&frame).unwrap();
        }
        let mut input = File::from(OwnedFd::from(receiver));
        let mut published = 0;
        let mut clients = Vec::new();
        crate::forward_reports(
            &mut input,
            identity,
            None,
            Some(&status),
            None,
            crate::ReportTimers::default(),
            |event| {
                let crate::SessionEvent::Report(report) = event else {
                    unreachable!()
                };
                assert_eq!(report.counter, published);
                published += 1;
                if published == 1 || published == VALID_PER_BATCH + 1 {
                    clients.push(UnixStream::connect_addr(&address)?);
                }
                Ok(if published == VALID_TOTAL {
                    std::ops::ControlFlow::Break(())
                } else {
                    std::ops::ControlFlow::Continue(())
                })
            },
        )
        .unwrap();
        assert_eq!(published, VALID_TOTAL);
        assert_eq!(clients.len(), 2);
        for (batch, mut client) in (1..=2).zip(clients) {
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            for expected in [
                format!("reports_decoded={}", u64::from(VALID_PER_BATCH) * batch),
                format!("reports_rejected={batch}"),
                format!("report_full_batches={batch}"),
            ] {
                assert!(response.lines().any(|line| line == expected), "{response}");
            }
        }
        let reports = status.reports.get();
        assert_eq!(reports.decoded, u64::from(VALID_TOTAL));
        assert_eq!(reports.rejected, 3);
        assert_eq!(reports.full_batches, 3);
        status.update(None, false);
        assert_eq!(status.reports.get().full_batches, 0);
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
            sessions: Cell::default(),
            filter: Cell::default(),
            profile_source: Cell::default(),
            active_hidraw: RefCell::default(),
            gamepad_enabled: Cell::new(true),
            gamepad_sysfs: RefCell::default(),
            touchpad_grabbed: Cell::default(),
        };
        let (_sender, input) = UnixStream::pair().unwrap();

        for (identity, gyro, battery, expected, started, ended) in [
            (
                None,
                true,
                None,
                "version=1\ncontroller=waiting\ngyro_configured=true\nlast_reload=unavailable\ntransport=none\nmodel=none\nbattery_state=none\nbattery_percent=none\n",
                0,
                0,
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
                1,
                0,
            ),
            (
                Some(Identity {
                    model: DeviceModel::DualSenseEdge,
                    transport: Transport::Usb,
                }),
                true,
                None,
                "version=1\ncontroller=connected\ngyro_configured=true\nlast_reload=unavailable\ntransport=usb\nmodel=dualsense_edge\nbattery_state=none\nbattery_percent=none\n",
                2,
                1,
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
                2,
                1,
            ),
            (
                None,
                true,
                None,
                "version=1\ncontroller=waiting\ngyro_configured=true\nlast_reload=unavailable\ntransport=none\nmodel=none\nbattery_state=none\nbattery_percent=none\n",
                2,
                2,
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
                crate::reload::wait(input.as_fd(), None, Some(&status), None, None).unwrap();
            assert!(ready.is_empty());
            assert!(requested.is_none());
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            assert_eq!(
                response,
                format!(
                    "{expected}reports_decoded=0\nreports_rejected=0\nreport_rate_hz=none\nreport_full_batches=0\nuinput_gamepad_batches=0\nuinput_mouse_batches=0\ncontroller_sessions_started={started}\ncontroller_sessions_ended={ended}\nselected_transport=usb\nselected_model=any\ncontroller_id_configured=false\nuinput_keyboard_batches=0\nuinput_mouse_button_batches=0\nprofile_source=defaults\nactive_hidraw=none\nvirtual_gamepad_enabled=true\nvirtual_gamepad_sysfs=none\nnative_touchpad_grabbed=false\n"
                )
            );
        }
    }
}
