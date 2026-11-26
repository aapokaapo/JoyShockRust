#![cfg(target_os = "linux")]

use std::{
    fs,
    os::unix::net::UnixDatagram,
    path::PathBuf,
    process::{self, Child, Command},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("jsr-{label}-{}-{stamp}", process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Bridge(Child);

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn invalid_startup_profile_exits_with_config_status() {
    for flag in ["--profile", "--profile-if-present"] {
        let output = Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"))
            .args([flag, "/dev/null"])
            .output()
            .expect("run bridge with an empty profile");
        assert_eq!(output.status.code(), Some(78));
        assert!(String::from_utf8_lossy(&output.stderr).contains("missing field `version`"));
    }
}

#[test]
fn controller_inventory_can_run_without_a_service_or_profile() {
    let directory = TestDirectory::new("inventory");
    let output = Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"))
        .env("XDG_CONFIG_HOME", &directory.0)
        .args([
            "--list-controllers",
            "--show-controller-id",
            "--controller-model",
            "dualsense-edge",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.starts_with("Controller filter: DualSense Edge over USB or Bluetooth\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("/dev/uinput:"), "{stdout}");
    assert!(!directory.0.join("joyshockrust/controller.toml").exists());
}

#[test]
fn diagnostics_loads_the_profile_and_allows_a_transport_override() {
    let profile =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/gyro-except-l2.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"))
        .args([
            "--diagnose",
            "--controller-model",
            "any",
            "--bluetooth",
            "--profile",
        ])
        .arg(profile)
        .arg("/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("Controller filter: Bluetooth DualSense or DualSense Edge\n"));
    assert!(stdout.lines().any(|line| line.starts_with("/dev/uinput: ")));

    let invalid = Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"))
        .args(["--diagnose", "--profile", "/dev/null"])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(78));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("missing field `version`"));
}

#[test]
fn installing_a_profile_preserves_source_bytes_and_rejects_invalid_replacements() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let directory = TestDirectory::new("install");
    let source = directory.0.join("preset.toml");
    let destination = directory.0.join("joyshockrust/profile.toml");
    let alternate = directory.0.join("other.toml");
    let install = |destination: Option<&PathBuf>| {
        Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"))
            .env("XDG_CONFIG_HOME", &directory.0)
            .arg("--install-profile")
            .arg(&source)
            .args(destination)
            .output()
            .unwrap()
    };

    let original = "# Preserve this comment\nversion = 1\n[gyro]\n";
    fs::write(&source, original).unwrap();
    assert!(install(None).status.success());
    assert_eq!(fs::read_to_string(&destination).unwrap(), original);
    assert_eq!(
        fs::metadata(&destination).unwrap().permissions().mode() & 0o077,
        0
    );

    fs::write(&source, "version = 2\n").unwrap();
    assert!(!install(None).status.success());
    assert_eq!(fs::read_to_string(&destination).unwrap(), original);

    let replacement = "version = 1\nremap_buttons = [\"cross=circle\"]\n";
    fs::write(&source, replacement).unwrap();
    assert!(install(None).status.success());
    assert_eq!(fs::read_to_string(&destination).unwrap(), replacement);
    assert_eq!(
        fs::read_dir(destination.parent().unwrap()).unwrap().count(),
        1
    );

    symlink(&destination, &alternate).unwrap();
    assert!(!install(Some(&alternate)).status.success());
    assert_eq!(fs::read_link(&alternate).unwrap(), destination);
}

#[test]
fn profile_persistence_does_not_block_status_or_control_and_preserves_request_order() {
    use joyshockrust_ipc::control::{
        ControlCommand, OutputTarget, Reply, load_socket_path, profile_path_payload,
    };
    use std::{io::Read, os::unix::net::UnixStream, thread, time::Instant};

    let directory = TestDirectory::new("profile-lock");
    let startup = directory.0.join("profile.toml");
    let selected = directory.0.join("selected.toml");
    let remembered = directory.0.join("profile.toml.active");
    let status = directory.0.join("status");
    let reload = directory.0.join("reload");
    let notify = directory.0.join("notify");
    let listener = UnixDatagram::bind(&notify).unwrap();
    listener
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let _bridge = Bridge(
        Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"))
            .env("NOTIFY_SOCKET", &notify)
            .env("XDG_CONFIG_HOME", &directory.0)
            .arg("--profile-if-present")
            .arg(&startup)
            .args(["--controller-id", "ff:ff:ff:ff:ff:ff", "--status-socket"])
            .arg(&status)
            .arg("--reload-socket")
            .arg(&reload)
            .spawn()
            .unwrap(),
    );
    let mut ready = [0; 64];
    let len = listener.recv(&mut ready).expect("bridge readiness");
    assert_eq!(&ready[..len], b"READY=1");

    let connect = |name| {
        let client = UnixDatagram::bind(directory.0.join(name)).unwrap();
        client.connect(load_socket_path(&reload)).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        client
    };
    let profiles = connect("profiles");
    let control = connect("control");
    let lock = fs::File::open(&directory.0).unwrap();
    lock.lock().unwrap();
    profiles
        .send(&profile_path_payload(&selected, b"version = 1\n[gyro]\n").unwrap())
        .unwrap();

    // The atomic save creates its temporary file before taking the directory lock.
    let deadline = Instant::now() + Duration::from_secs(3);
    while !fs::read_dir(&directory.0).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .as_encoded_bytes()
            .starts_with(b".joyshockrust-profile-")
    }) {
        assert!(
            Instant::now() < deadline,
            "profile persistence did not begin"
        );
        thread::sleep(Duration::from_millis(5));
    }

    profiles
        .send(&ControlCommand::UnloadProfile.encode())
        .unwrap();
    control
        .send(
            &ControlCommand::Emulation {
                target: OutputTarget::Gamepad,
                enabled: false,
            }
            .encode(),
        )
        .unwrap();
    let assert_reply = |client: &UnixDatagram, expected: Reply| {
        let mut reply = [0; 32];
        let len = client.recv(&mut reply).unwrap();
        assert_eq!(&reply[..len], expected.bytes());
    };
    assert_reply(&control, Reply::Applied);
    for invalid in [
        &b"\0set-outputs\nmaybe"[..],
        b"\0preview-trigger\n",
        b"\0unknown\n",
    ] {
        control.send(invalid).unwrap();
        assert_reply(&control, Reply::Rejected);
    }
    let mut stream = UnixStream::connect(&status).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut snapshot = String::new();
    stream.read_to_string(&mut snapshot).unwrap();
    assert!(
        snapshot.lines().any(|line| line == "profile_source=loaded"),
        "{snapshot}"
    );
    assert!(
        snapshot
            .lines()
            .any(|line| line == "last_reload=not_attempted"),
        "{snapshot}"
    );
    assert!(!remembered.exists());

    profiles
        .send(b"\0load-profile-path\nmissing separator")
        .unwrap();
    lock.unlock().unwrap();
    assert_reply(&profiles, Reply::Applied);
    assert_reply(&profiles, Reply::Applied);
    assert_reply(&profiles, Reply::Rejected);
    assert_eq!(fs::read_to_string(remembered).unwrap(), "defaults\n");
}

#[test]
fn live_profiles_and_controller_selection_change_independently_while_waiting() {
    let directory = TestDirectory::new("selection");
    let controller_file = directory.0.join("joyshockrust/controller.toml");
    fs::create_dir_all(controller_file.parent().unwrap()).unwrap();
    fs::write(
        &controller_file,
        "transport = 'auto'\nmodel = 'dualsense-edge'\n",
    )
    .unwrap();
    let startup = directory.0.join("profile.toml");
    let preset = directory.0.join("gyro preset\nβ.toml");
    let status = directory.0.join("status");
    let reload = directory.0.join("reload");
    let notify = directory.0.join("notify");
    let listener = UnixDatagram::bind(&notify).unwrap();
    listener
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    // The broadcast address cannot be an individual controller's HID ID.
    let settings = |transport, model| {
        format!(
            "version = 1\n[bridge]\ntransport = '{transport}'\nmodel = '{model}'\ncontroller_id = 'ff:ff:ff:ff:ff:ff'\n"
        )
    };
    let binary = env!("CARGO_BIN_EXE_joyshockrust-linux");
    let _bridge = Bridge(
        Command::new(binary)
            .env("NOTIFY_SOCKET", &notify)
            .env("XDG_CONFIG_HOME", &directory.0)
            .arg("--profile-if-present")
            .arg(&startup)
            .args(["--controller-id", "ff:ff:ff:ff:ff:ff"])
            .arg("--status-socket")
            .arg(&status)
            .arg("--reload-socket")
            .arg(&reload)
            .spawn()
            .unwrap(),
    );
    let mut ready = [0; 64];
    let len = listener.recv(&mut ready).expect("bridge readiness");
    assert_eq!(&ready[..len], b"READY=1");

    let check = |transport, model, gyro, source| {
        let output = Command::new(binary)
            .arg("--status")
            .arg(&status)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        for line in [
            "controller=waiting".to_owned(),
            format!("selected_transport={transport}"),
            format!("selected_model={model}"),
            format!("gyro_configured={gyro}"),
            format!("profile_source={source}"),
            format!(
                "profile_name={}",
                match source {
                    "startup" => "profile.toml",
                    "loaded" => r"gyro preset\nβ.toml",
                    _ => "",
                }
            ),
            format!("native_touchpad_grab_requested={}", source == "startup"),
            "native_touchpad_grabbed=false".to_owned(),
            format!(
                "virtual_gamepad_kind={}",
                if source == "loaded" {
                    "xbox-series"
                } else {
                    "generic"
                }
            ),
            "controller_id_configured=true".to_owned(),
        ] {
            assert!(stdout.lines().any(|entry| entry == line), "{stdout}");
        }
        let expected_path = match source {
            "startup" => Some(&startup),
            "loaded" => Some(&preset),
            _ => None,
        };
        let path_field = stdout
            .lines()
            .find_map(|line| line.strip_prefix("profile_path_hex="));
        let encoded = expected_path.map(|path| {
            use std::fmt::Write;
            path.as_os_str()
                .as_encoded_bytes()
                .iter()
                .fold(String::new(), |mut hex, byte| {
                    write!(&mut hex, "{byte:02x}").unwrap();
                    hex
                })
        });
        assert_eq!(path_field, encoded.as_deref());
        assert!(!stdout.contains("ff:ff:ff:ff:ff:ff"), "{stdout}");
    };
    check("auto", "dualsense-edge", false, "defaults");
    let startup_contents = format!(
        "{}[gyro]\n[keyboard]\ngrab_touchpad = true\n",
        settings("usb", "dualsense")
    );
    fs::write(&startup, &startup_contents).unwrap();
    let result = Command::new(binary)
        .arg("--reload-profile")
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    check("auto", "dualsense-edge", true, "startup");

    fs::write(
        &preset,
        format!(
            "{}gamepad = 'xbox-series'\n",
            settings("bluetooth", "dualsense")
        ),
    )
    .unwrap();
    let load = || {
        Command::new(binary)
            .arg("--load-profile")
            .arg(&preset)
            .arg(&reload)
            .output()
            .unwrap()
    };
    let result = load();
    assert!(result.status.success(), "{result:?}");
    check("auto", "dualsense-edge", false, "loaded");
    let invalid = directory.0.join("rejected.toml");
    fs::write(&invalid, "version = 2\n").unwrap();
    let rejected = Command::new(binary)
        .arg("--load-profile")
        .arg(invalid)
        .arg(&reload)
        .output()
        .unwrap();
    assert!(!rejected.status.success(), "{rejected:?}");
    check("auto", "dualsense-edge", false, "loaded");

    let result = Command::new(binary)
        .args([
            "--select-controller",
            "ff:ff:ff:ff:ff:fe",
            "--controller-model",
            "dualsense",
            "--bluetooth",
        ])
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    check("bluetooth", "dualsense", false, "loaded");

    // Reload a file with conflicting startup defaults after the live selection.
    let result = Command::new(binary)
        .arg("--reload-profile")
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    check("bluetooth", "dualsense", true, "startup");
    let saved = fs::read_to_string(controller_file).unwrap();
    assert!(saved.contains("transport = \"bluetooth\""), "{saved}");
    assert!(saved.contains("model = \"dualsense\""), "{saved}");
    let output = Command::new(binary)
        .arg("--status")
        .arg(&status)
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("gyro_configured=true\nlast_reload=applied\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("ff:ff:ff:ff:ff:fe"), "{stdout}");
    for _ in 0..2 {
        let result = Command::new(binary)
            .arg("--unload-profile")
            .arg(&reload)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        check("bluetooth", "dualsense", false, "defaults");
    }
    assert_eq!(fs::read_to_string(&startup).unwrap(), startup_contents);
    let result = Command::new(binary)
        .arg("--reload-profile")
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    check("bluetooth", "dualsense", true, "startup");
}

#[test]
fn controller_can_be_selected_and_a_preset_loaded_without_a_startup_profile() {
    let directory = TestDirectory::new("no-profile");
    let preset = directory.0.join("preset");
    let status = directory.0.join("status");
    let reload = directory.0.join("reload");
    let notify = directory.0.join("notify");
    let listener = UnixDatagram::bind(&notify).unwrap();
    listener
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let binary = env!("CARGO_BIN_EXE_joyshockrust-linux");
    let _bridge = Bridge(
        Command::new(binary)
            .env("NOTIFY_SOCKET", &notify)
            .env("XDG_CONFIG_HOME", &directory.0)
            .args(["--controller-id", "ff:ff:ff:ff:ff:ff", "--stick-curve", "2"])
            .arg("--status-socket")
            .arg(&status)
            .arg("--reload-socket")
            .arg(&reload)
            .spawn()
            .unwrap(),
    );
    let mut ready = [0; 64];
    let len = listener.recv(&mut ready).expect("bridge readiness");
    assert_eq!(&ready[..len], b"READY=1");

    let result = Command::new(binary)
        .args([
            "--select-controller",
            "ff:ff:ff:ff:ff:fe",
            "--controller-model",
            "dualsense",
            "--bluetooth",
        ])
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let check = |gyro, outcome, source, gamepad| {
        let output = Command::new(binary)
            .arg("--status")
            .arg(&status)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        for expected in [
            "selected_model=dualsense".to_owned(),
            "selected_transport=bluetooth".to_owned(),
            format!("gyro_configured={gyro}"),
            format!("last_reload={outcome}"),
            format!("profile_source={source}"),
            format!("virtual_gamepad_enabled={gamepad}"),
        ] {
            assert!(stdout.lines().any(|line| line == expected), "{stdout}");
        }
    };
    check(false, "unavailable", "command_line", true);
    let toggle = |mode| {
        let result = Command::new(binary)
            .args(["--set-gamepad", mode])
            .arg(&reload)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
    };
    toggle("off");
    check(false, "unavailable", "command_line", false);
    fs::write(&preset, "version = 1\n[gyro]\n").unwrap();
    let result = Command::new(binary)
        .arg("--load-profile")
        .arg(&preset)
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    check(true, "applied", "loaded", false);
    for (mode, paused) in [("pause", true), ("pause", true), ("resume", false)] {
        let result = Command::new(binary)
            .args(["--set-outputs", mode])
            .arg(&reload)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        let reply = Command::new(binary)
            .arg("--status")
            .arg(&status)
            .output()
            .unwrap();
        assert!(reply.status.success(), "{reply:?}");
        let reply = String::from_utf8(reply.stdout).unwrap();
        assert!(
            reply
                .lines()
                .any(|line| line == format!("outputs_paused={paused}")),
            "{reply}"
        );
        check(true, "applied", "loaded", false);
    }
    let result = Command::new(binary)
        .arg("--reload-profile")
        .arg(&reload)
        .output()
        .unwrap();
    assert!(!result.status.success(), "{result:?}");
    assert!(String::from_utf8_lossy(&result.stderr).contains("no startup profile configured"));
    check(true, "applied", "loaded", false);
    let invalid = directory.0.join("invalid");
    fs::write(&invalid, "version = 2\n").unwrap();
    let result = Command::new(binary)
        .arg("--load-profile")
        .arg(&invalid)
        .arg(&reload)
        .output()
        .unwrap();
    assert!(!result.status.success(), "{result:?}");
    check(true, "rejected", "loaded", false);
    let result = Command::new(binary)
        .arg("--unload-profile")
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    check(false, "applied", "defaults", false);
    toggle("on");
    check(false, "applied", "defaults", true);
    assert_eq!(fs::read_to_string(preset).unwrap(), "version = 1\n[gyro]\n");
}

#[test]
fn remembered_profile_and_emulation_controls_work_across_service_restarts() {
    let directory = TestDirectory::new("remembered");
    let startup = directory.0.join("profile.toml");
    let preset = directory.0.join("gyro preset.toml");
    let invalid = directory.0.join("invalid.toml");
    let status = directory.0.join("status");
    let reload = directory.0.join("reload");
    let notify = directory.0.join("notify");
    let listener = UnixDatagram::bind(&notify).unwrap();
    listener
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    fs::write(&startup, "version = 1\n[bridge]\nvirtual_gamepad = false\n").unwrap();
    fs::write(&preset, "version = 1\n[gyro]\n").unwrap();
    fs::write(&invalid, "version = 2\n").unwrap();
    let binary = env!("CARGO_BIN_EXE_joyshockrust-linux");
    let start = || {
        for path in [&status, &reload, &directory.0.join("reload.load")] {
            let _ = fs::remove_file(path);
        }
        let bridge = Bridge(
            Command::new(binary)
                .env("NOTIFY_SOCKET", &notify)
                .env("XDG_CONFIG_HOME", &directory.0)
                .arg("--profile-if-present")
                .arg(&startup)
                .args(["--controller-id", "ff:ff:ff:ff:ff:ff"])
                .arg("--status-socket")
                .arg(&status)
                .arg("--reload-socket")
                .arg(&reload)
                .spawn()
                .unwrap(),
        );
        let mut ready = [0; 64];
        let len = listener.recv(&mut ready).expect("bridge readiness");
        assert_eq!(&ready[..len], b"READY=1");
        bridge
    };
    let run = |args: &[&std::ffi::OsStr]| {
        Command::new(binary)
            .args(args)
            .arg(&reload)
            .output()
            .unwrap()
    };
    let check = |expected: &[&str]| {
        let result = Command::new(binary)
            .arg("--status")
            .arg(&status)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        let text = String::from_utf8(result.stdout).unwrap();
        for line in expected {
            assert!(text.lines().any(|actual| actual == *line), "{text}");
        }
    };

    let bridge = start();
    check(&[
        "profile_name=profile.toml",
        "virtual_gamepad_enabled=false",
        "gyro_mouse_enabled=false",
    ]);
    assert!(
        run(&["--set-gyro-mouse".as_ref(), "on".as_ref()])
            .status
            .success()
    );
    check(&["virtual_gamepad_enabled=false", "gyro_mouse_enabled=true"]);
    assert!(
        run(&["--set-emulation-master".as_ref(), "off".as_ref()])
            .status
            .success()
    );
    check(&[
        "emulation_master_enabled=false",
        "virtual_gamepad_enabled=false",
        "gyro_mouse_enabled=false",
        "virtual_gamepad_selected=false",
        "gyro_mouse_selected=true",
    ]);
    assert!(
        !run(&["--set-gamepad".as_ref(), "on".as_ref()])
            .status
            .success()
    );
    assert!(
        !run(&["--set-gyro-mouse".as_ref(), "off".as_ref()])
            .status
            .success()
    );
    assert!(
        run(&["--set-emulation-master".as_ref(), "on".as_ref()])
            .status
            .success()
    );
    check(&[
        "emulation_master_enabled=true",
        "virtual_gamepad_enabled=false",
        "gyro_mouse_enabled=true",
    ]);
    assert!(
        run(&["--set-emulation".as_ref(), "on".as_ref()])
            .status
            .success()
    );
    check(&["virtual_gamepad_enabled=true", "gyro_mouse_enabled=true"]);
    assert!(
        run(&["--set-emulation".as_ref(), "off".as_ref()])
            .status
            .success()
    );
    check(&["virtual_gamepad_enabled=false", "gyro_mouse_enabled=false"]);
    assert!(
        run(&["--load-profile".as_ref(), preset.as_os_str()])
            .status
            .success()
    );
    assert!(
        !run(&["--load-profile".as_ref(), invalid.as_os_str()])
            .status
            .success()
    );
    check(&[
        "profile_name=gyro preset.toml",
        "gyro_configured=true",
        "gyro_mouse_enabled=false",
    ]);
    drop(bridge);

    let bridge = start();
    check(&[
        "profile_name=gyro preset.toml",
        "profile_source=loaded",
        "virtual_gamepad_enabled=false",
        "gyro_mouse_enabled=true",
    ]);
    assert!(run(&["--unload-profile".as_ref()]).status.success());
    drop(bridge);

    let bridge = start();
    check(&[
        "profile_name=",
        "profile_source=defaults",
        "gyro_mouse_enabled=false",
    ]);
    assert!(
        run(&["--load-profile".as_ref(), preset.as_os_str()])
            .status
            .success()
    );
    drop(bridge);

    fs::remove_file(&preset).unwrap();
    let _bridge = start();
    check(&["profile_name=profile.toml", "profile_source=startup"]);
}

#[test]
fn whole_turn_calibration_saves_only_a_valid_measurement() {
    let directory = TestDirectory::new("whole-turn-calibration");
    let profile = directory.0.join("profile.toml");
    let original = "version = 1\n[gyro]\nsensitivity_x = 2.5 # keep this\n";
    fs::write(&profile, original).unwrap();
    let save = |turns| {
        Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"))
            .args(["--calibrate-game", "--save-measurement"])
            .arg(&profile)
            .args(["28800", "2", turns])
            .output()
            .unwrap()
    };
    assert!(!save("0").status.success());
    assert!(!save("1.5").status.success());
    assert_eq!(fs::read_to_string(&profile).unwrap(), original);
    let saved = save("4");
    assert!(saved.status.success(), "{saved:?}");
    assert_eq!(saved.stdout, b"40.0\n");
    let text = fs::read_to_string(&profile).unwrap();
    assert!(text.contains("sensitivity_x = 2.5 # keep this"));
    assert!(text.contains("real_world_calibration = 40.0"));
    assert!(text.contains("in_game_sens = 2.0"));
}

#[test]
fn gyro_calibration_is_saved_separately_for_each_controller() {
    let directory = TestDirectory::new("gyro-calibration");
    let first = "00:11:22:33:44:55";
    let second = "00:11:22:33:44:66";
    let save = |id, x| {
        Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"))
            .env("XDG_CONFIG_HOME", &directory.0)
            .args(["--gyro-calibration", "--save", id, x, "0", "-0.1"])
            .output()
            .unwrap()
    };
    assert!(save(first, "0.25").status.success());
    assert!(save(second, "0.5").status.success());
    assert!(!save(first, "nan").status.success());
    let bias_dir = directory.0.join("joyshockrust/gyro-calibration");
    let first_bias = fs::read_to_string(bias_dir.join(format!("{first}.toml"))).unwrap();
    let second_bias = fs::read_to_string(bias_dir.join(format!("{second}.toml"))).unwrap();
    assert!(first_bias.contains("gyro_bias_dps = [0.25, 0.0, -0.1]"));
    assert!(second_bias.contains("gyro_bias_dps = [0.5, 0.0, -0.1]"));
}

#[test]
fn shared_control_client_preserves_requests_and_reports_rejection() {
    use joyshockrust_ipc::{
        control::{ControlCommand, OutputTarget, Reply, load_socket_path},
        unix::ControlClient,
    };

    let directory = TestDirectory::new("control-client");
    let reload = directory.0.join("reload.sock");
    let server = UnixDatagram::bind(load_socket_path(&reload)).unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let profile = directory.0.join("profile.toml");
    fs::write(&profile, "version = 1\n").unwrap();

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut bytes = [0; 512];
            for (expected, reply) in [
                (&b"\0set-gamepad\noff"[..], Reply::Applied),
                (&b"\0select-controller\nany\nany\nauto"[..], Reply::Rejected),
            ] {
                let (len, sender) = server.recv_from(&mut bytes).unwrap();
                assert_eq!(&bytes[..len], expected);
                assert!(!sender.is_unnamed());
                server.send_to_addr(reply.bytes(), &sender).unwrap();
            }
            let (len, sender) = server.recv_from(&mut bytes).unwrap();
            let view = joyshockrust_ipc::control::InlineProfile::decode(&bytes[..len]).unwrap();
            assert_eq!(view.path, Some(profile.as_path()));
            assert_eq!(view.contents, b"version = 1\n");
            server
                .send_to_addr(Reply::Applied.bytes(), &sender)
                .unwrap();
        });

        let client = ControlClient::new(reload);
        client
            .send(ControlCommand::Emulation {
                target: OutputTarget::Gamepad,
                enabled: false,
            })
            .unwrap();
        let error = client
            .send(ControlCommand::SelectController {
                id: "any",
                model: Some("any"),
                transport: Some("auto"),
            })
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert!(error.to_string().contains("controller selection rejected"));
        client.load_profile(&profile).unwrap();
    });
}

