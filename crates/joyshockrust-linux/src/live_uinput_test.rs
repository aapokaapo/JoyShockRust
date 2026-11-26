use std::{
    cell::Cell,
    fs::{self, File},
    io::Write,
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use evdev::{Device, KeyCode};
use joyshockrust_input::{DeviceModel, Transport};

use crate::{
    SessionDevice,
    discovery::ControllerFilter,
    hidraw::Identity,
    profile::{BridgeOptions, RuntimeOptions},
    reload, serve_session,
    status::{self, StatusSocket},
};

struct TestDirectory(PathBuf);

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn wait_for<T>(description: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn query(path: &Path) -> String {
    let mut response = Vec::new();
    status::query(Some(path), &mut response).unwrap();
    String::from_utf8(response).unwrap()
}

fn gamepad_path(response: &str) -> PathBuf {
    let path = response
        .lines()
        .find_map(|line| line.strip_prefix("virtual_gamepad_sysfs="))
        .unwrap();
    assert_ne!(path, "none", "{response}");
    PathBuf::from(path)
}

pub(super) fn open_evdev(sysfs: &Path) -> Device {
    wait_for("readable virtual evdev node", || {
        fs::read_dir(sysfs).ok()?.flatten().find_map(|entry| {
            let name = entry.file_name();
            if !name.as_encoded_bytes().starts_with(b"event") {
                return None;
            }
            Device::open(Path::new("/dev/input").join(name)).ok()
        })
    })
}

fn report(sender: &mut UnixStream, cross: bool) {
    let mut frame = [0; 64];
    frame[0] = 1;
    frame[1..=4].fill(128);
    frame[8] = if cross { 0x28 } else { 8 };
    frame[33] = 0x80;
    frame[37] = 0x80;
    sender.write_all(&frame).unwrap();
}

#[test]
#[ignore = "requires writable /dev/uinput and readable virtual evdev nodes"]
fn uinput_live_gamepad_toggle() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("jsr-uinput-{}-{nonce}", std::process::id()));
    fs::create_dir(&path).unwrap();
    let directory = TestDirectory(path);
    let status_path = directory.0.join("status");
    let reload_path = directory.0.join("reload");
    let status = StatusSocket::bind(&status_path).unwrap();
    let mut bridge = BridgeOptions::default();
    let reload = reload::Reload::new(None, Some(&reload_path), bridge, true, None, None).unwrap();
    let outputs_paused = Cell::new(false);
    let (mut sender, receiver) = UnixStream::pair().unwrap();
    receiver.set_nonblocking(true).unwrap();
    let mut input = File::from(OwnedFd::from(receiver));

    thread::scope(|scope| {
        let status_path = &status_path;
        let reload_path = &reload_path;
        let observer = scope.spawn(move || {
            let initial = query(status_path);
            assert!(
                initial
                    .lines()
                    .any(|line| line == "controller_sessions_started=1")
            );
            let first = gamepad_path(&initial);
            let device = open_evdev(&first);
            report(&mut sender, true);
            wait_for("initial Cross press", || {
                device
                    .get_key_state()
                    .ok()?
                    .contains(KeyCode::BTN_SOUTH)
                    .then_some(())
            });

            reload::set_emulation(reload::OutputTarget::Gamepad, false, Some(reload_path)).unwrap();
            let disabled = query(status_path);
            assert!(
                disabled
                    .lines()
                    .any(|line| line == "virtual_gamepad_enabled=false")
            );
            assert!(
                disabled
                    .lines()
                    .any(|line| line == "virtual_gamepad_sysfs=none")
            );
            wait_for("old gamepad removal", || (!first.exists()).then_some(()));
            report(&mut sender, false);
            let state = wait_for("HID report processing while disabled", || {
                let response = query(status_path);
                response
                    .lines()
                    .any(|line| line == "reports_decoded=2")
                    .then_some(response)
            });
            assert!(state.lines().any(|line| line == "uinput_gamepad_batches=1"));

            reload::set_emulation(reload::OutputTarget::Gamepad, true, Some(reload_path)).unwrap();
            let enabled = query(status_path);
            let second = gamepad_path(&enabled);
            assert_ne!(first, second);
            for expected in [
                "virtual_gamepad_enabled=true",
                "controller_sessions_started=1",
                "controller_sessions_ended=0",
            ] {
                assert!(enabled.lines().any(|line| line == expected), "{enabled}");
            }
            let reopened = open_evdev(&second);
            assert!(
                !reopened
                    .get_key_state()
                    .unwrap()
                    .contains(KeyCode::BTN_SOUTH)
            );
            report(&mut sender, true);
            wait_for("Cross press on the recreated device", || {
                reopened
                    .get_key_state()
                    .ok()?
                    .contains(KeyCode::BTN_SOUTH)
                    .then_some(())
            });
            report(&mut sender, false);
            wait_for("Cross release on the recreated device", || {
                (!reopened.get_key_state().ok()?.contains(KeyCode::BTN_SOUTH)).then_some(())
            });
            reload::set_outputs_paused(true, Some(reload_path)).unwrap();
            assert!(!second.exists(), "virtual gamepad survived output pause");
        });

        let selection = serve_session(
            &mut input,
            &mut RuntimeOptions::default(),
            Some(&reload),
            Some(&status),
            SessionDevice {
                identity: Identity {
                    model: DeviceModel::DualSenseEdge,
                    transport: Transport::Usb,
                },
                filter: ControllerFilter::default(),
                path: Path::new("/dev/hidraw-test"),
                grab_touchpad: false,
                outputs_paused: Some(&outputs_paused),
                emulation: &mut crate::Emulation::default(),
            },
            &mut bridge,
            None,
        )
        .unwrap();
        assert!(selection == Some(ControllerFilter::default()));
        assert!(outputs_paused.get());
        observer.join().unwrap();
    });
}