#[test]
fn live_status_stream_matches_legacy_status_and_notifies_on_commands() {
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
    };

    use joyshockrust_ipc::{
        status::Field,
        stream::{Topic, read_status},
    };

    let directory = TestDirectory::new("status-stream");
    let status = directory.0.join("status");
    let reload = directory.0.join("reload");
    let notify = directory.0.join("notify");
    let listener = UnixDatagram::bind(&notify).unwrap();
    let timeout = Some(Duration::from_secs(3));
    listener.set_read_timeout(timeout).unwrap();
    let binary = env!("CARGO_BIN_EXE_joyshockrust-linux");
    let bridge = Bridge(
        Command::new(binary)
            .env("NOTIFY_SOCKET", &notify)
            .env("XDG_CONFIG_HOME", &directory.0)
            .args(["--controller-id", "ff:ff:ff:ff:ff:ff"])
            .arg("--status-socket")
            .arg(&status)
            .arg("--reload-socket")
            .arg(&reload)
            .spawn()
            .unwrap(),
    );
    let mut ready = [0; 64];
    let len = listener.recv(&mut ready).expect("bridge readiness");
    assert_eq!(&ready[..len], b"READY=1");

    let updates = Topic::Status.socket_path(&status);
    let mut incomplete = UnixStream::connect(&updates).unwrap();
    incomplete
        .write_all(&Topic::Status.subscribe(99)[..3])
        .unwrap();
    let mut reader = UnixStream::connect(&updates).unwrap();
    reader.set_read_timeout(timeout).unwrap();
    reader.write_all(&Topic::Status.subscribe(42)).unwrap();
    let legacy = || {
        let reply = Command::new(binary)
            .arg("--status")
            .arg(&status)
            .output()
            .unwrap();
        assert!(reply.status.success());
        String::from_utf8(reply.stdout).unwrap()
    };
    let (first, body) = read_status(&mut reader, 42).unwrap();
    assert_eq!(Field::Controller.value(&body), Some("waiting"));
    assert_eq!(body, legacy());

    let command = Command::new(binary)
        .args(["--set-gamepad", "off"])
        .arg(&reload)
        .output()
        .unwrap();
    assert!(command.status.success(), "{command:?}");
    let (updated, body) = read_status(&mut reader, 42).unwrap();
    assert_eq!(updated.instance, first.instance);
    assert!(updated.revision > first.revision);
    assert_eq!(Field::VirtualGamepadEnabled.value(&body), Some("false"));
    assert_eq!(body, legacy());

    drop(bridge);
    assert_eq!(reader.read(&mut [0]).unwrap(), 0);
}
